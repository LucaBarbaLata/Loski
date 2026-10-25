//! Joining voice, following you between channels, and leaving when you go.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::Result;
use parking_lot::Mutex;
use songbird::{
    CoreEvent, Event, EventContext, EventHandler, TrackEvent,
    driver::Bitrate,
    events::context_data::DisconnectReason,
    input::{Input, RawAdapter},
    tracks::PlayMode,
};
use tokio::{sync::Notify, task::JoinHandle};
use twilight_model::{
    guild::PremiumTier,
    id::{
        Id,
        marker::{ChannelMarker, GuildMarker, UserMarker},
    },
    voice::VoiceState as DiscordVoiceState,
};

use super::{App, Core, SHORT_SECS, cards, oops, panel, recap};
use crate::{
    audio::{CHANNELS, MixReader, SAMPLE_RATE},
    config::AudioBitrate,
    personality,
};

#[derive(Default)]
struct State {
    guild: Option<Id<GuildMarker>>,
    channel: Option<Id<ChannelMarker>>,
    leave_timer: Option<JoinHandle<()>>,
    idle_timer: Option<JoinHandle<()>>,
}

#[derive(Default)]
pub struct Voice {
    state: Mutex<State>,
    /// Woken when the voice connection drops or the audio stream dies.
    heal: Arc<Notify>,
    dropped: Arc<AtomicBool>,
}

impl Voice {
    pub fn guild(&self) -> Option<Id<GuildMarker>> {
        self.state.lock().guild
    }

    pub fn channel(&self) -> Option<Id<ChannelMarker>> {
        self.state.lock().channel
    }

    fn cancel_leave(&self) {
        if let Some(timer) = self.state.lock().leave_timer.take() {
            timer.abort();
        }
    }

    pub fn cancel_idle(&self) {
        if let Some(timer) = self.state.lock().idle_timer.take() {
            timer.abort();
        }
    }
}

/// The voice channel someone is sitting in, from the gateway cache.
pub fn user_channel(
    app: &App,
    guild: Id<GuildMarker>,
    user: Id<UserMarker>,
) -> Option<Id<ChannelMarker>> {
    app.cache.voice_state(user, guild).map(|v| v.channel_id())
}

/// The best quality this server allows, unless AUDIO_BITRATE says otherwise.
fn bitrate(app: &App, guild: Id<GuildMarker>) -> Bitrate {
    match app.cfg.bitrate {
        AudioBitrate::Fixed(bits) => Bitrate::Bits(bits),
        AudioBitrate::Auto => {
            let tier = app
                .cache
                .guild(guild)
                .map_or(PremiumTier::None, |g| g.premium_tier());
            Bitrate::Bits(match tier {
                PremiumTier::Tier1 => 128_000,
                PremiumTier::Tier2 => 256_000,
                PremiumTier::Tier3 => 384_000,
                _ => 96_000,
            })
        }
    }
}

/// The bitrate Loski sends in this server, for /status.
pub fn bitrate_kbps(app: &App, guild: Id<GuildMarker>) -> u32 {
    match bitrate(app, guild) {
        Bitrate::Bits(bits) => (bits / 1000) as u32,
        _ => 0,
    }
}

/// The endless stream songbird plays: the engine's mixer as raw f32 PCM.
fn output_input(core: &Core) -> Input {
    let source = RawAdapter::new(
        MixReader::new(core.engine.mixer()),
        SAMPLE_RATE,
        CHANNELS as u32,
    );
    Input::from(source)
}

/// songbird drops a track that fails quietly; log it and get the heal loop
/// to start a new one.
struct OutputErrorLogger {
    heal: Arc<Notify>,
}

#[async_trait::async_trait]
impl EventHandler for OutputErrorLogger {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(tracks) = ctx {
            for (state, _) in *tracks {
                if let PlayMode::Errored(e) = &state.playing {
                    tracing::error!("Discord audio output stopped: {e}");
                    self.heal.notify_one();
                }
            }
        }
        None
    }
}

/// songbird retries a broken voice connection a few times by itself; this
/// notices when it gives up, so the heal loop can join again.
struct DropWatch {
    heal: Arc<Notify>,
    dropped: Arc<AtomicBool>,
}

#[async_trait::async_trait]
impl EventHandler for DropWatch {
    async fn act(&self, ctx: &EventContext<'_>) -> Option<Event> {
        if let EventContext::DriverDisconnect(data) = ctx
            // No reason means we asked to leave or move.
            && let Some(reason) = &data.reason
            && !matches!(reason, DisconnectReason::Requested)
        {
            tracing::warn!("voice connection dropped ({reason:?})");
            self.dropped.store(true, Ordering::Relaxed);
            self.heal.notify_one();
        }
        None
    }
}

pub async fn join(
    app: &App,
    core: &Core,
    guild: Id<GuildMarker>,
    channel: Id<ChannelMarker>,
) -> Result<()> {
    app.voice.cancel_leave();
    let connected = app.songbird.get(guild).is_some();
    if connected && app.voice.channel() == Some(channel) && core.engine.output().is_some() {
        return Ok(());
    }
    connect(app, core, guild, channel).await
}

/// Joins (or rejoins) the channel and makes sure Loski's audio is playing into it.
async fn connect(
    app: &App,
    core: &Core,
    guild: Id<GuildMarker>,
    channel: Id<ChannelMarker>,
) -> Result<()> {
    let call = app
        .songbird
        .join(guild, channel)
        .await
        .map_err(|e| oops(format!("I couldn't join your voice channel: {e}")))?;
    let mut call = call.lock().await;
    if !call.is_deaf() {
        let _ = call.deafen(true).await;
    }
    call.set_bitrate(bitrate(app, guild));
    // One watcher per call, however often Loski moves channels.
    call.remove_all_global_events();
    call.add_global_event(
        Event::Core(CoreEvent::DriverDisconnect),
        DropWatch {
            heal: app.voice.heal.clone(),
            dropped: app.voice.dropped.clone(),
        },
    );

    let alive = match core.engine.output() {
        Some(handle) => handle
            .get_info()
            .await
            .is_ok_and(|state| !state.playing.is_done()),
        None => false,
    };
    if !alive {
        let handle = call.play_input(output_input(core));
        let _ = handle.add_event(
            Event::Track(TrackEvent::Error),
            OutputErrorLogger {
                heal: app.voice.heal.clone(),
            },
        );
        core.engine.attach_output(handle);
    }

    let mut st = app.voice.state.lock();
    st.guild = Some(guild);
    st.channel = Some(channel);
    Ok(())
}

/// Whether the voice connection and audio stream are as they should be.
/// True when Loski isn't meant to be in voice at all.
async fn voice_ok(app: &App, core: &Core) -> bool {
    let Some(guild) = app.voice.guild() else {
        return true;
    };
    if app.voice.dropped.swap(false, Ordering::Relaxed) || app.songbird.get(guild).is_none() {
        return false;
    }
    match core.engine.output() {
        Some(handle) => handle
            .get_info()
            .await
            .is_ok_and(|state| !state.playing.is_done()),
        None => false,
    }
}

/// Puts voice back together when it falls apart: after songbird gives up
/// on a dropped connection, or if the audio stream stops. Checks every 30
/// seconds as well, and tells you if it can't get back in.
pub async fn heal_loop(app: Arc<App>) {
    const WAITS: [u64; 5] = [2, 5, 10, 20, 40];
    loop {
        tokio::select! {
            () = app.voice.heal.notified() => {}
            () = tokio::time::sleep(Duration::from_secs(30)) => {}
        }
        let Some(core) = app.core.get() else { continue };
        if voice_ok(&app, core).await {
            continue;
        }
        for (attempt, wait) in WAITS.into_iter().enumerate() {
            tokio::time::sleep(Duration::from_secs(wait)).await;
            // You may have made Loski leave in the meantime.
            let (Some(guild), Some(channel)) = (app.voice.guild(), app.voice.channel()) else {
                break;
            };
            match connect(&app, core, guild, channel).await {
                Ok(()) => {
                    tracing::info!("Back in voice after a dropout");
                    app.all_clear("voice", "I'm back in voice.").await;
                    break;
                }
                Err(e) if attempt + 1 == WAITS.len() => {
                    app.alert(
                        "voice",
                        &format!("I dropped out of voice and couldn't get back in ({e}). `/play` brings me back."),
                    )
                    .await;
                }
                Err(e) => tracing::warn!("rejoining voice failed: {e:#}"),
            }
        }
    }
}

/// Leaves voice, stops playback and clears the queue.
pub async fn leave(app: &Arc<App>, farewell: Option<&str>) {
    let guild = {
        let mut st = app.voice.state.lock();
        for timer in [st.leave_timer.take(), st.idle_timer.take()]
            .into_iter()
            .flatten()
        {
            timer.abort();
        }
        st.channel = None;
        st.guild.take()
    };
    if let Some(core) = app.core.get() {
        core.engine.detach_output();
    }
    if let Some(guild) = guild {
        let _ = app.songbird.remove(guild).await;
    }
    app.set_listening(None);
    // The panel becomes the session's recap; with nothing to sum up it just goes.
    let played = app
        .core
        .get()
        .map(|core| core.engine.take_played())
        .unwrap_or_default();
    if recap::show(app, &played, farewell).await {
        return;
    }
    panel::delete(app).await;
    if let (Some(text), Some(home)) = (farewell, app.ui.home()) {
        app.announce(home, cards::info(&app.icons, text), SHORT_SECS)
            .await;
    }
}

fn spawn_timer(
    app: &Arc<App>,
    after: Duration,
    farewell: &'static str,
    slot: fn(&mut State) -> &mut Option<JoinHandle<()>>,
) {
    let task_app = app.clone();
    let handle = tokio::spawn(async move {
        tokio::time::sleep(after).await;
        // Forget our own handle first so `leave` doesn't abort this task.
        drop(slot(&mut task_app.voice.state.lock()).take());
        leave(&task_app, Some(farewell)).await;
    });
    if let Some(old) = slot(&mut app.voice.state.lock()).replace(handle) {
        old.abort();
    }
}

pub fn schedule_idle_leave(app: &Arc<App>) {
    if app.voice.guild().is_none() {
        return;
    }
    spawn_timer(app, app.cfg.idle_leave, personality::leaving_idle(), |st| {
        &mut st.idle_timer
    });
}

fn schedule_owner_gone_leave(app: &Arc<App>) {
    if app.voice.state.lock().leave_timer.is_some() {
        return;
    }
    spawn_timer(
        app,
        app.cfg.leave_after,
        personality::leaving_alone(),
        |st| &mut st.leave_timer,
    );
}

pub async fn on_voice_state(app: &Arc<App>, state: &DiscordVoiceState) {
    let Some(our_guild) = app.voice.guild() else {
        return;
    };
    if state.guild_id != Some(our_guild) {
        return;
    }

    if state.user_id == app.bot_id {
        match state.channel_id {
            // Someone disconnected us.
            None => leave(app, None).await,
            Some(channel) => app.voice.state.lock().channel = Some(channel),
        }
        return;
    }

    if state.user_id != app.owner_id() {
        return;
    }
    match state.channel_id {
        Some(channel) => {
            app.voice.cancel_leave();
            if app.voice.channel() != Some(channel)
                && let Some(core) = app.core.get()
                && let Err(e) = join(app, core, our_guild, channel).await
            {
                tracing::warn!("couldn't follow you to the new channel: {e:#}");
            }
        }
        None => schedule_owner_gone_leave(app),
    }
}

/// Where you are in voice, in any server Loski shares with you.
pub fn owner_anywhere(app: &App) -> Option<(Id<GuildMarker>, Id<ChannelMarker>)> {
    let owner = app.owner_id();
    if let Some(guild) = app.voice.guild()
        && let Some(channel) = user_channel(app, guild, owner)
    {
        return Some((guild, channel));
    }
    app.cache
        .iter()
        .guilds()
        .map(|g| *g.key())
        .find_map(|guild| user_channel(app, guild, owner).map(|channel| (guild, channel)))
}

/// You picked Loski in the Spotify app: join your voice channel, or pause
/// and say why if you're not in one.
pub async fn join_for_connect(app: &Arc<App>, core: &Core) {
    app.voice.cancel_idle();
    let Some((guild, channel)) = owner_anywhere(app) else {
        core.engine.connect_pause();
        app.dm_owner(cards::info(
            &app.icons,
            "You picked me in Spotify, but you're not in a voice channel. Join one, then press play in Spotify.",
        ))
        .await;
        return;
    };
    if app.ui.home().is_none() {
        // The voice channel's own text chat.
        app.ui.set_home(channel);
    }
    if let Err(e) = join(app, core, guild, channel).await {
        tracing::warn!("couldn't join voice for Spotify Connect: {e:#}");
        core.engine.connect_pause();
    }
}

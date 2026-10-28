//! Joining voice, following you between channels, and leaving when you go,
//! or staying put and keeping the music going with 24/7.

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
    Call, CoreEvent, Event, EventContext, EventHandler, TrackEvent,
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

use super::{App, Core, SHORT_SECS, cards, friends, hearing, library, lyrics, oops, panel, recap};
use crate::{
    audio::{CHANNELS, MixReader, SAMPLE_RATE},
    config::AudioBitrate,
    engine::Status,
    personality,
    track::TrackInfo,
    util::truncate,
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
    /// Discord refused the channel status (a missing permission); don't keep asking.
    status_denied: AtomicBool,
    /// 24/7 is fetching a radio, so voice updates don't start a second one.
    starting_radio: AtomicBool,
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

/// The best quality this server allows, unless /settings or AUDIO_BITRATE
/// says otherwise.
fn bitrate(app: &App, guild: Id<GuildMarker>) -> Bitrate {
    match app.bitrate() {
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

/// The quality changed in /settings: the call picks it up straight away.
pub async fn apply_bitrate(app: &App) {
    let Some(guild) = app.voice.guild() else {
        return;
    };
    if let Some(call) = app.songbird.get(guild) {
        call.lock().await.set_bitrate(bitrate(app, guild));
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
    // Following you to another channel: the old one stops showing the song.
    let previous = app.voice.channel().filter(|c| *c != channel);
    if let Some(previous) = previous {
        set_status(app, previous, "").await;
    }
    connect(app, core, guild, channel).await?;
    if previous.is_some() {
        show_song(app, core.engine.current_track().as_deref()).await;
    }
    Ok(())
}

/// The song in the voice channel's status, the line under its name in the
/// channel list, so people see what's on before they join. `None` clears it.
pub async fn show_song(app: &App, track: Option<&TrackInfo>) {
    let Some(channel) = app.voice.channel() else {
        return;
    };
    let status = track
        .map(|t| truncate(&format!("♪ {}", t.display()), 500))
        .unwrap_or_default();
    set_status(app, channel, &status).await;
}

async fn set_status(app: &App, channel: Id<ChannelMarker>, status: &str) {
    if app.voice.status_denied.load(Ordering::Relaxed) {
        return;
    }
    // twilight has no call for this yet, so it goes to Discord directly.
    let sent = app
        .web
        .put(format!(
            "https://discord.com/api/v10/channels/{channel}/voice-status"
        ))
        .header("Authorization", format!("Bot {}", app.cfg.discord_token))
        .json(&serde_json::json!({ "status": status }))
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    match sent {
        Ok(response) if response.status().is_success() => {}
        Ok(response) if response.status() == reqwest::StatusCode::FORBIDDEN => {
            if !app.voice.status_denied.swap(true, Ordering::Relaxed) {
                tracing::warn!(
                    "Loski can't show the song in the voice channel's status. Give its role the \"Set Voice Channel Status\" permission to turn that on."
                );
            }
        }
        Ok(response) => tracing::debug!("voice channel status: Discord said {}", response.status()),
        Err(e) => tracing::debug!("voice channel status: {e}"),
    }
}

/// The call's events, one set per call however often Loski moves channels:
/// a watcher for dropped connections, and ears (see `hearing.rs`).
fn set_up_events(app: &App, call: &mut Call) {
    call.remove_all_global_events();
    call.add_global_event(
        Event::Core(CoreEvent::DriverDisconnect),
        DropWatch {
            heal: app.voice.heal.clone(),
            dropped: app.voice.dropped.clone(),
        },
    );
    hearing::add_events(call, &app.ears);
}

/// Voice commands went on or off: the call Loski is in follows.
pub async fn hearing_changed(app: &App) {
    let Some(call) = app.voice.guild().and_then(|guild| app.songbird.get(guild)) else {
        return;
    };
    let mut call = call.lock().await;
    set_up_events(app, &mut call);
    hearing::set_deaf(&mut call, &app.ears).await;
}

/// Joins (or rejoins) the channel and makes sure Loski's audio is playing into it.
async fn connect(
    app: &App,
    core: &Core,
    guild: Id<GuildMarker>,
    channel: Id<ChannelMarker>,
) -> Result<()> {
    // Discord says whose voice is whose as the call connects: the events
    // go in first, so none of that is missed.
    set_up_events(app, &mut *app.songbird.get_or_insert(guild).lock().await);
    let call = app
        .songbird
        .join(guild, channel)
        .await
        .map_err(|e| oops(format!("I couldn't join your voice channel: {e}")))?;
    let mut call = call.lock().await;
    call.set_bitrate(bitrate(app, guild));
    hearing::set_deaf(&mut call, &app.ears).await;

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

    {
        let mut st = app.voice.state.lock();
        st.guild = Some(guild);
        st.channel = Some(channel);
    }
    remember_channel(app);
    friends::people_changed(app);
    Ok(())
}

/// 24/7 notes where Loski is, to come back there after a restart.
fn remember_channel(app: &App) {
    let (Some(guild), Some(channel)) = (app.voice.guild(), app.voice.channel()) else {
        return;
    };
    let here = Some((guild.get(), channel.get()));
    if app.always_on() && app.prefs().stay_in != here {
        app.update_prefs(|p| p.stay_in = here);
    }
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
        (st.guild.take(), st.channel.take())
    };
    let (guild, channel) = guild;
    // Cleared while Loski is still in the channel, which the status needs.
    if let Some(channel) = channel {
        set_status(app, channel, "").await;
    }
    lyrics::close(app).await;
    if let Some(core) = app.core.get() {
        core.engine.detach_output();
    }
    if let Some(guild) = guild {
        let _ = app.songbird.remove(guild).await;
    }
    app.set_listening(None);
    friends::people_changed(app);
    // The panel becomes the session's recap; with nothing to sum up it just goes.
    let played = app
        .core
        .get()
        .map(|core| core.engine.take_played())
        .unwrap_or_default();
    let liked = friends::session_over(app);
    if recap::show(app, &played, liked, farewell).await {
        return;
    }
    panel::delete(app).await;
    if let (Some(text), Some(home)) = (farewell, app.ui.home()) {
        app.announce(home, cards::info(&app.icons, text), SHORT_SECS)
            .await;
    }
}

/// Leaves until you call Loski back: `/stop`, the sleep timer, or someone
/// disconnecting it. 24/7 doesn't bring it back until the next `/play`.
pub async fn send_away(app: &Arc<App>, farewell: Option<&str>) {
    if app.prefs().stay_in.is_some() {
        app.update_prefs(|p| p.stay_in = None);
    }
    leave(app, farewell).await;
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
    if app.voice.guild().is_none() || app.always_on() {
        return;
    }
    spawn_timer(app, app.idle_leave(), personality::leaving_idle(), |st| {
        &mut st.idle_timer
    });
}

fn schedule_owner_gone_leave(app: &Arc<App>) {
    if app.voice.state.lock().leave_timer.is_some() || app.always_on() {
        return;
    }
    spawn_timer(app, app.leave_after(), personality::leaving_alone(), |st| {
        &mut st.leave_timer
    });
}

/// The queue ran out: 24/7 keeps going with a radio, otherwise Loski
/// leaves after a while.
pub fn queue_ended(app: &Arc<App>) {
    if !app.always_on() {
        schedule_idle_leave(app);
        return;
    }
    if let Some(core) = app.core.get() {
        tokio::spawn(keep_going(app.clone(), core.clone()));
    }
}

/// 24/7: with nothing playing, a radio from the last song keeps the music
/// going, whether or not anyone's there to hear it.
async fn keep_going(app: Arc<App>, core: Arc<Core>) {
    if !app.always_on() {
        return;
    }
    let snap = core.engine.snapshot();
    if snap.status != Status::Idle || snap.following || snap.connect {
        return;
    }
    if app.voice.starting_radio.swap(true, Ordering::Relaxed) {
        return;
    }
    let started = library::radio_to_keep_going(&core, &app.store).await;
    app.voice.starting_radio.store(false, Ordering::Relaxed);
    match started {
        Ok(count) => tracing::info!("24/7: the queue ran out, so {count} songs from a radio"),
        Err(e) => tracing::warn!("24/7 couldn't start a radio: {e:#}"),
    }
}

/// 24/7: the music plays on with or without anyone in the call (time
/// alone doesn't count in your stats), and starts again if it stopped.
async fn check_room(app: &Arc<App>, core: &Arc<Core>) {
    if app.voice.channel().is_none() {
        return;
    }
    keep_going(app.clone(), core.clone()).await;
}

/// 24/7 was switched on or off in /settings.
pub async fn always_on_changed(app: &Arc<App>) {
    let Some(core) = app.core.get().filter(|_| app.voice.guild().is_some()) else {
        return;
    };
    if app.always_on() {
        app.voice.cancel_leave();
        app.voice.cancel_idle();
        check_room(app, core).await;
        return;
    }
    // Back to leaving by itself, the usual way.
    if !friends::someone_in_charge(app) {
        schedule_owner_gone_leave(app);
    }
    if core.engine.snapshot().status == Status::Idle {
        schedule_idle_leave(app);
    }
}

/// 24/7 after a restart: back to the channel Loski was in, and on with the
/// music if anyone's there.
pub async fn come_back(app: Arc<App>) {
    let Some((guild, channel)) = app.prefs().stay_in.filter(|_| app.always_on()) else {
        return;
    };
    let (Some(guild), Some(channel)) = (
        Id::<GuildMarker>::new_checked(guild),
        Id::<ChannelMarker>::new_checked(channel),
    ) else {
        return;
    };
    // The server (and who's in voice) arrives a moment after logging in.
    for _ in 0..60 {
        if app.cache.guild(guild).is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    // Discord tidies away a voice session left over from a crash first.
    tokio::time::sleep(Duration::from_secs(3)).await;
    let Some(core) = app.core.get() else { return };
    if app.voice.guild().is_some() {
        return;
    }
    if app.ui.home().is_none() {
        // The voice channel's own text chat.
        app.ui.set_home(channel);
    }
    match join(&app, core, guild, channel).await {
        Ok(()) => {
            tracing::info!("24/7: back in voice after a restart");
            check_room(&app, core).await;
        }
        Err(e) => tracing::warn!("24/7 couldn't get back into voice: {e:#}"),
    }
}

pub async fn on_voice_state(app: &Arc<App>, state: &DiscordVoiceState) {
    let Some(our_guild) = app.voice.guild() else {
        return;
    };
    if state.guild_id != Some(our_guild) {
        return;
    }

    if state.user_id == app.bot_id {
        let Some(channel) = state.channel_id else {
            // Someone disconnected us.
            send_away(app, None).await;
            return;
        };
        // Moved, by you or by someone else: a new room to check below.
        app.voice.state.lock().channel = Some(channel);
        remember_channel(app);
    } else if state.user_id == app.owner_id()
        && let Some(channel) = state.channel_id
        && app.voice.channel() != Some(channel)
        && let Some(core) = app.core.get()
        && let Err(e) = join(app, core, our_guild, channel).await
    {
        tracing::warn!("couldn't follow you to the new channel: {e:#}");
    }
    friends::people_changed(app);
    if app.always_on() {
        // 24/7 keeps the music going, whoever's there.
        if let Some(core) = app.core.get() {
            check_room(app, core).await;
        }
    } else if friends::someone_in_charge(app) {
        app.voice.cancel_leave();
    } else {
        // Nobody left who runs things (you or a co-DJ): leave in a minute.
        schedule_owner_gone_leave(app);
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

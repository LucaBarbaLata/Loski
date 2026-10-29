//! The live "now playing" card: one per session, kept at the bottom of the
//! channel you last used /play in (or the one picked in /settings), updated
//! as the song plays.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use librespot_core::SpotifyId;
use parking_lot::Mutex;
use serde_json::Value;
use tokio::sync::mpsc;
use twilight_model::{
    channel::message::{Component, component::ButtonStyle},
    http::attachment::Attachment,
    id::{
        Id,
        marker::{ChannelMarker, MessageMarker},
    },
};

use super::{
    App, Core, ERROR_SECS,
    cards::{self, Btn, color},
    friends::{self, RoomView},
    icons::{Icon, Icons},
    requests, voice,
};
use crate::{
    banner,
    color::cover_color,
    dsp::Filter,
    engine::{Notice, QueueView, Repeat, SleepState, Snapshot, Status},
    personality,
    store::HistoryEntry,
    track::TrackInfo,
    util::{escape_md, fmt_duration_words, fmt_ms, truncate},
};

/// How often the progress bar moves while a song plays.
const PROGRESS_EVERY: Duration = Duration::from_secs(10);
/// How often a lost panel is tried again.
const RETRY_LOST: Duration = Duration::from_secs(15);
/// Small changes (volume, repeat) are batched for this long.
const DEBOUNCE: Duration = Duration::from_millis(900);
const QUEUE_PAGE_SIZE: usize = 10;

/// The queue card's page (0-based) that shows upcoming position `pos`.
pub fn queue_page_of(pos: usize) -> usize {
    pos.saturating_sub(1) / QUEUE_PAGE_SIZE
}
/// Songs shown under "up next" on the panel.
const UP_NEXT_SHOWN: usize = 2;
/// Songs shown by /history.
pub const HISTORY_SIZE: usize = 15;
/// Of those, the newest few shown with their covers.
const HISTORY_COVERS: usize = 5;
const BAR_SEGMENTS: usize = 12;

pub(super) type PanelRef = (Id<ChannelMarker>, Id<MessageMarker>);

/// How the panel is drawn, from /settings.
#[derive(Debug, Clone, Copy, Default)]
pub struct Look {
    /// The slim panel: no artwork, one row of buttons.
    pub compact: bool,
    /// How the artwork looks, when there is some.
    pub style: banner::Style,
    /// 24/7 is on, so Loski isn't about to leave when the music ends.
    pub staying: bool,
}

struct UiState {
    /// The chat of the voice channel Loski is in: the panel's own place.
    home: Option<Id<ChannelMarker>>,
    /// The panel channel picked in /settings; it beats `home`.
    fixed: Option<Id<ChannelMarker>>,
    /// Where /panel brought it, until Loski leaves the call; it beats both.
    here: Option<Id<ChannelMarker>>,
    /// Where the last command was used, for when the voice channel's chat
    /// won't take the panel.
    asked: Option<Id<ChannelMarker>>,
    panel: Option<PanelRef>,
    /// The panel couldn't be updated or posted (a network blip, say): it's
    /// posted again shortly.
    lost: bool,
    /// The banner uploaded with the current panel, and the song it shows.
    banner: Option<(SpotifyId, String)>,
    color: u32,
    dirty: bool,
    last_edit: Instant,
    /// The goodbye recap left in chat, removed when the next session starts.
    recap: Option<PanelRef>,
    /// When the last session began and what played, for "Save as a mix".
    last_session: Option<(i64, Vec<Arc<TrackInfo>>)>,
}

pub struct Ui {
    state: Mutex<UiState>,
}

impl Default for Ui {
    fn default() -> Self {
        Self {
            state: Mutex::new(UiState {
                home: None,
                fixed: None,
                here: None,
                asked: None,
                panel: None,
                lost: false,
                banner: None,
                color: color::BRAND,
                dirty: false,
                last_edit: Instant::now(),
                recap: None,
                last_session: None,
            }),
        }
    }
}

impl Ui {
    /// Loski joined a voice channel: the panel lives in its chat.
    pub fn set_home(&self, channel: Id<ChannelMarker>) {
        self.state.lock().home = Some(channel);
    }

    /// A command was used here: where the panel goes if the voice
    /// channel's chat won't take it.
    pub fn asked_from(&self, channel: Id<ChannelMarker>) {
        self.state.lock().asked = Some(channel);
    }

    /// /panel: the panel stays here until Loski leaves the call.
    pub fn put_here(&self, channel: Id<ChannelMarker>) {
        self.state.lock().here = Some(channel);
    }

    /// The call is over: the next one's panel goes back to its own place.
    pub fn call_over(&self) {
        self.state.lock().here = None;
    }

    /// Where the panel goes: where /panel brought it, the channel picked
    /// in /settings, or the voice channel's chat.
    pub fn home(&self) -> Option<Id<ChannelMarker>> {
        let st = self.state.lock();
        st.here.or(st.fixed).or(st.home).or(st.asked)
    }

    /// The voice channel's chat wouldn't take the panel (no permission to
    /// post there, say): where the last command was used instead.
    fn fall_back(&self, failed: Id<ChannelMarker>) -> Option<Id<ChannelMarker>> {
        let mut st = self.state.lock();
        if st.here.is_some() || st.fixed.is_some() || st.home != Some(failed) {
            return None;
        }
        let other = st.asked.filter(|a| *a != failed)?;
        st.home = Some(other);
        Some(other)
    }

    /// Keeps the panel in one channel (`None`: the voice channel's chat).
    pub fn set_fixed(&self, channel: Option<Id<ChannelMarker>>) {
        self.state.lock().fixed = channel;
    }

    pub fn has_panel(&self) -> bool {
        self.state.lock().panel.is_some()
    }

    pub fn color(&self) -> u32 {
        self.state.lock().color
    }

    pub(super) fn mark_dirty(&self) {
        self.state.lock().dirty = true;
    }

    /// Call after the panel was updated another way (a button reply).
    pub fn mark_fresh(&self, message: Id<MessageMarker>) {
        let mut st = self.state.lock();
        if st.panel.is_some_and(|(_, m)| m == message) {
            st.dirty = false;
            st.last_edit = Instant::now();
        }
    }

    pub(super) fn take_panel(&self) -> Option<PanelRef> {
        let mut st = self.state.lock();
        st.banner = None;
        st.lost = false;
        st.panel.take()
    }

    pub(super) fn set_recap(&self, message: PanelRef, started_at: i64, songs: Vec<Arc<TrackInfo>>) {
        let mut st = self.state.lock();
        st.recap = Some(message);
        st.last_session = Some((started_at, songs));
    }

    fn take_recap(&self) -> Option<PanelRef> {
        self.state.lock().recap.take()
    }

    pub fn last_session(&self) -> Option<(i64, Vec<Arc<TrackInfo>>)> {
        self.state.lock().last_session.clone()
    }

    /// The uploaded banner's address, if it shows the song in `snap`.
    pub fn banner_for(&self, snap: &Snapshot) -> Option<String> {
        let st = self.state.lock();
        match (&st.banner, &snap.track) {
            (Some((id, url)), Some(track)) if *id == track.id => Some(url.clone()),
            _ => None,
        }
    }
}

pub async fn run(app: Arc<App>, core: Arc<Core>, mut notices: mpsc::UnboundedReceiver<Notice>) {
    let mut ticker = tokio::time::interval(Duration::from_millis(500));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            notice = notices.recv() => {
                let Some(notice) = notice else { break };
                on_notice(&app, &core, notice).await;
            }
            _ = ticker.tick() => refresh_if_due(&app, &core).await,
        }
    }
}

async fn on_notice(app: &Arc<App>, core: &Core, notice: Notice) {
    match notice {
        Notice::TrackStarted(track) => {
            app.voice.cancel_idle();
            let accent = match &track.cover_thumb_url {
                Some(url) => cover_color(&app.web, url).await,
                None => color::BRAND,
            };
            app.ui.state.lock().color = accent;
            friends::song_changed(app, &track);
            app.set_listening(Some(&track.display()));
            if core.engine.is_connect() {
                super::refresh_connect_queue(core).await;
            }
            repost(app, core).await;
            voice::show_song(app, Some(&track)).await;
            requests::party_turn(app).await;
        }
        Notice::Changed => app.ui.mark_dirty(),
        Notice::QueueEnded => {
            // The panel itself turns into the "last song" card.
            app.ui.mark_dirty();
            app.set_listening(None);
            voice::show_song(app, None).await;
            voice::queue_ended(app);
            // Party mode keeps going while requests are waiting.
            requests::party_turn(app).await;
        }
        Notice::ConnectStarted => voice::join_for_connect(app, core).await,
        Notice::SleepReached => {
            voice::send_away(app, Some(personality::sleep_reached())).await;
        }
        Notice::ConnectEnded => {
            app.ui.mark_dirty();
            app.set_listening(None);
            voice::show_song(app, None).await;
            voice::schedule_idle_leave(app);
        }
        Notice::Unavailable(track) => {
            if let Some(home) = app.ui.home() {
                let text = format!(
                    "Skipped **{}** by {}. Spotify won't let me play it here (region or licensing).",
                    escape_md(&track.title),
                    escape_md(&track.artists_joined())
                );
                app.announce(home, cards::error(&app.icons, &text), ERROR_SECS)
                    .await;
            }
        }
    }
}

async fn refresh_if_due(app: &App, core: &Core) {
    let retry = {
        let mut st = app.ui.state.lock();
        let due = st.lost && st.panel.is_none() && st.last_edit.elapsed() >= RETRY_LOST;
        if due {
            st.last_edit = Instant::now();
        }
        due
    };
    if retry {
        tracing::info!("Putting the panel back up after it couldn't be updated");
        repost(app, core).await;
        return;
    }
    let panel = {
        let st = app.ui.state.lock();
        let since = st.last_edit.elapsed();
        let due = (st.dirty && since >= DEBOUNCE) || since >= PROGRESS_EVERY;
        match st.panel {
            Some(panel) if due => panel,
            _ => return,
        }
    };
    let snap = core.engine.snapshot();
    {
        let mut st = app.ui.state.lock();
        // Nothing moves while paused or stopped, so only edit for real changes.
        let changed = st.dirty;
        st.dirty = false;
        st.last_edit = Instant::now();
        if !changed && snap.status != Status::Playing {
            return;
        }
    }
    let (channel, message) = panel;
    let components = [current_card(app, &snap)];
    let edit = app
        .http
        .update_message(channel, message)
        .flags(cards::V2)
        .components(Some(&components))
        .await;
    if let Err(e) = edit {
        tracing::warn!("couldn't update the panel ({e}); it'll be put back up shortly");
        let mut st = app.ui.state.lock();
        if st.panel == Some(panel) {
            st.panel = None;
            st.lost = true;
        }
    }
}

/// What's playing, for /nowplaying: the song, where it's at and what's
/// next. Only whoever asked sees it.
pub fn now_card(app: &App, snap: &Snapshot) -> Component {
    let icons = &app.icons;
    let Some(track) = &snap.track else {
        return cards::info(icons, personality::nothing_playing());
    };
    let paused = snap.status == Status::Paused;
    let explicit = if track.explicit { "  `Explicit`" } else { "" };
    let mut about = Vec::new();
    if !track.album.is_empty() {
        about.push(escape_md(&track.album));
    }
    about.extend(track.year.map(|year| year.to_string()));
    about.extend(app.genres.top_of(&track.artists));
    let mut lines = vec![
        format!(
            "{} **[{}]({})**{explicit}",
            icons.md(if paused { Icon::Pause } else { Icon::Play }),
            escape_md(&track.title),
            track.url()
        ),
        escape_md(&track.artists_joined()),
    ];
    if !about.is_empty() {
        lines.push(format!("-# {}", about.join(" · ")));
    }
    lines.push(format!(
        "`{}` {} `{}`{}",
        fmt_ms(snap.position_ms as u64),
        progress_bar(icons, snap.position_ms, track.duration_ms),
        fmt_ms(track.duration_ms as u64),
        if paused { " · paused" } else { "" }
    ));
    let mut notes = Vec::new();
    match &track.from {
        Some(from) if !snap.following && !snap.connect => notes.push(format!("From {from}")),
        _ => {}
    }
    if let Some(friend) = track.requested_by {
        notes.push(format!("asked for by <@{friend}>"));
    }
    if let Some(next) = snap.up_next.first().filter(|_| !snap.following) {
        notes.push(format!(
            "up next: **{}** by {}",
            escape_md(&truncate(&next.title, 60)),
            escape_md(&truncate(&next.artists_joined(), 40))
        ));
    }
    let mut parts = vec![cards::text_with_cover(
        lines.join("\n"),
        track.cover_thumb_url.as_deref(),
    )];
    if !notes.is_empty() {
        parts.push(cards::text(format!("-# {}", notes.join(" · "))));
    }
    cards::card(Some(app.ui.color()), parts)
}

/// Puts a fresh panel at the bottom of the home channel. If the current
/// panel is already the last message there, it's edited instead (no flicker).
pub async fn repost(app: &App, core: &Core) {
    let Some(mut home) = app.ui.home() else {
        return;
    };
    // A new session is starting: the last one's recap makes way.
    if let Some((channel, message)) = app.ui.take_recap() {
        let _ = app.http.delete_message(channel, message).await;
    }
    let snap = core.engine.snapshot();
    let look = app.look();
    let banner = match &snap.track {
        Some(track) if !look.compact => {
            let genre = app.genres.top_of(&track.artists);
            // The songs coming up get theirs looked up ahead of time.
            app.genres.want(
                snap.up_next
                    .iter()
                    .flat_map(|t| t.artists.iter().map(String::as_str)),
            );
            banner::for_track(&app.web, track, look.style, genre).await
        }
        _ => None,
    };
    let attachments: Vec<Attachment> = banner
        .iter()
        .map(|jpeg| Attachment::from_bytes(banner::FILENAME.to_string(), jpeg.to_vec(), 1))
        .collect();
    let reference = format!("attachment://{}", banner::FILENAME);
    let room = friends::room_view(app, snap.track.as_deref());
    let components = [now_playing_card(
        &app.icons,
        app.ui.color(),
        &snap,
        banner.as_ref().map(|_| reference.as_str()),
        look,
        &room,
    )];

    let mut posted = None;
    if let Some((channel, message)) = app.ui.take_panel() {
        if channel == home && is_latest(app, channel, message).await {
            // Replacing the attachments swaps in the new song's banner.
            let edited = app
                .http
                .update_message(channel, message)
                .flags(cards::V2)
                .attachments(&attachments)
                .components(Some(&components))
                .await;
            if let Ok(response) = edited {
                posted = Posted::read(response).await;
            }
        }
        if posted.is_none() {
            let _ = app.http.delete_message(channel, message).await;
        }
    }
    if posted.is_none() {
        posted = post(app, home, &attachments, &components).await;
        if posted.is_none()
            && let Some(other) = app.ui.fall_back(home)
        {
            tracing::warn!(
                "Loski can't post in the voice channel's chat, so the panel goes where /play was used. Let its role send messages in voice channels to keep it there."
            );
            home = other;
            posted = post(app, home, &attachments, &components).await;
        }
    }

    let Some(posted) = posted else {
        tracing::warn!("the panel couldn't be posted; trying again shortly");
        let mut st = app.ui.state.lock();
        st.lost = true;
        st.last_edit = Instant::now();
        return;
    };
    let mut st = app.ui.state.lock();
    st.panel = Some((home, posted.id));
    st.lost = false;
    // Later edits point at the uploaded file instead of re-sending it. If
    // Discord doesn't say where it went, they refer to it by name.
    st.banner = match (&snap.track, banner.is_some()) {
        (Some(track), true) => Some((track.id, posted.banner_url.unwrap_or(reference))),
        _ => None,
    };
    st.dirty = false;
    st.last_edit = Instant::now();
}

/// Posts a new panel in `channel`.
async fn post(
    app: &App,
    channel: Id<ChannelMarker>,
    attachments: &[Attachment],
    components: &[Component],
) -> Option<Posted> {
    match app
        .http
        .create_message(channel)
        .flags(cards::V2)
        .attachments(attachments)
        .components(components)
        .await
    {
        Ok(response) => Posted::read(response).await,
        Err(e) => {
            tracing::warn!("could not post the now playing panel: {e}");
            None
        }
    }
}

/// What Loski keeps from a posted panel. Read from Discord's raw reply, so
/// a field twilight doesn't know yet can't lose track of the panel.
struct Posted {
    id: Id<MessageMarker>,
    /// Where Discord stored the banner, if its reply says.
    banner_url: Option<String>,
}

impl Posted {
    async fn read<T>(response: twilight_http::Response<T>) -> Option<Self> {
        let bytes = response.bytes().await.ok()?;
        let json: Value = serde_json::from_slice(&bytes).ok()?;
        let id = serde_json::from_value(json.get("id")?.clone()).ok()?;
        let banner_url = banner_url(&json);
        static REPORTED: AtomicBool = AtomicBool::new(false);
        if !REPORTED.swap(true, Ordering::Relaxed) {
            let found = match &banner_url {
                Some(_) if listed_banner(&json).is_some() => "with the message's files",
                Some(_) => "inside the card",
                None => "nowhere, so edits refer to it by name",
            };
            tracing::info!("Discord's reply listed the panel banner {found}");
        }
        Some(Self { id, banner_url })
    }
}

fn listed_banner(message: &Value) -> Option<&str> {
    message["attachments"]
        .as_array()?
        .iter()
        .find(|a| a["filename"] == banner::FILENAME)?["url"]
        .as_str()
}

/// Discord may list the upload with the message's files, or only inside the
/// card as the gallery's resolved media. Either link works for edits.
fn banner_url(message: &Value) -> Option<String> {
    listed_banner(message)
        .map(str::to_string)
        .or_else(|| gallery_url(&message["components"]))
}

fn gallery_url(value: &Value) -> Option<String> {
    match value {
        Value::Array(items) => items.iter().find_map(gallery_url),
        Value::Object(map) => {
            // 12 is a media gallery; the banner is the only one on the panel.
            if map.get("type").and_then(Value::as_u64) == Some(12) {
                let media = &map.get("items")?[0]["media"];
                return [&media["url"], &media["proxy_url"]]
                    .into_iter()
                    .filter_map(Value::as_str)
                    .find(|url| url.starts_with("https://"))
                    .map(str::to_string);
            }
            map.values().find_map(gallery_url)
        }
        _ => None,
    }
}

async fn is_latest(app: &App, channel: Id<ChannelMarker>, message: Id<MessageMarker>) -> bool {
    let Ok(response) = app.http.channel_messages(channel).limit(1).await else {
        return false;
    };
    response
        .models()
        .await
        .ok()
        .and_then(|m| m.first().map(|m| m.id))
        == Some(message)
}

pub async fn delete(app: &App) {
    if let Some((channel, message)) = app.ui.take_panel() {
        let _ = app.http.delete_message(channel, message).await;
    }
}

fn progress_bar(icons: &Icons, position_ms: u32, duration_ms: u32) -> String {
    let knob = if duration_ms == 0 {
        0
    } else {
        ((position_ms as f64 / duration_ms as f64) * (BAR_SEGMENTS - 1) as f64).round() as usize
    }
    .min(BAR_SEGMENTS - 1);

    if !icons.is_ready() {
        return format!(
            "{}●{}",
            "━".repeat(knob),
            "─".repeat(BAR_SEGMENTS - 1 - knob)
        );
    }
    (0..BAR_SEGMENTS)
        .map(|i| {
            let icon = match i {
                i if i == knob => Icon::BarKnob,
                0 => Icon::BarStartOn,
                i if i < knob => Icon::BarMidOn,
                i if i == BAR_SEGMENTS - 1 => Icon::BarEndOff,
                _ => Icon::BarMidOff,
            };
            icons.md(icon)
        })
        .collect()
}

/// The panel as it should look right now, with this song's banner if one was uploaded.
pub fn current_card(app: &App, snap: &Snapshot) -> Component {
    let banner = app.ui.banner_for(snap);
    now_playing_card(
        &app.icons,
        app.ui.color(),
        snap,
        banner.as_deref(),
        app.look(),
        &friends::room_view(app, snap.track.as_deref()),
    )
}

pub fn now_playing_card(
    icons: &Icons,
    accent: u32,
    snap: &Snapshot,
    banner: Option<&str>,
    look: Look,
    room: &RoomView,
) -> Component {
    let Some(track) = &snap.track else {
        let text = if snap.following {
            "**Following your Spotify.** Play something there and it'll play here."
        } else {
            "Nothing playing. Use `/play` to start."
        };
        return cards::card(Some(accent), vec![cards::text(text)]);
    };
    let finished = snap.status == Status::Idle && !snap.following && !snap.connect;
    let kicker = match (snap.following, snap.connect, snap.status) {
        (_, _, Status::Paused) => "PAUSED",
        (true, _, _) => "FOLLOWING YOUR SPOTIFY",
        (_, true, Status::Idle) => "WAITING FOR YOUR SPOTIFY APP",
        (_, true, _) => "PLAYING FROM YOUR SPOTIFY APP",
        (false, false, Status::Loading) => "STARTING",
        (false, false, Status::Playing) => "NOW PLAYING",
        (false, false, Status::Idle) => "THAT WAS THE LAST SONG",
    };

    let mut parts = Vec::new();
    match banner.filter(|_| !look.compact) {
        Some(url) => {
            let alt = format!("{} by {}", track.title, track.artists_joined());
            parts.push(cards::image(url, &alt));
        }
        None => {
            let explicit = if track.explicit { "  `Explicit`" } else { "" };
            let mut heading = format!(
                "### [{}]({}){explicit}\n**{}**",
                escape_md(&truncate(&track.title, 90)),
                track.url(),
                escape_md(&truncate(&track.artists_joined(), 100))
            );
            if !track.album.is_empty() {
                heading.push_str(&format!(" · *{}*", escape_md(&truncate(&track.album, 60))));
            }
            parts.push(cards::text_with_cover(heading, track.cover_url.as_deref()));
        }
    }

    if finished {
        let plan = if look.staying {
            "24/7 is on, so I'm staying. `/play` something, or start a radio."
        } else {
            "I'll head out in a few minutes unless you `/play` something."
        };
        parts.push(cards::text(format!("-# {kicker}\n{plan}")));
        parts.push(cards::row(vec![
            Btn::icon("loski:radio", Icon::Radio)
                .with_label("Radio from this song")
                .style(ButtonStyle::Primary)
                .build(icons),
            Btn::icon("loski:replay", Icon::Repeat)
                .with_label("Replay queue")
                .build(icons),
            Btn::icon("loski:stop", Icon::Stop)
                .with_label("Leave")
                .style(ButtonStyle::Danger)
                .build(icons),
            cards::link(&track.url(), "Open in Spotify"),
        ]));
        return cards::card(Some(accent), parts);
    }

    // Like Spotify's "playing from", for anything queued as a group.
    let from = match &track.from {
        Some(from) if !snap.following && !snap.connect => format!(" · from {from}"),
        _ => String::new(),
    };
    let mut progress = format!(
        "-# {kicker}{from}\n`{}` {} `{}`",
        fmt_ms(snap.position_ms as u64),
        progress_bar(icons, snap.position_ms, track.duration_ms),
        fmt_ms(track.duration_ms as u64)
    );
    if look.compact {
        // Up next as one line instead of a list with covers.
        if let Some(next) = snap.up_next.first().filter(|_| !snap.following) {
            progress.push_str(&format!(
                "\n-# Next: **{}** · {}",
                escape_md(&truncate(&next.title, 60)),
                escape_md(&truncate(&next.artists_joined(), 40))
            ));
        }
        parts.push(cards::text(progress));
        parts.extend(room_line(icons, room));
    } else {
        parts.push(cards::text(progress));
        parts.extend(room_line(icons, room));
        parts.push(cards::gap());
        parts.extend(up_next(snap));
        parts.push(cards::gap());
    }

    let playing = matches!(snap.status, Status::Playing | Status::Loading);
    let idle = snap.status == Status::Idle;
    // The queue's own controls only make sense for the Discord queue;
    // previous/next also work for Spotify Connect.
    let queue_mode = !snap.following && !snap.connect;
    let transport = !snap.following;
    let repeat_icon = if snap.repeat == Repeat::Track {
        Icon::RepeatOne
    } else {
        Icon::Repeat
    };
    let repeat_style = if snap.repeat == Repeat::Off {
        ButtonStyle::Secondary
    } else {
        ButtonStyle::Success
    };

    let prev = Btn::icon("loski:prev", Icon::Prev)
        .disabled(!transport)
        .build(icons);
    let toggle = Btn::icon(
        "loski:toggle",
        if playing { Icon::Pause } else { Icon::Play },
    )
    .with_label(if playing { "Pause" } else { "Play" })
    .style(ButtonStyle::Primary)
    .disabled(idle)
    .build(icons);
    let skip = Btn::icon("loski:skip", Icon::Next)
        .disabled(!transport || idle)
        .build(icons);
    let stop = Btn::icon("loski:stop", Icon::Stop)
        .style(ButtonStyle::Danger)
        .build(icons);
    let footer = footer(snap, track);
    if look.compact {
        parts.extend([
            cards::row(vec![
                prev,
                toggle,
                skip,
                Btn::icon("loski:queue", Icon::Queue)
                    .disabled(!queue_mode)
                    .build(icons),
                stop,
            ]),
            cards::row(vec![more_menu(icons, snap, true)]),
            cards::text(footer),
        ]);
        return cards::card(Some(accent), parts);
    }

    let controls = cards::row(vec![
        prev,
        toggle,
        skip,
        Btn::icon("loski:shuffle", Icon::Shuffle)
            .disabled(!queue_mode || snap.upcoming < 2)
            .build(icons),
        Btn::icon("loski:repeat", repeat_icon)
            .style(repeat_style)
            .disabled(!queue_mode)
            .build(icons),
    ]);
    let second_row = cards::row(vec![
        Btn::icon("loski:voldown", Icon::VolDown)
            .disabled(snap.volume == 0)
            .build(icons),
        Btn::icon("loski:volup", Icon::VolUp)
            .disabled(snap.volume >= 100)
            .build(icons),
        Btn::icon("loski:queue", Icon::Queue)
            .with_label("Queue")
            .disabled(!queue_mode)
            .build(icons),
        Btn::icon("loski:lyrics", Icon::Lyrics)
            .with_label("Lyrics")
            .build(icons),
        stop,
    ]);
    parts.extend([
        controls,
        second_row,
        cards::row(vec![more_menu(icons, snap, false)]),
        cards::text(footer),
    ]);
    cards::card(Some(accent), parts)
}

/// The small print under the buttons: volume, what's left, the modes that are on.
fn footer(snap: &Snapshot, track: &TrackInfo) -> String {
    let mut footer = vec![format!("{}% volume", snap.volume)];
    if snap.upcoming > 0 {
        footer.push(format!(
            "{} in queue · {} left",
            snap.upcoming,
            fmt_duration_words(snap.upcoming_ms)
        ));
    }
    if snap.repeat != Repeat::Off {
        footer.push(format!("Repeat {}", snap.repeat.label()));
    }
    if snap.crossfade_secs > 0 {
        footer.push(format!("Crossfade {}s", snap.crossfade_secs));
    }
    if snap.filter != Filter::Off {
        footer.push(snap.filter.label().to_string());
    }
    match snap.sleep {
        Some(SleepState::AfterSong) => footer.push("Sleep after this song".to_string()),
        Some(SleepState::In(left)) => footer.push(format!(
            "Sleep in {} min",
            left.as_secs().div_ceil(60).max(1)
        )),
        None => {}
    }
    if snap.connect {
        footer.push("Spotify Connect".to_string());
    }
    footer.push(format!("[Open in Spotify]({})", track.url()));
    format!("-# {}", footer.join("  ·  "))
}

/// Who's listening and who liked this song, with the heart anyone in the
/// call can press. Nothing while Loski isn't in voice.
fn room_line(icons: &Icons, room: &RoomView) -> Option<Component> {
    if room.listeners.is_empty() {
        return None;
    }
    // A mic by the names of the people I listen to for voice commands.
    let mut text = format!(
        "-# LISTENING\n{}",
        friends::marked_names_list(&room.listeners, 4, &room.heard, &icons.md(Icon::Lyrics))
    );
    if !room.liked_by.is_empty() {
        text.push_str(&format!(
            "  ·  {} {}",
            icons.md(Icon::Liked),
            friends::names_list(&room.liked_by, 3)
        ));
    }
    if let Some((votes, needed)) = room.votes {
        text.push_str(&format!("  ·  skip votes {votes} of {needed}"));
    }
    let label = match room.liked_by.len() {
        0 => "Like".to_string(),
        n => n.to_string(),
    };
    let heart = Btn::icon("loski:like", Icon::Liked)
        .with_label(&label)
        .build(icons);
    Some(cards::section(text, heart))
}

/// Up next, each song with its cover, the way the Spotify app lists it.
fn up_next(snap: &Snapshot) -> Vec<Component> {
    if snap.following {
        return vec![cards::text("-# UP NEXT\nWhatever you play next on Spotify")];
    }
    if snap.up_next.is_empty() {
        return vec![cards::text(if snap.connect {
            "-# UP NEXT\nYour Spotify app is in charge. Pick another device there to go back to the queue."
        } else {
            "-# UP NEXT\nNothing yet. `/play` to add more"
        })];
    }
    let heading = if snap.connect {
        "-# UP NEXT IN YOUR SPOTIFY APP"
    } else {
        "-# UP NEXT"
    };
    let mut parts = vec![cards::text(heading)];
    for t in snap.up_next.iter().take(UP_NEXT_SHOWN) {
        let repeat = if snap.repeat == Repeat::Track && !snap.connect {
            " · on repeat"
        } else {
            ""
        };
        let line = format!(
            "**{}**\n-# {} · {}{repeat}",
            escape_md(&truncate(&t.title, 60)),
            escape_md(&truncate(&t.artists_joined(), 48)),
            fmt_ms(t.duration_ms as u64)
        );
        parts.push(cards::text_with_cover(line, t.cover_thumb_url.as_deref()));
    }
    parts
}

/// One menu for everything that doesn't need its own button: jumping
/// around, radio, the sound filters and moving the panel. The compact panel
/// also keeps the buttons it has no room for here.
fn more_menu(icons: &Icons, snap: &Snapshot, compact: bool) -> Component {
    let mut options = Vec::new();
    if compact {
        let queue_mode = !snap.following && !snap.connect;
        options.extend([
            cards::option(
                "Volume up",
                "volup",
                Some(format!("Now {}%", snap.volume)),
                Some(icons.emoji(Icon::VolUp)),
            ),
            cards::option(
                "Volume down",
                "voldown",
                None,
                Some(icons.emoji(Icon::VolDown)),
            ),
            cards::option("Lyrics", "lyrics", None, Some(icons.emoji(Icon::Lyrics))),
        ]);
        if queue_mode {
            options.extend([
                cards::option(
                    "Shuffle what's coming up",
                    "shuffle",
                    None,
                    Some(icons.emoji(Icon::Shuffle)),
                ),
                cards::option(
                    format!("Repeat: {} (change)", snap.repeat.label()),
                    "repeat",
                    Some("Off → the queue → this song".to_string()),
                    Some(icons.emoji(Icon::Repeat)),
                ),
            ]);
        }
    }
    if !snap.following {
        options.extend([
            cards::option(
                "Back 15 seconds",
                "back15",
                None,
                Some(icons.emoji(Icon::Back15)),
            ),
            cards::option(
                "Forward 15 seconds",
                "fwd15",
                None,
                Some(icons.emoji(Icon::Fwd15)),
            ),
            cards::option(
                "Jump to a time…",
                "seek",
                Some("Like 1:23, or +30 from here".to_string()),
                Some(icons.emoji(Icon::Search)),
            ),
            cards::option(
                "Restart this song",
                "restart",
                None,
                Some(icons.emoji(Icon::Prev)),
            ),
            cards::option(
                "Start radio from this song",
                "radio",
                Some("Replaces what's coming up".to_string()),
                Some(icons.emoji(Icon::Radio)),
            ),
        ]);
    }
    for filter in Filter::ALL {
        // Speed filters would drift away from your Spotify in follow mode.
        if snap.following && filter.speed() != 1.0 {
            continue;
        }
        let current = if filter == snap.filter { " (on)" } else { "" };
        options.push(cards::option(
            format!("Sound: {}{current}", filter.label()),
            format!("filter:{}", filter.key()),
            Some(filter.description().to_string()),
            Some(icons.emoji(Icon::Filters)),
        ));
    }
    let mine_on = matches!(snap.filter, Filter::Mine(_));
    options.extend([
        cards::option(
            format!("Sound: My sound{}", if mine_on { " (on)" } else { "" }),
            "filter:mine",
            Some("Your own EQ".to_string()),
            Some(icons.emoji(Icon::Filters)),
        ),
        cards::option(
            "Change My sound…",
            "eq",
            Some("Bass, mids and treble, each up or down".to_string()),
            Some(icons.emoji(Icon::Settings)),
        ),
    ]);
    options.push(cards::option(
        "Move this panel to the bottom",
        "repost",
        Some("Handy after a busy chat".to_string()),
        Some(icons.emoji(Icon::Music)),
    ));
    let placeholder = match snap.filter {
        Filter::Off => "More: sound, jump, radio…".to_string(),
        filter => format!("Sound: {} · more…", filter.label()),
    };
    cards::select(
        "loski:more",
        &placeholder,
        snap.status == Status::Idle,
        options,
    )
}

/// The queue, ten songs a page. `selected` is a 1-based queue position whose
/// actions (play now, move to top, remove) are shown.
pub fn queue_card(
    icons: &Icons,
    accent: u32,
    view: &QueueView,
    page: usize,
    selected: Option<usize>,
) -> Component {
    let pages = view.upcoming.len().div_ceil(QUEUE_PAGE_SIZE).max(1);
    let page = page.min(pages - 1);

    let now = match &view.current {
        Some(t) if view.status != Status::Idle => cards::text_with_cover(
            format!(
                "-# QUEUE\n**Now:** [{}]({}) · {} `{}`",
                escape_md(&truncate(&t.title, 60)),
                t.url(),
                escape_md(&truncate(&t.artists_joined(), 40)),
                fmt_ms(t.duration_ms as u64)
            ),
            t.cover_thumb_url.as_deref(),
        ),
        _ => cards::text("-# QUEUE\n**Now:** nothing"),
    };

    let list: Vec<String> = if view.upcoming.is_empty() {
        vec!["The queue is empty. Add songs with `/play`.".to_string()]
    } else {
        view.upcoming
            .iter()
            .enumerate()
            .skip(page * QUEUE_PAGE_SIZE)
            .take(QUEUE_PAGE_SIZE)
            .map(|(i, t)| {
                format!(
                    "`{:>2}` **{}** · {} `{}`",
                    i + 1,
                    escape_md(&truncate(&t.title, 50)),
                    escape_md(&truncate(&t.artists_joined(), 32)),
                    fmt_ms(t.duration_ms as u64)
                )
            })
            .collect()
    };

    let total: u64 = view.upcoming.iter().map(|t| t.duration_ms as u64).sum();
    let mut footer = format!(
        "-# Page {} of {} · {} songs · {}",
        page + 1,
        pages,
        view.upcoming.len(),
        fmt_duration_words(total)
    );
    if view.repeat != Repeat::Off {
        footer.push_str(&format!(" · Repeat {}", view.repeat.label()));
    }

    let mut components = vec![now, cards::divider()];
    components.extend(cards::text_blocks(list));
    components.push(cards::text(footer));

    if !view.upcoming.is_empty() {
        let options = view
            .upcoming
            .iter()
            .enumerate()
            .skip(page * QUEUE_PAGE_SIZE)
            .take(QUEUE_PAGE_SIZE)
            .map(|(i, t)| {
                let mut option = cards::option(
                    truncate(&format!("{}. {}", i + 1, t.title), 100),
                    format!("{}:{}", i + 1, t.base62()),
                    Some(truncate(&t.artists_joined(), 100)),
                    None,
                );
                option.default = selected == Some(i + 1);
                option
            })
            .collect();
        components.push(cards::row(vec![cards::select(
            &format!("loski:qsel:{page}"),
            "Pick a song to play, move or remove",
            false,
            options,
        )]));
    }

    let chosen = selected.and_then(|pos| Some((pos, view.upcoming.get(pos.checked_sub(1)?)?)));
    if let Some((pos, track)) = chosen {
        let target = format!("{pos}:{}:{page}", track.base62());
        components.push(cards::text(format!(
            "-# SELECTED\n`{pos}` **{}** · {}",
            escape_md(&truncate(&track.title, 60)),
            escape_md(&truncate(&track.artists_joined(), 40))
        )));
        let last = view.upcoming.len();
        components.push(cards::row(vec![
            Btn::icon(&format!("loski:qact:play:{target}"), Icon::Play)
                .with_label("Play now")
                .style(ButtonStyle::Primary)
                .build(icons),
            Btn::icon(&format!("loski:qact:top:{target}"), Icon::PlayNext)
                .with_label("Play next")
                .disabled(pos == 1)
                .build(icons),
            // One step at a time; the song stays picked for the next press.
            Btn::icon(&format!("loski:qact:up:{target}"), Icon::MoveUp)
                .disabled(pos == 1)
                .build(icons),
            Btn::icon(&format!("loski:qact:down:{target}"), Icon::MoveDown)
                .disabled(pos == last)
                .build(icons),
            Btn::label(&format!("loski:qact:to:{target}"), "Move to…")
                .disabled(last < 2)
                .build(icons),
        ]));
        components.push(cards::row(vec![
            Btn::label(&format!("loski:qact:del:{target}"), "Remove")
                .style(ButtonStyle::Danger)
                .build(icons),
            Btn::label(&format!("loski:qpage:{page}"), "Back").build(icons),
        ]));
    }

    let mut buttons = Vec::new();
    if pages > 1 {
        // Marked, as "Back" above can lead to the same page and Discord
        // turns down a card where two buttons share an id.
        buttons.push(
            Btn::icon(
                &format!("loski:qpage:{}:prev", page.saturating_sub(1)),
                Icon::Left,
            )
            .disabled(page == 0)
            .build(icons),
        );
        buttons.push(
            Btn::icon(&format!("loski:qpage:{}:next", page + 1), Icon::Right)
                .disabled(page + 1 >= pages)
                .build(icons),
        );
    }
    buttons.push(
        Btn::icon(&format!("loski:qshuffle:{page}"), Icon::Shuffle)
            .with_label("Shuffle")
            .disabled(view.upcoming.len() < 2)
            .build(icons),
    );
    let dedupe = match view.duplicates {
        0 => None,
        1 => Some("Remove 1 repeat".to_string()),
        n => Some(format!("Remove {n} repeats")),
    };
    buttons.push(
        Btn::icon("loski:qsavemix", Icon::Add)
            .with_label("Save as mix")
            .disabled(view.upcoming.is_empty() && view.current.is_none())
            .build(icons),
    );
    buttons.push(Btn::label("loski:qclose", "Close").build(icons));
    if let Some(label) = &dedupe {
        buttons.push(Btn::label(&format!("loski:qdedupe:{page}"), label).build(icons));
    }
    // Five buttons fit on a row.
    for row in buttons.chunks(5) {
        components.push(cards::row(row.to_vec()));
    }
    cards::card(Some(accent), components)
}

/// Recent plays, newest first, with a menu to play one again. `selected`
/// is the Spotify id picked in that menu.
pub fn history_card(
    icons: &Icons,
    accent: u32,
    entries: &[HistoryEntry],
    selected: Option<&str>,
    search: Option<(&str, usize)>,
) -> Component {
    let heading = match search {
        Some((about, total)) => format!(
            "{} **History** · {about}\n-# {} match, newest first",
            icons.md(Icon::History),
            if total == 1 {
                "1 play".to_string()
            } else {
                format!("{total} plays")
            }
        ),
        None => format!("{} **Recently played**", icons.md(Icon::History)),
    };
    if entries.is_empty() {
        let empty = if search.is_some() {
            "Nothing matches. Try fewer words, or a longer stretch of time."
        } else {
            "Nothing yet. Songs show up here once they've played for 30 seconds."
        };
        return cards::card(
            Some(accent),
            vec![cards::text(format!("{heading}\n{empty}"))],
        );
    }

    // The newest few with their covers, like the panel's up next; the
    // rest as plain lines so the card stays short.
    let mut components = vec![cards::text(heading)];
    let (recent, older) = entries.split_at(entries.len().min(HISTORY_COVERS));
    for entry in recent {
        let line = format!(
            "**[{}](https://open.spotify.com/track/{})**\n-# {} · <t:{}:R>",
            escape_md(&truncate(&entry.title, 60)),
            entry.track_id(),
            escape_md(&truncate(&entry.artists, 48)),
            entry.played_at
        );
        components.push(cards::text_with_cover(line, entry.cover.as_deref()));
    }
    if !older.is_empty() {
        let lines = older.iter().map(|entry| {
            format!(
                "-# <t:{}:R> · [{}](https://open.spotify.com/track/{}) · {}",
                entry.played_at,
                escape_md(&truncate(&entry.title, 50)),
                entry.track_id(),
                escape_md(&truncate(&entry.artists, 32))
            )
        });
        components.push(cards::gap());
        components.extend(cards::text_blocks(lines));
    }

    // Each song once in the menu, even if you played it a few times.
    let mut seen = std::collections::HashSet::new();
    let songs: Vec<&HistoryEntry> = entries
        .iter()
        .filter(|e| seen.insert(e.track_id()))
        .collect();
    let options = songs
        .iter()
        .map(|e| {
            let mut option = cards::option(
                truncate(&e.title, 100),
                e.track_id(),
                Some(truncate(&e.artists, 100)),
                None,
            );
            option.default = selected == Some(e.track_id());
            option
        })
        .collect();
    components.push(cards::row(vec![cards::select(
        "loski:hsel",
        "Play one of these again",
        false,
        options,
    )]));

    let chosen = selected.and_then(|id| songs.iter().find(|e| e.track_id() == id));
    match chosen {
        Some(entry) => {
            let id = entry.track_id();
            components.push(cards::text(format!(
                "-# SELECTED\n**{}** · {}",
                escape_md(&truncate(&entry.title, 60)),
                escape_md(&truncate(&entry.artists, 40))
            )));
            components.push(cards::row(vec![
                Btn::icon(&format!("loski:hgo:now:{id}"), Icon::Play)
                    .with_label("Play now")
                    .style(ButtonStyle::Primary)
                    .build(icons),
                Btn::icon(&format!("loski:hgo:next:{id}"), Icon::PlayNext)
                    .with_label("Play next")
                    .build(icons),
                Btn::icon(&format!("loski:hgo:end:{id}"), Icon::Add)
                    .with_label("Add to queue")
                    .build(icons),
                Btn::label("loski:hback", "Back").build(icons),
            ]));
        }
        None => components.push(cards::row(vec![
            Btn::label("loski:qclose", "Close").build(icons),
        ])),
    }
    cards::card(Some(accent), components)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_banner_wherever_discord_puts_it() {
        let listed = serde_json::json!({
            "id": "1",
            "attachments": [{"filename": "banner.jpg", "url": "https://cdn.discordapp.com/a/banner.jpg?ex=1"}],
            "components": []
        });
        assert_eq!(
            banner_url(&listed).as_deref(),
            Some("https://cdn.discordapp.com/a/banner.jpg?ex=1")
        );

        let in_card = serde_json::json!({
            "id": "1",
            "attachments": [],
            "components": [{"type": 17, "components": [
                {"type": 12, "items": [{"media": {
                    "url": "attachment://banner.jpg",
                    "proxy_url": "https://media.discordapp.net/a/banner.jpg",
                    "attachment_id": "9"
                }}]},
                {"type": 10, "content": "x"}
            ]}]
        });
        assert_eq!(
            banner_url(&in_card).as_deref(),
            Some("https://media.discordapp.net/a/banner.jpg")
        );

        let none = serde_json::json!({"id": "1", "components": [{"type": 10, "content": "x"}]});
        assert_eq!(banner_url(&none), None);
    }
}

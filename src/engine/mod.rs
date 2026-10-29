//! Playback engine: the queue, two librespot players ("decks") and the
//! rules for moving between songs (gapless, crossfade, follow mode).
//!
//! Everything here is synchronous and guarded by one mutex. librespot and
//! songbird calls only post messages to their own threads, so nothing blocks.

mod connect;
mod listen;
pub mod queue;
mod sound;
#[cfg(test)]
mod tests;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use librespot_core::{Session, SpotifyUri};
use librespot_playback::{
    config::{Bitrate, NormalisationType, PlayerConfig},
    player::{Player, PlayerEvent},
};
use parking_lot::Mutex;
use songbird::tracks::TrackHandle;
use tokio::sync::mpsc;

use crate::{
    audio::{CONNECT_DECK, Mixer, PIPE_CAPACITY_MS},
    dsp::Filter,
    store::{Settings, Store},
    track::TrackInfo,
};
pub use listen::Played;
use queue::Queue;
pub use queue::Repeat;
pub use sound::SleepState;

const TICK: Duration = Duration::from_millis(200);
/// "Previous" restarts the song instead once it's been playing this long.
const RESTART_THRESHOLD_MS: u32 = 3_000;
/// Follow mode re-syncs when it drifts further than this from your Spotify.
const FOLLOW_MAX_DRIFT_MS: i64 = 3_000;
/// Rough delay between Spotify reporting a position and us playing it.
const FOLLOW_LATENCY_MS: u32 = 250;
/// Give up after this many unplayable songs in a row.
const MAX_CONSECUTIVE_FAILURES: u32 = 10;
/// A song this close to its end that has gone quiet is over.
const OUTRO_WINDOW_MS: u32 = 20_000;
/// How long it has to stay quiet first.
const QUIET_OUTRO_MS: u32 = 1_500;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Idle,
    Loading,
    Playing,
    Paused,
}

/// Things the Discord side wants to react to.
#[derive(Debug)]
pub enum Notice {
    TrackStarted(Arc<TrackInfo>),
    Changed,
    QueueEnded,
    Unavailable(Arc<TrackInfo>),
    /// You picked Loski as a device in the Spotify app.
    ConnectStarted,
    /// Playback moved off Loski in the Spotify app.
    ConnectEnded,
    /// The sleep timer is up: time to leave.
    SleepReached,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub track: Option<Arc<TrackInfo>>,
    pub status: Status,
    pub position_ms: u32,
    /// The next few songs, in play order.
    pub up_next: Vec<Arc<TrackInfo>>,
    pub upcoming: usize,
    pub upcoming_ms: u64,
    pub volume: u8,
    pub repeat: Repeat,
    pub crossfade_secs: u8,
    pub following: bool,
    /// The Spotify app is in control (Spotify Connect).
    pub connect: bool,
    pub filter: Filter,
    pub sleep: Option<SleepState>,
}

pub struct QueueView {
    pub current: Option<Arc<TrackInfo>>,
    pub status: Status,
    pub upcoming: Vec<Arc<TrackInfo>>,
    pub repeat: Repeat,
    pub following: bool,
    pub connect: bool,
    /// Upcoming songs that are already playing or earlier in the queue.
    pub duplicates: usize,
}

/// How playback is doing, for /status.
pub struct Health {
    pub status: Status,
    pub mode: &'static str,
    /// Audio decoded and waiting to be sent to Discord.
    pub buffered_ms: u32,
    pub buffer_capacity_ms: u32,
    /// Loski shows up as a device in the Spotify app.
    pub connect_ready: bool,
}

pub struct Enqueued {
    /// Upcoming position of the first added song (1 = next).
    pub position: usize,
    pub started: bool,
    /// Follow mode or Spotify Connect, if adding songs switched it off.
    pub left_mode: Option<&'static str>,
}

pub struct FollowTarget {
    pub track: Arc<TrackInfo>,
    pub playing: bool,
    pub progress_ms: u32,
}

/// What the engine asks of a deck's player: librespot's `Player` when
/// Loski runs, a fake that records every call in the tests.
trait DeckPlayer: Send + Sync {
    fn load(&self, uri: SpotifyUri, play: bool, position_ms: u32);
    fn preload(&self, uri: SpotifyUri);
    fn play(&self);
    fn pause(&self);
    fn stop(&self);
    fn seek(&self, position_ms: u32);
    fn set_session(&self, session: Session);
}

impl DeckPlayer for Player {
    fn load(&self, uri: SpotifyUri, play: bool, position_ms: u32) {
        Player::load(self, uri, play, position_ms);
    }

    fn preload(&self, uri: SpotifyUri) {
        Player::preload(self, uri);
    }

    fn play(&self) {
        Player::play(self);
    }

    fn pause(&self) {
        Player::pause(self);
    }

    fn stop(&self) {
        Player::stop(self);
    }

    fn seek(&self, position_ms: u32) {
        Player::seek(self, position_ms);
    }

    fn set_session(&self, session: Session) {
        Player::set_session(self, session);
    }
}

struct Deck {
    player: Arc<dyn DeckPlayer>,
    /// Request id of our latest load; events from older loads are ignored.
    request: Option<u64>,
    /// A load was sent but librespot hasn't assigned it an id yet.
    pending: bool,
}

impl Deck {
    fn load(&mut self, track: &TrackInfo, play: bool, position_ms: u32) {
        self.pending = true;
        self.request = None;
        self.player.load(track.uri(), play, position_ms);
    }

    fn is_current(&self, request: u64) -> bool {
        !self.pending && self.request == Some(request)
    }
}

#[derive(Clone, Copy)]
struct Fade {
    from: usize,
    to: usize,
    index: usize,
    /// The incoming song has started and is now the current one.
    switched: bool,
}

enum Mode {
    Queue,
    Follow {
        track: Option<Arc<TrackInfo>>,
        /// Last play state seen on your Spotify, so a pause from Discord sticks
        /// until you change something there.
        last_playing: Option<bool>,
    },
    /// The Spotify app drives a separate player (see `connect.rs`).
    Connect {
        track: Option<Arc<TrackInfo>>,
    },
}

/// Song position, in the song's own time (a nightcore minute is 48 seconds).
struct Clock {
    base_ms: u32,
    since: Option<Instant>,
    rate: f64,
}

impl Default for Clock {
    fn default() -> Self {
        Self {
            base_ms: 0,
            since: None,
            rate: 1.0,
        }
    }
}

impl Clock {
    fn position(&self) -> u32 {
        let elapsed = self
            .since
            .map_or(0.0, |s| s.elapsed().as_millis() as f64 * self.rate);
        self.base_ms + elapsed as u32
    }

    fn set_rate(&mut self, rate: f64) {
        let position = self.position();
        self.rate = rate;
        if self.since.is_some() {
            self.run_from(position);
        } else {
            self.hold_at(position);
        }
    }

    fn run_from(&mut self, ms: u32) {
        self.base_ms = ms;
        self.since = Some(Instant::now());
    }

    fn hold_at(&mut self, ms: u32) {
        self.base_ms = ms;
        self.since = None;
    }

    fn resume(&mut self) {
        if self.since.is_none() {
            self.since = Some(Instant::now());
        }
    }

    /// Silence at the start of the song was skipped: the song is further along.
    fn skip(&mut self, ms: u32) {
        self.base_ms += ms;
    }
}

struct Inner {
    decks: [Deck; 2],
    active: usize,
    mixer: Arc<Mixer>,
    output: Option<TrackHandle>,
    output_playing: bool,
    queue: Queue,
    mode: Mode,
    status: Status,
    clock: Clock,
    fade: Option<Fade>,
    failures: u32,
    settings: Settings,
    store: Arc<Store>,
    notices: mpsc::UnboundedSender<Notice>,
    connect: Option<connect::ConnectHandle>,
    /// Soft pause: the output is fading; the players pause once it's silent.
    pending_pause: Option<Instant>,
    sleep: sound::Sleep,
    /// Kept so the decks can be rebuilt (normalisation switch).
    session: Session,
    normalise: bool,
    deck_events: mpsc::UnboundedSender<(usize, PlayerEvent)>,
    /// The song being timed for the history.
    listen: Option<listen::Listen>,
    /// Songs played since Loski last left voice.
    played: Vec<Played>,
    /// What the Spotify app has coming up, while it's in control.
    connect_up_next: Vec<Arc<TrackInfo>>,
    /// Friends in the call right now (Discord user ids), for the history.
    audience: Vec<u64>,
    /// Nobody's in the call: what plays now doesn't count as listened to.
    alone: bool,
    /// How long Loski has played, anyone listening or not, on its way to
    /// the store.
    played_clock: PlayedClock,
}

/// Time played, counted each tick and saved about once a minute.
#[derive(Default)]
struct PlayedClock {
    last: Option<Instant>,
    /// The Unix hour the time below belongs to.
    hour: i64,
    ms: u64,
    saved: Option<Instant>,
}

/// How often the time played is saved.
const SAVE_PLAYED_EVERY: Duration = Duration::from_secs(60);

pub struct Engine {
    inner: Mutex<Inner>,
    mixer: Arc<Mixer>,
}

pub fn volume_gain(volume: u8) -> f32 {
    // Squared so the slider feels even to the ear instead of all happening at the top.
    let v = volume.min(100) as f32 / 100.0;
    v * v
}

pub(crate) fn player_config(normalise: bool) -> PlayerConfig {
    PlayerConfig {
        bitrate: Bitrate::Bitrate320,
        gapless: true,
        normalisation: normalise,
        normalisation_type: NormalisationType::Track,
        // We hand songbird floats, so there's nothing to dither.
        ditherer: None,
        ..PlayerConfig::default()
    }
}

impl Engine {
    pub fn new(
        session: Session,
        normalise: bool,
        store: Arc<Store>,
        notices: mpsc::UnboundedSender<Notice>,
    ) -> Arc<Self> {
        let mixer = Mixer::new();
        let settings = store.load_settings();
        let normalise = settings.normalise.unwrap_or(normalise);
        mixer.set_volume(volume_gain(settings.volume));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let decks = [0usize, 1].map(|deck| sound::new_deck(&session, normalise, &mixer, deck, &tx));

        let engine = Arc::new(Self {
            inner: Mutex::new(Inner {
                decks,
                active: 0,
                mixer: mixer.clone(),
                output: None,
                output_playing: false,
                queue: Queue::default(),
                mode: Mode::Queue,
                status: Status::Idle,
                clock: Clock::default(),
                fade: None,
                failures: 0,
                settings,
                store,
                notices,
                connect: None,
                pending_pause: None,
                sleep: sound::Sleep::default(),
                session,
                normalise,
                deck_events: tx,
                listen: None,
                played: Vec::new(),
                connect_up_next: Vec::new(),
                audience: Vec::new(),
                alone: false,
                played_clock: PlayedClock::default(),
            }),
            mixer,
        });

        let weak = Arc::downgrade(&engine);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(TICK);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    Some((deck, event)) = rx.recv() => {
                        let Some(engine) = weak.upgrade() else { break };
                        engine.inner.lock().on_player_event(deck, event);
                    }
                    _ = tick.tick() => {
                        let Some(engine) = weak.upgrade() else { break };
                        engine.inner.lock().on_tick();
                    }
                }
            }
        });

        engine
    }

    /// Loski says something in the call (talking back): heard over the
    /// music, while it's paused, or with nothing playing.
    /// How long it takes, the music's fade before it included.
    pub fn say(&self, mono: &[f32], rate: u32) -> Duration {
        let mut g = self.inner.lock();
        let took = g.mixer.say(mono, rate);
        g.ensure_output_playing();
        took
    }

    pub fn mixer(&self) -> Arc<Mixer> {
        self.mixer.clone()
    }

    pub fn set_session(&self, session: Session) {
        let mut g = self.inner.lock();
        for deck in &g.decks {
            deck.player.set_session(session.clone());
        }
        g.session = session;
    }

    pub fn output(&self) -> Option<TrackHandle> {
        self.inner.lock().output.clone()
    }

    pub fn attach_output(&self, handle: TrackHandle) {
        let mut g = self.inner.lock();
        // Volume is applied (smoothly) in the mixer instead.
        let _ = handle.set_volume(1.0);
        if matches!(g.status, Status::Playing | Status::Loading) {
            let _ = handle.play();
            g.output_playing = true;
        } else {
            let _ = handle.pause();
            g.output_playing = false;
        }
        g.output = Some(handle);
    }

    /// Stops everything and forgets the queue (we left voice).
    pub fn detach_output(&self) {
        let mut g = self.inner.lock();
        g.leave_connect(true);
        g.stop_decks();
        g.reset_sound();
        g.queue.clear();
        g.mode = Mode::Queue;
        g.status = Status::Idle;
        g.clock.hold_at(0);
        if let Some(out) = g.output.take() {
            let _ = out.stop();
        }
        g.output_playing = false;
    }

    pub fn snapshot(&self) -> Snapshot {
        let g = self.inner.lock();
        let following = matches!(g.mode, Mode::Follow { .. });
        let connect = matches!(g.mode, Mode::Connect { .. });
        let track = g.current_track();
        let up_next = match (following || connect, g.queue.repeat, &track) {
            _ if connect => g.connect_up_next.iter().take(3).cloned().collect(),
            (true, _, _) => Vec::new(),
            (false, Repeat::Track, Some(current)) if g.status != Status::Idle => {
                vec![current.clone()]
            }
            _ => g.queue.upcoming().iter().take(3).cloned().collect(),
        };
        let position_ms = match &track {
            Some(t) if g.status != Status::Idle => g.clock.position().min(t.duration_ms),
            _ => 0,
        };
        Snapshot {
            track,
            status: g.status,
            position_ms,
            up_next,
            upcoming: if following || connect {
                0
            } else {
                g.queue.upcoming().len()
            },
            upcoming_ms: if following || connect {
                0
            } else {
                g.queue.upcoming_duration_ms()
            },
            volume: g.settings.volume,
            repeat: g.queue.repeat,
            crossfade_secs: g.settings.crossfade_secs,
            following,
            connect,
            filter: g.mixer.filter(),
            sleep: g.sleep.state(),
        }
    }

    pub fn queue_view(&self) -> QueueView {
        let g = self.inner.lock();
        QueueView {
            current: g.current_track(),
            status: g.status,
            upcoming: g.queue.upcoming().to_vec(),
            repeat: g.queue.repeat,
            following: matches!(g.mode, Mode::Follow { .. }),
            connect: matches!(g.mode, Mode::Connect { .. }),
            duplicates: g.queue.duplicates(),
        }
    }

    pub fn is_connect(&self) -> bool {
        matches!(self.inner.lock().mode, Mode::Connect { .. })
    }

    /// The Spotify app's own up next, shown on the panel while it's in control.
    pub fn set_connect_up_next(&self, tracks: Vec<TrackInfo>) {
        let mut g = self.inner.lock();
        if !matches!(g.mode, Mode::Connect { .. }) {
            return;
        }
        let same = tracks.len() == g.connect_up_next.len()
            && tracks
                .iter()
                .zip(&g.connect_up_next)
                .all(|(a, b)| a.id == b.id);
        if !same {
            g.connect_up_next = tracks.into_iter().map(Arc::new).collect();
            g.notify(Notice::Changed);
        }
    }

    /// Who's in the call besides you, so the history knows who you listened with.
    pub fn set_audience(&self, friends: Vec<u64>) {
        let mut g = self.inner.lock();
        // Time so far belongs to whoever was there before.
        g.tick_listen();
        g.audience = friends;
        g.tick_listen();
    }

    /// Whether anyone's in the call to hear the music, for the history.
    pub fn set_alone(&self, alone: bool) {
        let mut g = self.inner.lock();
        if g.alone != alone {
            // Time so far counts as it was.
            g.tick_listen();
            g.alone = alone;
        }
    }

    /// What played since the last call (or since Loski started).
    pub fn take_played(&self) -> Vec<Played> {
        std::mem::take(&mut self.inner.lock().played)
    }

    /// Every song in the queue, played ones included, in order.
    pub fn queue_tracks(&self) -> Vec<Arc<TrackInfo>> {
        self.inner.lock().queue.items().to_vec()
    }

    pub fn health(&self) -> Health {
        let g = self.inner.lock();
        let (mode, pipe) = match g.mode {
            Mode::Queue => ("Discord queue", g.active),
            Mode::Follow { .. } => ("Following your Spotify", g.active),
            Mode::Connect { .. } => ("Spotify Connect", CONNECT_DECK),
        };
        Health {
            status: g.status,
            mode,
            buffered_ms: g.mixer.pipes[pipe].buffered_ms(),
            buffer_capacity_ms: PIPE_CAPACITY_MS,
            connect_ready: g.connect.is_some(),
        }
    }

    pub fn enqueue(&self, tracks: Vec<TrackInfo>, play_next: bool) -> Enqueued {
        let mut g = self.inner.lock();
        let left_mode = g.leave_special_mode();
        let tracks = tracks.into_iter().map(Arc::new).collect();
        let position = g.queue.add(tracks, play_next);
        let started = g.status == Status::Idle;
        if started {
            if let Some(index) = g.queue.next_index(true) {
                g.play_index(index);
            }
        } else {
            // The song after the current one may have changed.
            g.drop_pending_fade();
            g.notify(Notice::Changed);
        }
        Enqueued {
            position,
            started,
            left_mode,
        }
    }

    /// Plays these songs right away; the rest of the queue follows them.
    pub fn play_now(&self, tracks: Vec<TrackInfo>) -> Enqueued {
        let mut g = self.inner.lock();
        let left_mode = g.leave_special_mode();
        let tracks = tracks.into_iter().map(Arc::new).collect();
        let position = g.queue.add(tracks, true);
        if let Some(index) = g.queue.index_of(position) {
            g.play_index(index);
        }
        Enqueued {
            position,
            started: true,
            left_mode,
        }
    }

    /// Replaces everything after the current song, or starts playing these
    /// if nothing is playing. Used for radio.
    pub fn replace_upcoming(&self, tracks: Vec<TrackInfo>) -> Enqueued {
        let mut g = self.inner.lock();
        let left_mode = g.leave_special_mode();
        g.drop_pending_fade();
        g.queue.clear_upcoming();
        let tracks = tracks.into_iter().map(Arc::new).collect();
        let position = g.queue.add(tracks, false);
        let started = g.status == Status::Idle;
        if started {
            if let Some(index) = g.queue.next_index(true) {
                g.play_index(index);
            }
        } else {
            g.notify(Notice::Changed);
        }
        Enqueued {
            position,
            started,
            left_mode,
        }
    }

    /// The song playing (or last played, once the queue ended).
    pub fn current_track(&self) -> Option<Arc<TrackInfo>> {
        self.inner.lock().current_track()
    }

    /// Starts the finished queue again from the top.
    pub fn replay(&self) -> bool {
        let mut g = self.inner.lock();
        if !matches!(g.mode, Mode::Queue) || g.status != Status::Idle || g.queue.is_empty() {
            return false;
        }
        g.play_index(0);
        true
    }

    /// Returns the new paused state, or None if nothing is playing.
    pub fn toggle_pause(&self) -> Option<bool> {
        let mut g = self.inner.lock();
        match g.status {
            Status::Playing | Status::Loading => g.pause().then_some(true),
            Status::Paused => g.resume().then_some(false),
            Status::Idle => None,
        }
    }

    pub fn pause(&self) -> bool {
        self.inner.lock().pause()
    }

    pub fn resume(&self) -> bool {
        self.inner.lock().resume()
    }

    /// Skips `count` songs and returns the one that was playing.
    pub fn skip(&self, count: usize) -> Option<Arc<TrackInfo>> {
        let mut g = self.inner.lock();
        if let Some(spirc) = g.connect_spirc() {
            let current = g.current_track();
            let _ = spirc.next();
            return current;
        }
        if !matches!(g.mode, Mode::Queue) || g.status == Status::Idle {
            return None;
        }
        let skipped = g.queue.current().cloned()?;
        let target = if count <= 1 {
            g.queue.next_index(true)
        } else {
            g.queue.index_of(count)
        };
        match target {
            Some(index) => g.play_index(index),
            None => {
                g.stop_decks();
                g.finish_queue();
            }
        }
        Some(skipped)
    }

    /// The song at 1-based upcoming `position`.
    pub fn track_at(&self, position: usize) -> Option<Arc<TrackInfo>> {
        let g = self.inner.lock();
        g.queue
            .index_of(position)
            .and_then(|i| g.queue.get(i).cloned())
    }

    pub fn skip_to(&self, position: usize) -> Option<Arc<TrackInfo>> {
        let mut g = self.inner.lock();
        if !matches!(g.mode, Mode::Queue) {
            return None;
        }
        let index = g.queue.index_of(position)?;
        let track = g.queue.get(index).cloned();
        g.play_index(index);
        track
    }

    /// Restarts the song, or goes back one if it only just started.
    pub fn previous(&self) -> bool {
        let mut g = self.inner.lock();
        if let Some(spirc) = g.connect_spirc() {
            return spirc.prev().is_ok();
        }
        if !matches!(g.mode, Mode::Queue) {
            return false;
        }
        if g.status != Status::Idle && g.clock.position() > RESTART_THRESHOLD_MS {
            return g.seek(0);
        }
        let target = match g.status {
            Status::Idle => g.queue.current_index(),
            _ => g.queue.previous_index().or(g.queue.current_index()),
        };
        match target {
            Some(index) => {
                g.play_index(index);
                true
            }
            None => false,
        }
    }

    pub fn seek(&self, position_ms: u32) -> bool {
        self.inner.lock().seek(position_ms)
    }

    pub fn position_ms(&self) -> Option<u32> {
        let g = self.inner.lock();
        (g.status != Status::Idle).then(|| g.clock.position())
    }

    pub fn remove(&self, position: usize) -> Option<Arc<TrackInfo>> {
        let mut g = self.inner.lock();
        g.drop_pending_fade();
        let removed = g.queue.remove(position);
        g.notify(Notice::Changed);
        removed
    }

    /// Removes 1-based upcoming positions `from` to `to`, both included.
    pub fn remove_range(&self, from: usize, to: usize) -> usize {
        let mut g = self.inner.lock();
        g.drop_pending_fade();
        let removed = g.queue.remove_range(from, to);
        g.notify(Notice::Changed);
        removed
    }

    /// Drops upcoming songs that are already playing or earlier in the queue.
    pub fn dedupe(&self) -> usize {
        let mut g = self.inner.lock();
        g.drop_pending_fade();
        let removed = g.queue.dedupe();
        g.notify(Notice::Changed);
        removed
    }

    pub fn move_track(&self, from: usize, to: usize) -> Option<Arc<TrackInfo>> {
        let mut g = self.inner.lock();
        g.drop_pending_fade();
        let moved = g.queue.move_track(from, to);
        g.notify(Notice::Changed);
        moved
    }

    pub fn clear(&self) -> usize {
        let mut g = self.inner.lock();
        g.drop_pending_fade();
        let removed = g.queue.clear_upcoming();
        g.notify(Notice::Changed);
        removed
    }

    pub fn shuffle(&self) -> usize {
        let mut g = self.inner.lock();
        g.drop_pending_fade();
        let count = g.queue.shuffle_upcoming();
        g.notify(Notice::Changed);
        count
    }

    /// Remembers what's coming up, for the Undo button of the change about to happen.
    pub fn save_undo(&self) -> u64 {
        self.inner.lock().queue.save_undo()
    }

    /// Puts back what was coming up before a change; returns how many songs that is.
    pub fn undo(&self, id: u64) -> Result<usize, &'static str> {
        let mut g = self.inner.lock();
        if !matches!(g.mode, Mode::Queue) {
            return Err("The queue isn't in charge right now, so there's nothing to undo.");
        }
        g.drop_pending_fade();
        let count = g.queue.undo(id)?;
        match g.queue.next_index(true) {
            Some(index) if g.status == Status::Idle => g.play_index(index),
            _ => g.notify(Notice::Changed),
        }
        Ok(count)
    }

    pub fn set_repeat(&self, repeat: Repeat) {
        let mut g = self.inner.lock();
        g.queue.repeat = repeat;
        g.drop_pending_fade();
        g.notify(Notice::Changed);
    }

    pub fn cycle_repeat(&self) -> Repeat {
        let next = self.inner.lock().queue.repeat.cycle();
        self.set_repeat(next);
        next
    }

    pub fn volume(&self) -> u8 {
        self.inner.lock().settings.volume
    }

    pub fn set_volume(&self, volume: u8) -> u8 {
        let mut g = self.inner.lock();
        g.settings.volume = volume.min(100);
        g.mixer.set_volume(volume_gain(g.settings.volume));
        g.save_settings();
        g.notify(Notice::Changed);
        g.settings.volume
    }

    pub fn crossfade(&self) -> u8 {
        self.inner.lock().settings.crossfade_secs
    }

    pub fn set_crossfade(&self, secs: u8) {
        let mut g = self.inner.lock();
        g.settings.crossfade_secs = secs;
        g.drop_pending_fade();
        g.save_settings();
        g.notify(Notice::Changed);
    }

    pub fn is_following(&self) -> bool {
        matches!(self.inner.lock().mode, Mode::Follow { .. })
    }

    pub fn set_following(&self, on: bool) {
        let mut g = self.inner.lock();
        match (on, &g.mode) {
            (true, Mode::Queue | Mode::Connect { .. }) => {
                g.leave_connect(true);
                g.drop_speed_filter();
                g.stop_decks();
                g.status = Status::Idle;
                g.clock.hold_at(0);
                g.mode = Mode::Follow {
                    track: None,
                    last_playing: None,
                };
                g.notify(Notice::Changed);
            }
            (false, Mode::Follow { .. }) => {
                g.leave_special_mode();
                match g.queue.next_index(true) {
                    Some(index) => g.play_index(index),
                    None => g.notify(Notice::Changed),
                }
            }
            _ => {}
        }
    }

    /// Applies what your Spotify is doing right now (follow mode only).
    pub fn follow_sync(&self, target: Option<FollowTarget>) {
        let mut g = self.inner.lock();
        let Mode::Follow {
            track,
            last_playing,
        } = &g.mode
        else {
            return;
        };
        let current_id = track.as_ref().map(|t| t.id);
        let last_playing = *last_playing;

        let Some(target) = target else {
            if last_playing != Some(false) {
                g.set_follow_last_playing(Some(false));
                g.pause();
            }
            return;
        };

        if current_id != Some(target.track.id) {
            if current_id.is_some() && g.sleep.take_after_song() {
                g.stop_decks();
                g.status = Status::Idle;
                g.notify(Notice::SleepReached);
                return;
            }
            let position = if target.playing {
                target.progress_ms + FOLLOW_LATENCY_MS
            } else {
                target.progress_ms
            };
            g.stop_decks();
            g.mode = Mode::Follow {
                track: Some(target.track.clone()),
                last_playing: Some(target.playing),
            };
            let active = g.active;
            g.start_track(active, target.track, target.playing, position, false);
            return;
        }

        if last_playing != Some(target.playing) {
            g.set_follow_last_playing(Some(target.playing));
            if target.playing {
                if !g.resume() && g.status == Status::Idle {
                    // Our copy already ended; pick it back up where you are.
                    let active = g.active;
                    g.start_track(
                        active,
                        target.track,
                        true,
                        target.progress_ms + FOLLOW_LATENCY_MS,
                        false,
                    );
                    return;
                }
            } else {
                g.pause();
            }
        }

        if target.playing && g.status == Status::Playing {
            let drift = g.clock.position() as i64 - target.progress_ms as i64;
            if drift.abs() > FOLLOW_MAX_DRIFT_MS {
                g.seek(target.progress_ms + FOLLOW_LATENCY_MS);
            }
        }
    }
}

impl Inner {
    fn notify(&self, notice: Notice) {
        let _ = self.notices.send(notice);
    }

    fn current_track(&self) -> Option<Arc<TrackInfo>> {
        match &self.mode {
            Mode::Queue => self.queue.current().cloned(),
            Mode::Follow { track, .. } | Mode::Connect { track } => track.clone(),
        }
    }

    fn set_follow_last_playing(&mut self, value: Option<bool>) {
        if let Mode::Follow { last_playing, .. } = &mut self.mode {
            *last_playing = value;
        }
    }

    /// Returns true if we were following.
    /// Back to the Discord queue from follow mode or Spotify Connect.
    /// Returns the name of the mode that was switched off.
    fn leave_special_mode(&mut self) -> Option<&'static str> {
        match self.mode {
            Mode::Queue => None,
            Mode::Connect { .. } => {
                self.leave_connect(true);
                Some("Spotify Connect")
            }
            Mode::Follow { .. } => {
                self.stop_decks();
                self.mode = Mode::Queue;
                self.status = Status::Idle;
                self.clock.hold_at(0);
                Some("Follow mode")
            }
        }
    }

    fn save_settings(&self) {
        let store = self.store.clone();
        let settings = self.settings;
        tokio::task::spawn_blocking(move || store.save_settings(&settings));
    }

    fn ensure_output_playing(&mut self) {
        if let Some(out) = &self.output
            && !self.output_playing
        {
            let _ = out.play();
            self.output_playing = true;
        }
    }

    fn pause_output(&mut self) {
        if let Some(out) = &self.output
            && self.output_playing
        {
            let _ = out.pause();
            self.output_playing = false;
        }
    }

    fn stop_decks(&mut self) {
        self.end_listen();
        self.fade = None;
        for (deck, pipe) in self.decks.iter().zip(&self.mixer.pipes) {
            deck.player.stop();
            pipe.flush();
        }
        self.mixer.solo(self.active);
    }

    /// Ends any crossfade immediately, keeping only the active deck.
    fn cancel_fade(&mut self) {
        if self.fade.take().is_some() {
            let other = 1 - self.active;
            self.decks[other].player.stop();
            self.mixer.pipes[other].flush();
        }
        self.mixer.solo(self.active);
    }

    /// A crossfade that hasn't reached the next song yet is aimed at a song
    /// that may no longer be next; drop it and let the tick start a new one.
    fn drop_pending_fade(&mut self) {
        if self.fade.is_some_and(|f| !f.switched) {
            self.cancel_fade();
        }
    }

    /// Loads a song on a deck and tells everyone it started. With `trim`,
    /// silence it opens with is skipped.
    fn start_track(
        &mut self,
        deck: usize,
        track: Arc<TrackInfo>,
        play: bool,
        position_ms: u32,
        trim: bool,
    ) {
        self.begin_listen(&track);
        if trim {
            self.mixer.pipes[deck].trim_leading_silence();
        }
        if play {
            self.pending_pause = None;
            self.mixer.fade_to(1.0, Duration::ZERO);
        }
        self.decks[deck].load(&track, play, position_ms);
        self.status = if play {
            Status::Loading
        } else {
            Status::Paused
        };
        self.clock.hold_at(position_ms);
        if play {
            self.ensure_output_playing();
        }
        self.notify(Notice::TrackStarted(track));
    }

    /// Cuts to queue entry `index` straight away.
    fn play_index(&mut self, index: usize) {
        let Some(track) = self.queue.get(index).cloned() else {
            return;
        };
        self.cancel_fade();
        self.mixer.pipes[self.active].flush();
        self.queue.set_current(index);
        let active = self.active;
        self.start_track(active, track, true, 0, true);
    }

    fn finish_queue(&mut self) {
        self.end_listen();
        self.status = Status::Idle;
        self.clock.hold_at(0);
        self.queue.finish();
        self.failures = 0;
        self.notify(Notice::QueueEnded);
    }

    fn pause(&mut self) -> bool {
        if !matches!(self.status, Status::Playing | Status::Loading) {
            return false;
        }
        self.cancel_fade();
        let position = self.clock.position();
        self.status = Status::Paused;
        self.clock.hold_at(position);
        // Fade out first; `on_tick` pauses the player once it's silent.
        self.mixer.fade_to(0.0, sound::SOFT_FADE);
        self.pending_pause = Some(Instant::now());
        self.notify(Notice::Changed);
        true
    }

    /// The fade-out of a soft pause is done: actually pause.
    fn apply_pending_pause(&mut self) {
        if self
            .pending_pause
            .is_none_or(|at| at.elapsed() < sound::SOFT_FADE + Duration::from_millis(20))
        {
            return;
        }
        self.pending_pause = None;
        if let Some(spirc) = self.connect_spirc() {
            // Through Spotify, so the app shows it paused too.
            let _ = spirc.pause();
        } else {
            self.decks[self.active].player.pause();
        }
    }

    fn resume(&mut self) -> bool {
        if self.status != Status::Paused {
            return false;
        }
        self.mixer.fade_to(1.0, sound::SOFT_FADE);
        if self.pending_pause.take().is_some() {
            // Still fading out; the player never actually paused.
            self.status = Status::Playing;
            self.clock.resume();
            self.notify(Notice::Changed);
            return true;
        }
        self.ensure_output_playing();
        if let Some(spirc) = self.connect_spirc() {
            let _ = spirc.play();
            self.status = Status::Playing;
            self.clock.resume();
            self.notify(Notice::Changed);
            return true;
        }
        self.decks[self.active].player.play();
        self.status = Status::Playing;
        self.clock.resume();
        self.notify(Notice::Changed);
        true
    }

    fn seek(&mut self, position_ms: u32) -> bool {
        let Some(track) = self.current_track() else {
            return false;
        };
        if self.status == Status::Idle {
            return false;
        }
        let position_ms = position_ms.min(track.duration_ms.saturating_sub(1_000));
        if let Some(spirc) = self.connect_spirc() {
            // The Seeked event that follows flushes the old audio.
            let _ = spirc.set_position_ms(position_ms);
        } else {
            self.cancel_fade();
            self.decks[self.active].player.seek(position_ms);
            self.mixer.pipes[self.active].flush();
        }
        if self.status == Status::Paused {
            self.clock.hold_at(position_ms);
        } else {
            self.clock.run_from(position_ms);
        }
        self.notify(Notice::Changed);
        true
    }

    fn begin_fade(&mut self, index: usize) {
        let Some(track) = self.queue.get(index).cloned() else {
            return;
        };
        let to = 1 - self.active;
        self.mixer.pipes[to].flush();
        // The blend waits for the incoming song's first sound.
        self.mixer.pipes[to].trim_leading_silence();
        self.decks[to].load(&track, true, 0);
        self.mixer.start_fade(
            self.active,
            to,
            Duration::from_secs(self.settings.crossfade_secs as u64),
        );
        self.fade = Some(Fade {
            from: self.active,
            to,
            index,
            switched: false,
        });
    }

    /// The incoming song of a crossfade becomes the current one.
    fn switch_to_fade_target(&mut self, fade: Fade, position_ms: u32) {
        if let Some(track) = self.queue.get(fade.index).cloned() {
            self.begin_listen(&track);
        }
        self.fade = Some(Fade {
            switched: true,
            ..fade
        });
        self.active = fade.to;
        self.queue.set_current(fade.index);
        self.status = Status::Playing;
        self.clock.run_from(position_ms);
        self.failures = 0;
        if let Some(track) = self.queue.get(fade.index).cloned() {
            self.notify(Notice::TrackStarted(track));
        }
    }

    /// Moves on after the active song ended or couldn't play.
    fn advance(&mut self, manual: bool) {
        if !manual && self.sleep.take_after_song() {
            self.stop_decks();
            self.status = Status::Idle;
            self.clock.hold_at(0);
            self.notify(Notice::SleepReached);
            return;
        }
        match self.mode {
            // The Spotify app moves its own player on.
            Mode::Connect { .. } => {}
            Mode::Follow { .. } => {
                // Your Spotify decides what's next; the poller will load it.
                self.status = Status::Idle;
                self.notify(Notice::Changed);
            }
            Mode::Queue => match self.queue.next_index(manual) {
                Some(index) => {
                    let Some(track) = self.queue.get(index).cloned() else {
                        return;
                    };
                    // Songs from one album keep their gaps, silence included.
                    let trim = !self
                        .queue
                        .current()
                        .is_some_and(|c| sound::same_album(c, &track));
                    self.queue.set_current(index);
                    let active = self.active;
                    // No flush: the last bit of the previous song is still
                    // in the pipe, and this keeps albums gapless.
                    self.start_track(active, track, true, 0, trim);
                }
                None => self.finish_queue(),
            },
        }
    }

    fn on_player_event(&mut self, deck: usize, event: PlayerEvent) {
        if let PlayerEvent::PlayRequestIdChanged { play_request_id } = event {
            let d = &mut self.decks[deck];
            if d.pending {
                d.pending = false;
                d.request = Some(play_request_id);
            }
            return;
        }
        let Some(request) = event.get_play_request_id() else {
            return;
        };
        if !self.decks[deck].is_current(request) {
            return;
        }

        match event {
            PlayerEvent::Playing { position_ms, .. } => {
                if let Some(fade) = self.fade
                    && deck == fade.to
                    && !fade.switched
                {
                    self.switch_to_fade_target(fade, position_ms);
                    return;
                }
                if deck != self.active || self.status == Status::Paused {
                    return;
                }
                self.status = Status::Playing;
                self.failures = 0;
                let lag = self.mixer.pipes[deck].buffered_ms();
                self.clock.run_from(position_ms.saturating_sub(lag));
                self.ensure_output_playing();
                self.notify(Notice::Changed);
            }
            PlayerEvent::Paused { .. } => {
                if deck == self.active && self.status == Status::Paused {
                    // librespot has stopped writing, so the output can stop
                    // reading without stalling it.
                    self.pause_output();
                }
            }
            PlayerEvent::Seeked { position_ms, .. }
            | PlayerEvent::PositionCorrection { position_ms, .. } => {
                if deck == self.active && self.status == Status::Playing {
                    self.clock.run_from(position_ms);
                }
            }
            PlayerEvent::TimeToPreloadNextTrack { .. } => {
                if deck == self.active
                    && matches!(self.mode, Mode::Queue)
                    && self.fade.is_none()
                    && let Some(next) = self
                        .queue
                        .next_index(false)
                        .and_then(|i| self.queue.get(i))
                        .cloned()
                    && !self.will_crossfade_into(&next)
                {
                    self.decks[deck].player.preload(next.uri());
                }
            }
            PlayerEvent::EndOfTrack { .. } => {
                // During a crossfade the incoming deck takes over on its own.
                if deck == self.active && self.fade.is_none() {
                    self.advance(false);
                }
            }
            PlayerEvent::Unavailable { .. } => self.on_unavailable(deck),
            _ => {}
        }
    }

    /// A song that has gone quiet in its last seconds is over: move on (or
    /// start the crossfade) instead of playing the silence.
    fn end_quiet_outro(&mut self) {
        if !matches!(self.mode, Mode::Queue) || self.fade.is_some() {
            return;
        }
        let Some(current) = self.queue.current().cloned() else {
            return;
        };
        let position = self.clock.position();
        if position < 30_000
            || position + OUTRO_WINDOW_MS < current.duration_ms
            || self.mixer.pipes[self.active].quiet_ms() < QUIET_OUTRO_MS
        {
            return;
        }
        tracing::debug!("skipping the quiet end of {}", current.title);
        let crossfade_to = self
            .queue
            .next_index(false)
            .filter(|&next| Some(next) != self.queue.current_index())
            .filter(|&next| {
                self.queue
                    .get(next)
                    .is_some_and(|t| self.will_crossfade_into(t))
            });
        match crossfade_to {
            Some(next) => self.begin_fade(next),
            None => {
                self.decks[self.active].player.stop();
                self.mixer.pipes[self.active].flush();
                self.advance(false);
            }
        }
    }

    fn on_unavailable(&mut self, deck: usize) {
        if let Some(fade) = self.fade
            && deck == fade.to
            && !fade.switched
        {
            self.cancel_fade();
            if let Some(track) = self.queue.remove_index(fade.index) {
                self.notify(Notice::Unavailable(track));
            }
            return;
        }
        if deck != self.active {
            return;
        }
        if let Some(track) = self.current_track() {
            self.notify(Notice::Unavailable(track));
        }
        self.failures += 1;
        if self.failures >= MAX_CONSECUTIVE_FAILURES {
            self.stop_decks();
            self.finish_queue();
            return;
        }
        self.mixer.pipes[deck].flush();
        self.advance(true);
    }

    /// Counts the time since the last tick if the music was playing, and
    /// saves it now and then.
    fn tick_played(&mut self) {
        let now = Instant::now();
        let playing = self.status == Status::Playing;
        let clock = &mut self.played_clock;
        if let Some(last) = clock.last.replace(now)
            && playing
        {
            // A stall in between isn't playing time.
            clock.ms += (now - last).min(Duration::from_secs(5)).as_millis() as u64;
        }
        let hour = crate::spotify::auth::now_unix() / 3600;
        if clock.hour == 0 {
            clock.hour = hour;
        }
        let due = hour != clock.hour
            || clock
                .saved
                .is_none_or(|at| at.elapsed() >= SAVE_PLAYED_EVERY);
        if clock.ms > 0 && due {
            let (store, hour, ms) = (self.store.clone(), clock.hour, clock.ms);
            tokio::task::spawn_blocking(move || store.add_played(hour, ms));
            clock.ms = 0;
            clock.saved = Some(now);
        }
        clock.hour = hour;
    }

    fn on_tick(&mut self) {
        self.tick_played();
        self.tick_listen();
        self.apply_pending_pause();
        self.tick_sleep();
        if self.status == Status::Playing {
            let trimmed = self.mixer.pipes[self.active].take_trimmed_ms();
            self.clock.skip(trimmed);
            self.end_quiet_outro();
        }

        if self.mixer.take_fade_finished()
            && let Some(fade) = self.fade.take()
        {
            self.decks[fade.from].player.stop();
            self.mixer.pipes[fade.from].flush();
            if !fade.switched {
                let elapsed = self.settings.crossfade_secs as u32 * 1000;
                self.switch_to_fade_target(fade, elapsed);
                self.fade = None;
            }
        }

        if matches!(self.mode, Mode::Queue)
            && self.status == Status::Playing
            && self.fade.is_none()
            && self.settings.crossfade_secs > 0
            && self.queue.repeat != Repeat::Track
            && let Some(current) = self.queue.current()
        {
            let crossfade_ms = self.settings.crossfade_secs as u32 * 1000;
            let position = self.clock.position();
            let remaining = current.duration_ms.saturating_sub(position);
            // Start a second early to cover the next song's load time.
            if current.duration_ms > crossfade_ms * 2
                && position > 1_000
                && remaining <= crossfade_ms + 1_000
                && let Some(next) = self.queue.next_index(false)
                && Some(next) != self.queue.current_index()
                && let Some(next_track) = self.queue.get(next).cloned()
                && self.will_crossfade_into(&next_track)
            {
                self.begin_fade(next);
            }
        }

        if self.status == Status::Idle && self.output_playing && self.mixer.is_silent() {
            self.pause_output();
        }
    }
}

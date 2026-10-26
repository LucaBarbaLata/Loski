//! The engine driven through whole listening sessions, with fake players
//! standing in for librespot. Events are fed in the way librespot sends
//! them, and the tests check what the engine asked the players to do.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use librespot_core::{Session, SessionConfig, SpotifyId, SpotifyUri};
use librespot_playback::player::PlayerEvent;
use parking_lot::{Mutex, MutexGuard};
use tokio::sync::mpsc;

use super::*;
use crate::{audio::Mixer, store::Store, track::TrackInfo};

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Load(u8, bool, u32),
    Preload(u8),
    Play,
    Pause,
    Stop,
    Seek(u32),
}

/// Songs in the tests are numbered; their ids are that number repeated.
fn number(uri: &SpotifyUri) -> u8 {
    match uri {
        SpotifyUri::Track { id } => id.to_raw()[0],
        _ => 0,
    }
}

#[derive(Default)]
struct FakePlayer {
    calls: Mutex<Vec<Call>>,
}

impl DeckPlayer for FakePlayer {
    fn load(&self, uri: SpotifyUri, play: bool, position_ms: u32) {
        self.calls
            .lock()
            .push(Call::Load(number(&uri), play, position_ms));
    }

    fn preload(&self, uri: SpotifyUri) {
        self.calls.lock().push(Call::Preload(number(&uri)));
    }

    fn play(&self) {
        self.calls.lock().push(Call::Play);
    }

    fn pause(&self) {
        self.calls.lock().push(Call::Pause);
    }

    fn stop(&self) {
        self.calls.lock().push(Call::Stop);
    }

    fn seek(&self, position_ms: u32) {
        self.calls.lock().push(Call::Seek(position_ms));
    }

    fn set_session(&self, _: Session) {}
}

fn song(n: u8, album: &str) -> TrackInfo {
    TrackInfo {
        id: SpotifyId::from_raw(&[n; 16]).unwrap(),
        title: format!("Song {n}"),
        artists: vec!["Artist".into()],
        album: album.into(),
        duration_ms: 200_000,
        cover_url: None,
        cover_thumb_url: None,
        explicit: false,
        year: None,
        from: None,
        requested_by: None,
    }
}

/// Each song on its own album, so crossfades apply between them.
fn songs(numbers: impl IntoIterator<Item = u8>) -> Vec<TrackInfo> {
    numbers
        .into_iter()
        .map(|n| song(n, &format!("Album {n}")))
        .collect()
}

struct Harness {
    engine: Engine,
    players: [Arc<FakePlayer>; 2],
    notices: mpsc::UnboundedReceiver<Notice>,
    store: Arc<Store>,
    dir: PathBuf,
    next_request: u64,
}

impl Harness {
    fn new() -> Self {
        static RUN: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "loski-engine-test-{}-{}",
            std::process::id(),
            RUN.fetch_add(1, Ordering::Relaxed)
        ));
        let store = Arc::new(Store::new(dir.clone()).unwrap());
        let (notices_tx, notices) = mpsc::unbounded_channel();
        let (deck_events, _) = mpsc::unbounded_channel();
        let players = [
            Arc::new(FakePlayer::default()),
            Arc::new(FakePlayer::default()),
        ];
        let mixer = Mixer::new();
        let inner = Inner {
            decks: players.clone().map(|player| Deck {
                player,
                request: None,
                pending: false,
            }),
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
            settings: Settings::default(),
            store: store.clone(),
            notices: notices_tx,
            connect: None,
            pending_pause: None,
            sleep: sound::Sleep::default(),
            session: Session::new(SessionConfig::default(), None),
            normalise: false,
            deck_events,
            listen: None,
            played: Vec::new(),
            connect_up_next: Vec::new(),
            audience: Vec::new(),
        };
        Self {
            engine: Engine {
                inner: parking_lot::Mutex::new(inner),
                mixer,
            },
            players,
            notices,
            store,
            dir,
            next_request: 1,
        }
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.engine.inner.lock()
    }

    /// What `deck` was asked to do since the last look.
    fn calls(&self, deck: usize) -> Vec<Call> {
        std::mem::take(&mut *self.players[deck].calls.lock())
    }

    /// librespot takes the latest load on `deck` and gives it a request id.
    fn accepted(&mut self, deck: usize) -> u64 {
        let id = self.next_request;
        self.next_request += 1;
        self.event(
            deck,
            PlayerEvent::PlayRequestIdChanged {
                play_request_id: id,
            },
        );
        id
    }

    /// The latest load on `deck` starts playing.
    fn playing(&mut self, deck: usize) -> u64 {
        let id = self.accepted(deck);
        self.event(
            deck,
            PlayerEvent::Playing {
                play_request_id: id,
                track_id: song(0, "").uri(),
                position_ms: 0,
            },
        );
        id
    }

    fn event(&self, deck: usize, event: PlayerEvent) {
        self.inner().on_player_event(deck, event);
    }

    fn ended(&self, deck: usize, request: u64) {
        self.event(
            deck,
            PlayerEvent::EndOfTrack {
                play_request_id: request,
                track_id: song(0, "").uri(),
            },
        );
    }

    fn notices(&mut self) -> Vec<Notice> {
        std::iter::from_fn(|| self.notices.try_recv().ok()).collect()
    }

    /// Titles of the songs announced as started since the last look.
    fn started(&mut self) -> Vec<String> {
        self.notices()
            .into_iter()
            .filter_map(|n| match n {
                Notice::TrackStarted(track) => Some(track.title.clone()),
                _ => None,
            })
            .collect()
    }

    fn current(&self) -> Option<String> {
        self.engine.snapshot().track.map(|t| t.title.clone())
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[tokio::test]
async fn plays_the_queue_through_back_to_back() {
    let mut h = Harness::new();
    let added = h.engine.enqueue(songs(1..=3), false);
    assert!(added.started);
    assert_eq!(h.calls(0), [Call::Load(1, true, 0)]);
    assert_eq!(h.engine.snapshot().status, Status::Loading);

    let first = h.playing(0);
    assert_eq!(h.engine.snapshot().status, Status::Playing);
    h.ended(0, first);
    // Straight onto the same deck with nothing stopped in between: gapless.
    assert_eq!(h.calls(0), [Call::Load(2, true, 0)]);
    assert!(h.calls(1).is_empty());

    let second = h.playing(0);
    h.ended(0, second);
    assert_eq!(h.calls(0), [Call::Load(3, true, 0)]);
    let third = h.playing(0);
    h.ended(0, third);
    assert!(h.calls(0).is_empty());
    assert_eq!(h.started(), ["Song 1", "Song 2", "Song 3"]);
    assert_eq!(h.engine.snapshot().status, Status::Idle);
    assert_eq!(h.current().as_deref(), Some("Song 3"));
    // The finished queue can be played again from the top.
    assert!(h.engine.replay());
    assert_eq!(h.calls(0), [Call::Load(1, true, 0)]);
}

#[tokio::test]
async fn events_from_a_skipped_song_are_ignored() {
    let mut h = Harness::new();
    h.engine.enqueue(songs(1..=3), false);
    let first = h.playing(0);
    h.calls(0);

    h.engine.skip(1);
    assert_eq!(h.calls(0), [Call::Load(2, true, 0)]);
    // Song 1's end arrives late, after the skip: it must not skip again.
    h.ended(0, first);
    assert!(h.calls(0).is_empty());
    assert_eq!(h.current().as_deref(), Some("Song 2"));

    let second = h.playing(0);
    h.ended(0, first);
    assert!(h.calls(0).is_empty());
    h.ended(0, second);
    assert_eq!(h.calls(0), [Call::Load(3, true, 0)]);
}

#[tokio::test]
async fn repeat_one_replays_until_you_skip() {
    let mut h = Harness::new();
    h.engine.enqueue(songs(1..=2), false);
    h.engine.set_repeat(Repeat::Track);
    h.calls(0);
    for _ in 0..3 {
        let request = h.playing(0);
        h.ended(0, request);
        assert_eq!(h.calls(0), [Call::Load(1, true, 0)]);
    }
    h.playing(0);
    h.engine.skip(1);
    assert_eq!(h.calls(0), [Call::Load(2, true, 0)]);
}

#[tokio::test]
async fn crossfade_blends_into_the_next_song_on_the_other_deck() {
    let mut h = Harness::new();
    h.engine.set_crossfade(6);
    h.engine.enqueue(songs(1..=2), false);
    h.playing(0);
    h.calls(0);
    h.started();

    // Nothing happens mid-song…
    h.inner().clock.run_from(100_000);
    h.inner().on_tick();
    assert!(h.calls(1).is_empty());
    // …but with the crossfade (plus a second to load) left, the next song
    // starts on the other deck.
    h.inner().clock.run_from(193_500);
    h.inner().on_tick();
    assert_eq!(h.calls(1), [Call::Load(2, true, 0)]);
    assert!(h.inner().fade.is_some());

    h.playing(1);
    assert_eq!(h.inner().active, 1);
    assert_eq!(h.current().as_deref(), Some("Song 2"));
    assert_eq!(h.started(), ["Song 2"]);
    // Song 1 finishing under the fade doesn't move the queue on again.
    h.ended(0, 1);
    assert!(h.calls(0).is_empty() && h.calls(1).is_empty());
}

#[tokio::test]
async fn albums_stay_gapless_even_with_crossfade_on() {
    let mut h = Harness::new();
    h.engine.set_crossfade(6);
    h.engine
        .enqueue(vec![song(1, "Discovery"), song(2, "Discovery")], false);
    let request = h.playing(0);
    h.calls(0);

    h.inner().clock.run_from(193_500);
    h.inner().on_tick();
    assert!(h.calls(1).is_empty());
    assert!(h.inner().fade.is_none());
    // librespot gets the next song early instead, for a seamless join.
    h.event(
        0,
        PlayerEvent::TimeToPreloadNextTrack {
            play_request_id: request,
            track_id: song(1, "").uri(),
        },
    );
    assert_eq!(h.calls(0), [Call::Preload(2)]);
}

#[tokio::test]
async fn a_quiet_ending_moves_on_to_the_next_song() {
    let mut h = Harness::new();
    h.engine.enqueue(songs(1..=2), false);
    h.playing(0);
    h.calls(0);
    h.started();

    // Quiet mid-song is just a quiet passage.
    h.inner().clock.run_from(100_000);
    h.inner().mixer.pipes[0].pretend_quiet_for(5_000);
    h.inner().on_tick();
    assert!(h.calls(0).is_empty());

    // Quiet in the last seconds means the song is over.
    h.inner().clock.run_from(188_000);
    h.inner().on_tick();
    assert_eq!(h.calls(0), [Call::Stop, Call::Load(2, true, 0)]);
    assert_eq!(h.started(), ["Song 2"]);
}

#[tokio::test]
async fn a_quiet_ending_starts_the_crossfade_early() {
    let mut h = Harness::new();
    h.engine.set_crossfade(6);
    h.engine.enqueue(songs(1..=2), false);
    h.playing(0);
    h.calls(0);
    h.inner().clock.run_from(185_000);
    h.inner().mixer.pipes[0].pretend_quiet_for(2_000);
    h.inner().on_tick();
    assert_eq!(h.calls(1), [Call::Load(2, true, 0)]);
    assert!(h.calls(0).is_empty());
}

#[tokio::test]
async fn songs_spotify_wont_play_are_skipped_and_reported() {
    let mut h = Harness::new();
    h.engine.enqueue(songs(1..=12), false);
    h.calls(0);
    let unavailable = |h: &mut Harness| {
        let request = h.accepted(0);
        h.event(
            0,
            PlayerEvent::Unavailable {
                play_request_id: request,
                track_id: song(0, "").uri(),
            },
        );
    };

    unavailable(&mut h);
    assert_eq!(h.calls(0), [Call::Load(2, true, 0)]);
    assert!(
        h.notices()
            .iter()
            .any(|n| matches!(n, Notice::Unavailable(t) if t.title == "Song 1"))
    );

    // Ten in a row and Loski gives up instead of burning through the queue.
    for _ in 0..9 {
        unavailable(&mut h);
    }
    assert_eq!(h.engine.snapshot().status, Status::Idle);
    assert!(h.notices().iter().any(|n| matches!(n, Notice::QueueEnded)));
    assert!(!h.calls(0).contains(&Call::Load(12, true, 0)));
}

#[tokio::test]
async fn sleep_after_this_song_stops_when_it_ends() {
    let mut h = Harness::new();
    h.engine.enqueue(songs(1..=2), false);
    let request = h.playing(0);
    assert!(h.engine.sleep_after_song());
    h.calls(0);
    h.notices();

    h.ended(0, request);
    let calls = h.calls(0);
    assert!(calls.contains(&Call::Stop));
    assert!(!calls.iter().any(|c| matches!(c, Call::Load(..))));
    assert!(
        h.notices()
            .iter()
            .any(|n| matches!(n, Notice::SleepReached))
    );
    assert_eq!(h.engine.snapshot().sleep, None);
}

#[tokio::test]
async fn pause_fades_out_before_the_player_stops() {
    let mut h = Harness::new();
    h.engine.enqueue(songs(1..=1), false);
    h.playing(0);
    h.calls(0);

    assert_eq!(h.engine.toggle_pause(), Some(true));
    h.inner().on_tick();
    assert!(
        h.calls(0).is_empty(),
        "the player keeps going while it fades"
    );
    std::thread::sleep(sound::SOFT_FADE + Duration::from_millis(50));
    h.inner().on_tick();
    assert_eq!(h.calls(0), [Call::Pause]);

    assert_eq!(h.engine.toggle_pause(), Some(false));
    assert_eq!(h.calls(0), [Call::Play]);
    // Pausing and resuming within the fade never touches the player.
    h.engine.pause();
    h.engine.resume();
    std::thread::sleep(sound::SOFT_FADE + Duration::from_millis(50));
    h.inner().on_tick();
    assert!(h.calls(0).is_empty());
    assert_eq!(h.engine.snapshot().status, Status::Playing);
}

#[tokio::test]
async fn history_keeps_songs_you_listened_to_and_drops_skips() {
    let mut h = Harness::new();
    h.engine.enqueue(songs(1..=3), false);
    let first = h.playing(0);
    // Pretend 45 seconds of it played.
    h.inner().listen.as_mut().unwrap().heard = Duration::from_secs(45);
    h.ended(0, first);
    // Song 2 is skipped straight away; song 3 then stops with the queue.
    h.playing(0);
    h.engine.skip(1);
    h.playing(0);
    h.engine.skip(1);

    // History is written in the background; give it a moment.
    for _ in 0..50 {
        if !h.store.recent_history(10).is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    let history = h.store.recent_history(10);
    assert_eq!(history.len(), 1, "{history:?}");
    assert_eq!(history[0].title, "Song 1");
    assert!(history[0].listened_ms.unwrap() >= 45_000);

    // The same song is kept for the goodbye recap, once.
    let played = h.engine.take_played();
    assert_eq!(played.len(), 1);
    assert_eq!(played[0].track.title, "Song 1");
    assert!(h.engine.take_played().is_empty());
}

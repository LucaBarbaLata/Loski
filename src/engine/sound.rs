//! Filters, the sleep timer, smarter crossfades and the normalisation switch.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use librespot_core::Session;
use librespot_playback::{
    mixer::NoOpVolume,
    player::{Player, PlayerEvent},
};
use tokio::sync::mpsc;

use super::{Deck, Engine, Inner, Mode, Notice, Repeat, Status, player_config};
use crate::{
    audio::{Mixer, PipeSink},
    dsp::Filter,
    track::TrackInfo,
};

/// How long a soft pause or resume takes.
pub(super) const SOFT_FADE: Duration = Duration::from_millis(120);
/// The sleep timer fades out over this long before leaving.
const SLEEP_FADE: Duration = Duration::from_secs(6);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SleepState {
    /// Leaving after this much more time.
    In(Duration),
    /// Leaving when the current song ends.
    AfterSong,
}

#[derive(Default)]
pub(super) struct Sleep {
    at: Option<Instant>,
    after_song: bool,
    fading: bool,
}

impl Sleep {
    pub(super) fn state(&self) -> Option<SleepState> {
        if self.after_song {
            Some(SleepState::AfterSong)
        } else {
            self.at
                .map(|at| SleepState::In(at.saturating_duration_since(Instant::now())))
        }
    }

    /// True (once) if the timer was waiting for the song to end.
    pub(super) fn take_after_song(&mut self) -> bool {
        std::mem::take(&mut self.after_song)
    }
}

/// A librespot player writing into `deck`'s pipe, its events forwarded to the engine.
pub(super) fn new_deck(
    session: &Session,
    normalise: bool,
    mixer: &Arc<Mixer>,
    deck: usize,
    events: &mpsc::UnboundedSender<(usize, PlayerEvent)>,
) -> Deck {
    let sink_mixer = mixer.clone();
    let player = Player::new(
        player_config(normalise),
        session.clone(),
        Box::new(NoOpVolume),
        move || Box::new(PipeSink::new(sink_mixer, deck)),
    );
    let mut channel = player.get_player_event_channel();
    let tx = events.clone();
    tokio::spawn(async move {
        while let Some(event) = channel.recv().await {
            if tx.send((deck, event)).is_err() {
                break;
            }
        }
    });
    Deck {
        player,
        request: None,
        pending: false,
    }
}

/// Consecutive songs from one album: blending them would ruin a gapless album.
pub(super) fn same_album(a: &TrackInfo, b: &TrackInfo) -> bool {
    !a.album.is_empty() && a.album == b.album && a.artists.first() == b.artists.first()
}

impl Inner {
    /// Whether moving to `next` should crossfade rather than play gapless.
    pub(super) fn will_crossfade_into(&self, next: &TrackInfo) -> bool {
        self.settings.crossfade_secs > 0
            && self.queue.repeat != Repeat::Track
            && !self.sleep.after_song
            && !self.queue.current().is_some_and(|c| same_album(c, next))
    }

    pub(super) fn tick_sleep(&mut self) {
        let Some(at) = self.sleep.at else { return };
        let left = at.saturating_duration_since(Instant::now());
        if left <= SLEEP_FADE && !self.sleep.fading {
            self.sleep.fading = true;
            self.mixer.fade_to(0.0, left.max(Duration::from_secs(1)));
        }
        if left.is_zero() {
            self.sleep = Sleep::default();
            self.notify(Notice::SleepReached);
        }
    }

    /// Back to plain sound when Loski leaves voice.
    pub(super) fn reset_sound(&mut self) {
        self.mixer.set_filter(Filter::Off);
        self.clock.set_rate(1.0);
        self.mixer.fade_to(1.0, Duration::ZERO);
        self.pending_pause = None;
        self.sleep = Sleep::default();
    }

    /// Speed filters would drift away from your Spotify in follow mode.
    pub(super) fn drop_speed_filter(&mut self) {
        if self.mixer.filter().speed() != 1.0 {
            self.mixer.set_filter(Filter::Off);
            self.clock.set_rate(1.0);
        }
    }
}

impl Engine {
    pub fn set_filter(&self, filter: Filter) -> Result<(), &'static str> {
        let mut g = self.inner.lock();
        if matches!(g.mode, Mode::Follow { .. }) && filter.speed() != 1.0 {
            return Err(
                "Speed filters don't work in follow mode; they'd drift away from your Spotify.",
            );
        }
        g.mixer.set_filter(filter);
        g.clock.set_rate(filter.speed());
        g.notify(Notice::Changed);
        Ok(())
    }

    pub fn sleep_in(&self, after: Duration) {
        let mut g = self.inner.lock();
        g.sleep = Sleep {
            at: Some(Instant::now() + after),
            after_song: false,
            fading: false,
        };
        g.mixer.fade_to(1.0, SOFT_FADE);
        g.notify(Notice::Changed);
    }

    /// Returns false if nothing is playing to wait for.
    pub fn sleep_after_song(&self) -> bool {
        let mut g = self.inner.lock();
        if g.status == Status::Idle {
            return false;
        }
        g.sleep = Sleep {
            at: None,
            after_song: true,
            fading: false,
        };
        // A crossfade into the next song may already be on its way.
        g.drop_pending_fade();
        g.notify(Notice::Changed);
        true
    }

    /// Returns false if no timer was set.
    pub fn cancel_sleep(&self) -> bool {
        let mut g = self.inner.lock();
        let was_set = g.sleep.state().is_some();
        if g.sleep.fading {
            g.mixer.fade_to(1.0, SOFT_FADE);
        }
        g.sleep = Sleep::default();
        g.notify(Notice::Changed);
        was_set
    }

    pub fn normalise(&self) -> bool {
        self.inner.lock().normalise
    }

    /// librespot only reads the normalisation setting when a player is
    /// created, so the queue's players are rebuilt and the song picks up
    /// where it was. Returns false if nothing changed.
    pub fn set_normalise(&self, on: bool) -> bool {
        let old = {
            let mut g = self.inner.lock();
            if g.normalise == on {
                return false;
            }
            g.normalise = on;
            g.settings.normalise = Some(on);
            g.save_settings();

            let resume = match g.mode {
                Mode::Queue if g.status != Status::Idle => g
                    .queue
                    .current()
                    .cloned()
                    .map(|t| (t, g.clock.position(), g.status != Status::Paused)),
                _ => None,
            };
            g.fade = None;
            for deck in &g.decks {
                deck.player.stop();
            }
            let fresh =
                [0usize, 1].map(|deck| new_deck(&g.session, on, &g.mixer, deck, &g.deck_events));
            let old = std::mem::replace(&mut g.decks, fresh);
            for pipe in &g.mixer.pipes[..2] {
                pipe.flush();
            }
            if matches!(g.mode, Mode::Queue) {
                g.mixer.solo(g.active);
            }
            if let Some((track, position, playing)) = resume {
                let active = g.active;
                g.decks[active].load(&track, playing, position);
                g.status = if playing {
                    Status::Loading
                } else {
                    Status::Paused
                };
                g.clock.hold_at(position);
            }
            g.notify(Notice::Changed);
            old
        };
        // Dropping a player joins its thread; keep that off the async workers.
        tokio::task::spawn_blocking(move || drop(old));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_core::SpotifyId;

    fn track(album: &str, artist: &str) -> TrackInfo {
        TrackInfo {
            id: SpotifyId::from_raw(&[7; 16]).unwrap(),
            title: "t".into(),
            artists: vec![artist.into()],
            album: album.into(),
            duration_ms: 1000,
            cover_url: None,
            cover_thumb_url: None,
            explicit: false,
            year: None,
            from: None,
        }
    }

    #[test]
    fn albums_stay_gapless_but_other_songs_blend() {
        assert!(same_album(
            &track("Discovery", "Daft Punk"),
            &track("Discovery", "Daft Punk")
        ));
        assert!(!same_album(
            &track("Discovery", "Daft Punk"),
            &track("Homework", "Daft Punk")
        ));
        assert!(!same_album(
            &track("Greatest Hits", "Queen"),
            &track("Greatest Hits", "ABBA")
        ));
        assert!(!same_album(&track("", "X"), &track("", "X")));
    }

    #[test]
    fn sleep_state_reports_what_is_set() {
        let mut sleep = Sleep::default();
        assert_eq!(sleep.state(), None);
        sleep.after_song = true;
        assert_eq!(sleep.state(), Some(SleepState::AfterSong));
        assert!(sleep.take_after_song());
        assert!(!sleep.take_after_song());
        sleep.at = Some(Instant::now() + Duration::from_secs(600));
        assert!(matches!(sleep.state(), Some(SleepState::In(d)) if d > Duration::from_secs(590)));
    }
}

//! Spotify Connect mode: when you pick Loski as a device in the Spotify app,
//! librespot's Spirc drives its own player, which writes into the mixer's
//! third pipe. The engine mirrors that player's state for the panel and
//! routes Discord's controls back through Spotify.

use std::sync::Arc;

use librespot_connect::Spirc;
use librespot_core::SpotifyUri;
use librespot_metadata::audio::{AudioItem, UniqueFields};
use librespot_playback::player::{Player, PlayerEvent, PlayerEventChannel};

use super::{Engine, Inner, Mode, Notice, Status};
use crate::{audio::CONNECT_DECK, track::TrackInfo};

pub(super) struct ConnectHandle {
    player: Arc<Player>,
    spirc: Arc<Spirc>,
}

impl Engine {
    /// Hooks up a running Spotify Connect device.
    pub fn attach_connect(
        self: &Arc<Self>,
        player: Arc<Player>,
        spirc: Arc<Spirc>,
        mut events: PlayerEventChannel,
    ) {
        self.inner.lock().connect = Some(ConnectHandle { player, spirc });
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                let Some(engine) = weak.upgrade() else { break };
                engine.inner.lock().on_connect_event(event);
            }
        });
    }

    /// The Connect device went away (its connection dropped).
    pub fn detach_connect(&self) {
        let mut g = self.inner.lock();
        g.leave_connect(false);
        g.connect = None;
    }

    /// Pauses Spotify Connect playback, e.g. when there's no voice channel to play into.
    pub fn connect_pause(&self) {
        if let Some(spirc) = self.inner.lock().connect_spirc() {
            let _ = spirc.pause();
        }
    }
}

impl Inner {
    /// The Spirc handle, only while the Spotify app is in control.
    pub(super) fn connect_spirc(&self) -> Option<Arc<Spirc>> {
        match self.mode {
            Mode::Connect { .. } => self.connect.as_ref().map(|c| c.spirc.clone()),
            _ => None,
        }
    }

    fn enter_connect(&mut self) {
        if matches!(self.mode, Mode::Connect { .. }) {
            return;
        }
        // The Discord queue is kept; it just stops playing.
        self.stop_decks();
        self.mode = Mode::Connect { track: None };
        self.status = Status::Loading;
        self.clock.hold_at(0);
        self.mixer.solo(CONNECT_DECK);
        self.notify(Notice::ConnectStarted);
    }

    /// Hands control back to the Discord queue. With `tell_spotify`, the
    /// Spotify app is told too (playback pauses there).
    pub(super) fn leave_connect(&mut self, tell_spotify: bool) {
        if !matches!(self.mode, Mode::Connect { .. }) {
            return;
        }
        self.end_listen();
        if let Some(connect) = &self.connect {
            if tell_spotify {
                let _ = connect.spirc.disconnect(true);
            }
            connect.player.stop();
        }
        self.mixer.pipes[CONNECT_DECK].flush();
        self.mixer.solo(self.active);
        self.mode = Mode::Queue;
        self.status = Status::Idle;
        self.clock.hold_at(0);
        self.notify(Notice::ConnectEnded);
    }

    pub(super) fn on_connect_event(&mut self, event: PlayerEvent) {
        match event {
            PlayerEvent::SessionConnected { .. } => self.enter_connect(),
            PlayerEvent::SessionDisconnected { .. } => self.leave_connect(false),
            _ if !matches!(self.mode, Mode::Connect { .. }) => {}
            PlayerEvent::TrackChanged { audio_item } => {
                let had_song = matches!(self.mode, Mode::Connect { track: Some(_) });
                if had_song && self.sleep.take_after_song() {
                    // The sleep timer was waiting for that song to end.
                    self.notify(Notice::SleepReached);
                    return;
                }
                if let Some(track) = track_from_audio_item(&audio_item) {
                    let track = Arc::new(track);
                    self.mode = Mode::Connect {
                        track: Some(track.clone()),
                    };
                    self.begin_listen(&track);
                    self.notify(Notice::TrackStarted(track));
                }
            }
            PlayerEvent::Loading { .. } => self.status = Status::Loading,
            PlayerEvent::Playing { position_ms, .. } => {
                self.status = Status::Playing;
                let lag = self.mixer.pipes[CONNECT_DECK].buffered_ms();
                self.clock.run_from(position_ms.saturating_sub(lag));
                if self.output.is_none() {
                    // Pressed play in the app while Loski isn't in voice:
                    // the Discord side joins you again.
                    self.notify(Notice::ConnectStarted);
                }
                self.ensure_output_playing();
                self.notify(Notice::Changed);
            }
            PlayerEvent::Paused { position_ms, .. } => {
                self.status = Status::Paused;
                self.clock.hold_at(position_ms);
                self.pause_output();
                self.notify(Notice::Changed);
            }
            PlayerEvent::Seeked { position_ms, .. } => {
                // A seek from the app: drop the audio decoded before it.
                self.mixer.pipes[CONNECT_DECK].flush();
                if self.status == Status::Playing {
                    self.clock.run_from(position_ms);
                } else {
                    self.clock.hold_at(position_ms);
                }
                self.notify(Notice::Changed);
            }
            PlayerEvent::PositionCorrection { position_ms, .. }
                if self.status == Status::Playing =>
            {
                self.clock.run_from(position_ms);
            }
            PlayerEvent::Stopped { .. } => {
                self.end_listen();
                self.status = Status::Idle;
                self.clock.hold_at(0);
                self.notify(Notice::Changed);
            }
            _ => {}
        }
    }
}

/// librespot's description of the song the Spotify app picked.
fn track_from_audio_item(item: &AudioItem) -> Option<TrackInfo> {
    let SpotifyUri::Track { id } = item.track_id else {
        return None;
    };
    let (artists, album) = match &item.unique_fields {
        UniqueFields::Track { artists, album, .. } => (
            artists.iter().map(|a| a.name.clone()).collect(),
            album.clone(),
        ),
        _ => return None,
    };
    let mut covers: Vec<_> = item
        .covers
        .iter()
        .map(|c| (c.width, c.url.clone()))
        .collect();
    covers.sort_by_key(|(w, _)| *w);
    Some(TrackInfo {
        id,
        title: item.name.clone(),
        artists,
        album,
        duration_ms: item.duration_ms,
        cover_url: covers.last().map(|(_, u)| u.clone()),
        cover_thumb_url: covers.first().map(|(_, u)| u.clone()),
        explicit: item.is_explicit,
        year: None,
        from: None,
    })
}

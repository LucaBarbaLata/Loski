//! What you actually listened to, for /history and /stats. A song is filed
//! when it ends or something else starts, with the time it really played.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use super::{Inner, Status};
use crate::{spotify::auth::now_unix, store::HistoryEntry, track::TrackInfo};

/// Spotify counts a play after 30 seconds, and so does Loski.
const COUNTS_AFTER: Duration = Duration::from_secs(30);

/// A song that counted as played, kept until Loski leaves voice for the
/// goodbye recap.
#[derive(Debug, Clone)]
pub struct Played {
    pub track: Arc<TrackInfo>,
    pub started_at: i64,
    pub listened_ms: u64,
}

pub(super) struct Listen {
    track: Arc<TrackInfo>,
    started_at: i64,
    pub(super) heard: Duration,
    last: Instant,
}

/// Whether `heard` of a song counts as having played it.
fn counts(heard: Duration, duration_ms: u32) -> bool {
    // A very short song counts once half of it has played.
    let needed = COUNTS_AFTER.min(Duration::from_millis(duration_ms as u64 / 2));
    !heard.is_zero() && heard >= needed
}

impl Inner {
    /// Starts timing `track`, filing whatever was playing before it.
    pub(super) fn begin_listen(&mut self, track: &Arc<TrackInfo>) {
        self.end_listen();
        self.listen = Some(Listen {
            track: track.clone(),
            started_at: now_unix(),
            heard: Duration::ZERO,
            last: Instant::now(),
        });
    }

    /// Adds the time since the last call, if the music was playing.
    pub(super) fn tick_listen(&mut self) {
        let playing = self.status == Status::Playing;
        if let Some(listen) = &mut self.listen {
            let now = Instant::now();
            if playing {
                listen.heard += now - listen.last;
            }
            listen.last = now;
        }
    }

    /// Files the current song in the history if it played long enough.
    pub(super) fn end_listen(&mut self) {
        self.tick_listen();
        let Some(listen) = self.listen.take() else {
            return;
        };
        if !counts(listen.heard, listen.track.duration_ms) {
            return;
        }
        let track = &listen.track;
        self.played.push(Played {
            track: track.clone(),
            started_at: listen.started_at,
            listened_ms: listen.heard.as_millis() as u64,
        });
        let entry = HistoryEntry {
            played_at: listen.started_at,
            uri: track.spotify_uri(),
            title: track.title.clone(),
            artists: track.artists_joined(),
            artist_list: track.artists.clone(),
            cover: track.cover_thumb_url.clone(),
            listened_ms: Some(listen.heard.as_millis() as u64),
        };
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || store.append_history(&entry));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plays_count_after_thirty_seconds() {
        let secs = Duration::from_secs;
        assert!(!counts(secs(29), 200_000));
        assert!(counts(secs(30), 200_000));
        // A 20 second song counts after 10 seconds.
        assert!(!counts(secs(9), 20_000));
        assert!(counts(secs(10), 20_000));
        assert!(!counts(Duration::ZERO, 0));
    }
}

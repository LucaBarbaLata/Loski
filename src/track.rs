use std::sync::Arc;

use librespot_core::{SpotifyId, SpotifyUri};

#[derive(Debug, Clone)]
pub struct TrackInfo {
    pub id: SpotifyId,
    pub title: String,
    pub artists: Vec<String>,
    pub album: String,
    pub duration_ms: u32,
    /// Largest cover, for display.
    pub cover_url: Option<String>,
    /// Smallest cover, cheap to download for picking an embed colour.
    pub cover_thumb_url: Option<String>,
    pub explicit: bool,
    /// Release year of the album, when Spotify knows it.
    pub year: Option<i32>,
    /// What this song was queued from, like "the album **Discovery**", for
    /// the panel's "playing from" line.
    pub from: Option<Arc<str>>,
}

impl TrackInfo {
    pub fn uri(&self) -> SpotifyUri {
        SpotifyUri::Track { id: self.id }
    }

    pub fn base62(&self) -> String {
        self.id.to_base62().unwrap_or_default()
    }

    pub fn spotify_uri(&self) -> String {
        format!("spotify:track:{}", self.base62())
    }

    pub fn url(&self) -> String {
        format!("https://open.spotify.com/track/{}", self.base62())
    }

    pub fn artists_joined(&self) -> String {
        if self.artists.is_empty() {
            "Unknown artist".to_string()
        } else {
            self.artists.join(", ")
        }
    }

    /// "Title — Artist", the way it reads in a status line.
    pub fn display(&self) -> String {
        format!("{} — {}", self.title, self.artists_joined())
    }
}

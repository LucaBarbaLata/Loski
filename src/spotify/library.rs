//! Your Spotify library and stations, read through librespot's own
//! connection so no extra Web API permissions are needed.

use anyhow::{Context as _, Result, anyhow};
use librespot_core::{SpotifyId, SpotifyUri};
use librespot_protocol::{
    autoplay_context_request::AutoplayContextRequest, context::Context,
    playlist4_external::SelectedListContent,
};
use protobuf::Message;

use super::{Resolved, Spotify};
use crate::track::TrackInfo;

/// Songs in a radio station.
const RADIO_SIZE: usize = 50;
/// Liked Songs read at most; enough to shuffle from without stalling.
const LIBRARY_LIMIT: usize = 5_000;

pub struct PlaylistRef {
    pub id: SpotifyId,
    pub name: String,
    pub tracks: Option<u32>,
}

fn track_uris_from_json(value: &serde_json::Value, out: &mut Vec<String>) {
    if let Some(tracks) = value.get("tracks").and_then(|t| t.as_array()) {
        for track in tracks {
            if let Some(uri) = track.get("uri").and_then(|u| u.as_str())
                && uri.starts_with("spotify:track:")
            {
                out.push(uri.to_string());
            }
        }
    }
}

fn to_track_uris(uris: impl IntoIterator<Item = String>, limit: usize) -> Vec<SpotifyUri> {
    let mut seen = std::collections::HashSet::new();
    uris.into_iter()
        .filter(|u| seen.insert(u.clone()))
        .filter_map(|u| SpotifyUri::from_uri(&u).ok())
        .filter(|u| matches!(u, SpotifyUri::Track { .. }))
        .take(limit)
        .collect()
}

impl Spotify {
    /// Reads every track of a context, following its pages.
    async fn context_track_uris(&self, uri: &str, limit: usize) -> Result<Vec<String>> {
        let session = self.session();
        let spclient = session.spclient();
        let context: Context = spclient
            .get_context(uri)
            .await
            .map_err(|e| anyhow!("Spotify couldn't open {uri}: {e}"))?;

        let mut uris = Vec::new();
        let mut pending = Vec::new();
        for page in &context.pages {
            for track in &page.tracks {
                let uri = track.uri();
                if uri.starts_with("spotify:track:") {
                    uris.push(uri.to_string());
                }
            }
            if page.tracks.is_empty() && !page.page_url().is_empty() {
                pending.push(page.page_url().to_string());
            }
            if !page.next_page_url().is_empty() {
                pending.push(page.next_page_url().to_string());
            }
        }

        // Further pages come back as JSON pages of the same shape.
        while let Some(url) = pending.pop() {
            if uris.len() >= limit {
                break;
            }
            let Ok(bytes) = spclient.get_next_page(&url).await else {
                continue;
            };
            let Ok(page) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                continue;
            };
            track_uris_from_json(&page, &mut uris);
            if let Some(next) = page.get("next_page_url").and_then(|n| n.as_str())
                && !next.is_empty()
            {
                pending.push(next.to_string());
            }
        }
        Ok(uris)
    }

    /// Spotify's radio for a song: similar songs, like "Go to song radio".
    pub async fn radio(&self, seed: &TrackInfo) -> Result<Resolved> {
        let session = self.session();
        let seed_uri = seed.spotify_uri();
        let request = AutoplayContextRequest {
            context_uri: Some(seed_uri.clone()),
            recent_track_uri: vec![seed_uri.clone()],
            ..Default::default()
        };

        let mut uris: Vec<String> = match session.spclient().get_autoplay_context(&request).await {
            Ok(context) => context
                .pages
                .iter()
                .flat_map(|p| p.tracks.iter())
                .map(|t| t.uri().to_string())
                .collect(),
            Err(e) => {
                tracing::debug!("autoplay station failed: {e}");
                Vec::new()
            }
        };
        if uris.len() < 5 {
            let bytes = session
                .spclient()
                .get_apollo_station(
                    "stations",
                    &seed_uri,
                    Some(RADIO_SIZE),
                    vec![seed.id],
                    false,
                )
                .await
                .map_err(|e| anyhow!("Spotify has no radio for that song: {e}"))?;
            let json: serde_json::Value =
                serde_json::from_slice(&bytes).context("unexpected radio response")?;
            track_uris_from_json(&json, &mut uris);
        }
        uris.retain(|u| *u != seed_uri);
        let tracks = to_track_uris(uris, RADIO_SIZE);
        if tracks.is_empty() {
            return Err(anyhow!("Spotify has no radio for that song"));
        }
        Ok(self
            .load_tracks(
                tracks,
                format!("**{}** radio", crate::util::escape_md(&seed.title)),
            )
            .await)
    }

    /// Your Liked Songs, newest first, optionally a random selection of them.
    pub async fn liked_songs(&self, shuffle: bool) -> Result<Resolved> {
        let username = self.session().username();
        let mut uris = self
            .context_track_uris(
                &format!("spotify:user:{username}:collection"),
                LIBRARY_LIMIT,
            )
            .await?;
        if uris.is_empty() {
            return Err(anyhow!("your Liked Songs are empty"));
        }
        let total = uris.len();
        if shuffle {
            use rand::seq::SliceRandom;
            uris.shuffle(&mut rand::rng());
        }
        let tracks = to_track_uris(uris, super::MAX_TRACKS_PER_REQUEST);
        let mut resolved = self
            .load_tracks(tracks, "your **Liked Songs**".to_string())
            .await;
        resolved.truncated = total > super::MAX_TRACKS_PER_REQUEST;
        Ok(resolved)
    }

    /// Your playlists, in the order they appear in your Spotify sidebar.
    pub async fn my_playlists(&self) -> Result<Vec<PlaylistRef>> {
        let bytes = self
            .session()
            .spclient()
            .get_rootlist(0, Some(500))
            .await
            .map_err(|e| anyhow!("couldn't load your playlists: {e}"))?;
        let list =
            SelectedListContent::parse_from_bytes(&bytes).context("unexpected playlist list")?;
        let contents = list.contents.get_or_default();
        Ok(contents
            .items
            .iter()
            .zip(contents.meta_items.iter())
            .filter_map(|(item, meta)| {
                // Folders show up as start-group/end-group markers.
                let id = item.uri().strip_prefix("spotify:playlist:")?;
                let name = meta.attributes.name().trim();
                Some(PlaylistRef {
                    id: SpotifyId::from_base62(id).ok()?,
                    name: if name.is_empty() {
                        "Untitled playlist".to_string()
                    } else {
                        name.to_string()
                    },
                    tracks: meta.length.map(|l| l.max(0) as u32),
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_track_uris_from_station_json() {
        let json = serde_json::json!({
            "tracks": [
                {"uri": "spotify:track:4uLU6hMCjMI75M1A2tKUQC"},
                {"uri": "spotify:episode:512ojhOuo1ktJprKbVcKyQ"},
                {"uri": "spotify:track:4uLU6hMCjMI75M1A2tKUQC"},
                {"uri": "spotify:track:7ouMYWpwJ422jRcDASZB7P"}
            ]
        });
        let mut uris = Vec::new();
        track_uris_from_json(&json, &mut uris);
        assert_eq!(uris.len(), 3);
        let tracks = to_track_uris(uris, 10);
        assert_eq!(tracks.len(), 2, "duplicates and non-tracks are dropped");
    }
}

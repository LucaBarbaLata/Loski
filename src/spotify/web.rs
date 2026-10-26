//! Spotify Web API calls made with your own developer app: search and
//! "what am I listening to right now" for follow mode.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use librespot_core::SpotifyId;
use serde::Deserialize;
use tokio::sync::Mutex;

use super::auth::{self, WebToken};
use crate::{config::Config, store::write_atomic, track::TrackInfo};

const API: &str = "https://api.spotify.com/v1";

pub struct WebApi {
    cfg: Arc<Config>,
    http: reqwest::Client,
    token: Mutex<WebToken>,
    token_path: PathBuf,
    /// Spotify refused to renew the login: it was revoked or expired for good.
    signed_out: AtomicBool,
}

/// What the owner's Spotify is doing right now.
pub struct NowListening {
    pub track: TrackInfo,
    pub is_playing: bool,
    pub progress_ms: u32,
}

impl WebApi {
    pub fn token_path(cfg: &Config) -> PathBuf {
        cfg.data_dir.join("web_token.json")
    }

    pub fn load_token(cfg: &Config) -> Option<WebToken> {
        let text = std::fs::read_to_string(Self::token_path(cfg)).ok()?;
        serde_json::from_str(&text).ok()
    }

    pub fn save_token(cfg: &Config, token: &WebToken) -> Result<()> {
        let json = serde_json::to_string_pretty(token)?;
        write_atomic(&Self::token_path(cfg), json.as_bytes())
            .context("could not save Spotify token")
    }

    pub fn new(cfg: Arc<Config>, http: reqwest::Client, token: WebToken) -> Self {
        let token_path = Self::token_path(&cfg);
        Self {
            cfg,
            http,
            token: Mutex::new(token),
            token_path,
            signed_out: AtomicBool::new(false),
        }
    }

    /// Search and follow mode need you to log in again.
    pub fn is_signed_out(&self) -> bool {
        self.signed_out.load(Ordering::Relaxed)
    }

    /// A fresh login from the login page replaces the dead one.
    pub async fn replace_token(&self, token: WebToken) {
        if let Ok(json) = serde_json::to_string_pretty(&token) {
            let _ = write_atomic(&self.token_path, json.as_bytes());
        }
        *self.token.lock().await = token;
        self.signed_out.store(false, Ordering::Relaxed);
    }

    async fn access_token(&self) -> Result<String> {
        let mut token = self.token.lock().await;
        if token.expires_at - auth::now_unix() < 60 {
            let fresh =
                match auth::refresh_web_token(&self.cfg, &self.http, &token.refresh_token).await {
                    Ok(fresh) => fresh,
                    Err(e) => {
                        // Spotify says `invalid_grant` once a login is gone for good;
                        // anything else (no network, say) is worth retrying.
                        if format!("{e:#}").contains("invalid_grant") {
                            self.signed_out.store(true, Ordering::Relaxed);
                        }
                        return Err(e.context("could not refresh the Spotify Web API token"));
                    }
                };
            if let Ok(json) = serde_json::to_string_pretty(&fresh) {
                let _ = write_atomic(&self.token_path, json.as_bytes());
            }
            *token = fresh;
        }
        Ok(token.access_token.clone())
    }

    async fn get(&self, url: &str, query: &[(&str, &str)]) -> Result<Option<String>> {
        let token = self.access_token().await?;
        let resp = self
            .http
            .get(url)
            .bearer_auth(token)
            .query(query)
            .timeout(Duration::from_secs(8))
            .send()
            .await?;
        let status = resp.status();
        if status == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let wait = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("a few");
            bail!("Spotify is rate limiting requests, try again in {wait} seconds");
        }
        let body = resp.text().await?;
        if !status.is_success() {
            bail!("Spotify Web API returned {status}: {body}");
        }
        Ok(Some(body))
    }

    pub async fn search_tracks(&self, query: &str, limit: u8) -> Result<Vec<TrackInfo>> {
        let limit = limit.clamp(1, 10).to_string();
        let body = self
            .get(
                &format!("{API}/search"),
                &[("q", query), ("type", "track"), ("limit", &limit)],
            )
            .await?
            .ok_or_else(|| anyhow!("empty search response"))?;
        let parsed: SearchResponse =
            serde_json::from_str(&body).context("unexpected search response")?;
        Ok(parsed
            .tracks
            .items
            .into_iter()
            .flatten()
            .filter_map(ApiTrack::into_track)
            .collect())
    }

    /// Searches one kind of thing. Songs come back with full details; albums,
    /// playlists and artists as items to be loaded when played.
    pub async fn search(
        &self,
        query: &str,
        kind: SearchKind,
        limit: u8,
    ) -> Result<Vec<SearchItem>> {
        if kind == SearchKind::Songs {
            return Ok(self
                .search_tracks(query, limit)
                .await?
                .into_iter()
                .map(SearchItem::from_track)
                .collect());
        }
        let limit = limit.clamp(1, 10).to_string();
        let body = self
            .get(
                &format!("{API}/search"),
                &[("q", query), ("type", kind.api_type()), ("limit", &limit)],
            )
            .await?
            .ok_or_else(|| anyhow!("empty search response"))?;
        let json: serde_json::Value =
            serde_json::from_str(&body).context("unexpected search response")?;
        let items = json
            .get(format!("{}s", kind.api_type()))
            .and_then(|k| k.get("items"))
            .and_then(|i| i.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(items
            .iter()
            .filter(|i| !i.is_null())
            .filter_map(|i| SearchItem::from_json(kind, i))
            .collect())
    }

    /// Names of your Spotify Connect devices that are online right now.
    pub async fn device_names(&self) -> Result<Vec<String>> {
        let body = self
            .get(&format!("{API}/me/player/devices"), &[])
            .await?
            .unwrap_or_default();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        Ok(json
            .get("devices")
            .and_then(|d| d.as_array())
            .map(|d| {
                d.iter()
                    .filter_map(|x| x.get("name").and_then(|n| n.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Your profile name, as the Spotify app shows it.
    pub async fn display_name(&self) -> Result<Option<String>> {
        let body = self
            .get(&format!("{API}/me"), &[])
            .await?
            .unwrap_or_default();
        let json: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
        Ok(json
            .get("display_name")
            .and_then(|n| n.as_str())
            .filter(|n| !n.is_empty())
            .map(str::to_string))
    }

    /// What your Spotify has coming up next (the app's queue).
    pub async fn queue(&self) -> Result<Vec<TrackInfo>> {
        let Some(body) = self.get(&format!("{API}/me/player/queue"), &[]).await? else {
            return Ok(Vec::new());
        };
        #[derive(Deserialize)]
        struct Queue {
            #[serde(default)]
            queue: Vec<serde_json::Value>,
        }
        let parsed: Queue = serde_json::from_str(&body).context("unexpected queue response")?;
        // Podcast episodes and anything else that isn't a song are left out.
        Ok(parsed
            .queue
            .into_iter()
            .filter_map(|item| serde_json::from_value::<ApiTrack>(item).ok())
            .filter_map(ApiTrack::into_track)
            .take(5)
            .collect())
    }

    pub async fn now_listening(&self) -> Result<Option<NowListening>> {
        let Some(body) = self
            .get(
                &format!("{API}/me/player/currently-playing"),
                &[("market", "from_token")],
            )
            .await?
        else {
            return Ok(None);
        };
        let parsed: CurrentlyPlaying =
            serde_json::from_str(&body).context("unexpected player response")?;
        if parsed.currently_playing_type.as_deref() != Some("track") {
            return Ok(None);
        }
        let Some(track) = parsed.item.and_then(ApiTrack::into_track) else {
            return Ok(None);
        };
        Ok(Some(NowListening {
            track,
            is_playing: parsed.is_playing,
            progress_ms: parsed.progress_ms.unwrap_or(0),
        }))
    }
}

#[derive(Deserialize)]
struct SearchResponse {
    tracks: Paging,
}

#[derive(Deserialize)]
struct Paging {
    #[serde(default)]
    items: Vec<Option<ApiTrack>>,
}

#[derive(Deserialize)]
struct CurrentlyPlaying {
    #[serde(default)]
    is_playing: bool,
    progress_ms: Option<u32>,
    currently_playing_type: Option<String>,
    item: Option<ApiTrack>,
}

#[derive(Deserialize)]
struct ApiTrack {
    id: Option<String>,
    name: String,
    #[serde(default)]
    duration_ms: u32,
    #[serde(default)]
    explicit: bool,
    #[serde(default)]
    is_local: bool,
    #[serde(default)]
    artists: Vec<ApiArtist>,
    album: Option<ApiAlbum>,
}

#[derive(Deserialize)]
struct ApiArtist {
    name: String,
}

#[derive(Deserialize)]
struct ApiAlbum {
    name: String,
    #[serde(default)]
    images: Vec<ApiImage>,
    /// "2011", "2011-10" or "2011-10-14".
    release_date: Option<String>,
}

#[derive(Deserialize)]
struct ApiImage {
    url: String,
    width: Option<u32>,
}

impl ApiTrack {
    fn into_track(self) -> Option<TrackInfo> {
        if self.is_local {
            return None;
        }
        let id = SpotifyId::from_base62(self.id.as_deref()?).ok()?;
        let (album, mut images, release_date) = match self.album {
            Some(a) => (a.name, a.images, a.release_date),
            None => (String::new(), Vec::new(), None),
        };
        images.sort_by_key(|i| i.width.unwrap_or(0));
        Some(TrackInfo {
            id,
            title: self.name,
            artists: self.artists.into_iter().map(|a| a.name).collect(),
            album,
            duration_ms: self.duration_ms,
            cover_thumb_url: images.first().map(|i| i.url.clone()),
            cover_url: images.last().map(|i| i.url.clone()),
            explicit: self.explicit,
            year: release_date.and_then(|d| d.get(..4)?.parse().ok()),
            from: None,
            requested_by: None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchKind {
    Songs,
    Albums,
    Playlists,
    Artists,
}

impl SearchKind {
    pub const ALL: [SearchKind; 4] = [Self::Songs, Self::Albums, Self::Playlists, Self::Artists];

    fn api_type(self) -> &'static str {
        match self {
            Self::Songs => "track",
            Self::Albums => "album",
            Self::Playlists => "playlist",
            Self::Artists => "artist",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Songs => "Songs",
            Self::Albums => "Albums",
            Self::Playlists => "Playlists",
            Self::Artists => "Artists",
        }
    }

    /// Short id used in button ids.
    pub fn key(self) -> &'static str {
        self.api_type()
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.key() == key)
    }
}

/// What a search result plays.
#[derive(Debug, Clone)]
pub enum Target {
    Track(Box<TrackInfo>),
    Album(SpotifyId),
    Playlist(SpotifyId),
    Artist(SpotifyId),
}

#[derive(Debug, Clone)]
pub struct SearchItem {
    pub title: String,
    pub subtitle: String,
    pub cover: Option<String>,
    pub explicit: bool,
    pub target: Target,
}

/// A cover big enough to look sharp as a thumbnail, without being huge.
fn pick_image(images: Option<&serde_json::Value>) -> Option<String> {
    let mut images: Vec<(u64, String)> = images?
        .as_array()?
        .iter()
        .filter_map(|i| {
            Some((
                i.get("width").and_then(|w| w.as_u64()).unwrap_or(0),
                i.get("url")?.as_str()?.to_string(),
            ))
        })
        .collect();
    images.sort_by_key(|(w, _)| *w);
    images
        .iter()
        .find(|(w, _)| *w >= 160)
        .or(images.last())
        .map(|(_, url)| url.clone())
}

fn names(list: Option<&serde_json::Value>) -> String {
    list.and_then(|l| l.as_array())
        .map(|l| {
            l.iter()
                .filter_map(|a| a.get("name").and_then(|n| n.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

fn count_label(n: Option<u64>, one: &str, many: &str) -> Option<String> {
    n.map(|n| {
        if n == 1 {
            format!("1 {one}")
        } else {
            format!("{n} {many}")
        }
    })
}

impl SearchItem {
    pub fn from_track(t: TrackInfo) -> Self {
        Self {
            title: t.title.clone(),
            subtitle: format!(
                "{} · {} · {}",
                t.artists_joined(),
                t.album,
                crate::util::fmt_ms(t.duration_ms as u64)
            ),
            cover: t.cover_url.clone(),
            explicit: t.explicit,
            target: Target::Track(Box::new(t)),
        }
    }

    fn from_json(kind: SearchKind, item: &serde_json::Value) -> Option<Self> {
        let id = SpotifyId::from_base62(item.get("id")?.as_str()?).ok()?;
        let title = item.get("name")?.as_str()?.to_string();
        let cover = pick_image(item.get("images"));
        let (subtitle, target) = match kind {
            SearchKind::Songs => return None,
            SearchKind::Albums => {
                let year = item
                    .get("release_date")
                    .and_then(|d| d.as_str())
                    .and_then(|d| d.get(..4))
                    .map(str::to_string);
                let songs = count_label(
                    item.get("total_tracks").and_then(|t| t.as_u64()),
                    "song",
                    "songs",
                );
                let parts: Vec<String> = [Some(names(item.get("artists"))), year, songs]
                    .into_iter()
                    .flatten()
                    .filter(|s| !s.is_empty())
                    .collect();
                (parts.join(" · "), Target::Album(id))
            }
            SearchKind::Playlists => {
                let owner = item
                    .get("owner")
                    .and_then(|o| o.get("display_name"))
                    .and_then(|n| n.as_str())
                    .map(|n| format!("by {n}"));
                let total = item
                    .get("tracks")
                    .or_else(|| item.get("items"))
                    .and_then(|t| t.get("total"))
                    .and_then(|t| t.as_u64());
                let parts: Vec<String> = [owner, count_label(total, "song", "songs")]
                    .into_iter()
                    .flatten()
                    .collect();
                (parts.join(" · "), Target::Playlist(id))
            }
            SearchKind::Artists => {
                let genres: Vec<&str> = item
                    .get("genres")
                    .and_then(|g| g.as_array())
                    .map(|g| g.iter().filter_map(|x| x.as_str()).take(2).collect())
                    .unwrap_or_default();
                let subtitle = if genres.is_empty() {
                    "Artist · plays their top songs".to_string()
                } else {
                    format!("Artist · {} · plays their top songs", genres.join(", "))
                };
                (subtitle, Target::Artist(id))
            }
        };
        Some(Self {
            title,
            subtitle,
            cover,
            explicit: false,
            target,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_album_playlist_and_artist_results() {
        let album = serde_json::json!({
            "id": "4uLU6hMCjMI75M1A2tKUQC", "name": "Hurry Up, We're Dreaming",
            "artists": [{"name": "M83"}], "release_date": "2011-10-14", "total_tracks": 22,
            "images": [{"url": "big", "width": 640}, {"url": "mid", "width": 300}, {"url": "small", "width": 64}]
        });
        let item = SearchItem::from_json(SearchKind::Albums, &album).unwrap();
        assert_eq!(item.subtitle, "M83 · 2011 · 22 songs");
        assert_eq!(item.cover.as_deref(), Some("mid"));

        let playlist = serde_json::json!({
            "id": "4uLU6hMCjMI75M1A2tKUQC", "name": "Mix", "owner": {"display_name": "Luca"},
            "tracks": {"total": 1}, "images": []
        });
        let item = SearchItem::from_json(SearchKind::Playlists, &playlist).unwrap();
        assert_eq!(item.subtitle, "by Luca · 1 song");
        assert!(item.cover.is_none());

        let artist = serde_json::json!({"id": "4uLU6hMCjMI75M1A2tKUQC", "name": "M83", "genres": ["electronic"]});
        let item = SearchItem::from_json(SearchKind::Artists, &artist).unwrap();
        assert!(matches!(item.target, Target::Artist(_)));
        assert!(
            SearchItem::from_json(SearchKind::Albums, &serde_json::json!({"name": "no id"}))
                .is_none()
        );
    }
}

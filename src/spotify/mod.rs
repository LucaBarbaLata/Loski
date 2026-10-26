pub mod auth;
pub mod connect;
pub mod library;
pub mod web;

use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{Context, Result, anyhow};
use futures::{StreamExt, stream};
use librespot_core::{
    Session, SessionConfig, SpotifyId, SpotifyUri, authentication::Credentials, cache::Cache,
};
use librespot_metadata::{Album, Artist, Metadata, Playlist, Track};
use librespot_protocol::{
    self as protocol,
    extended_metadata::{
        BatchedEntityRequest, BatchedEntityRequestHeader, EntityRequest, ExtensionQuery,
    },
    extension_kind::ExtensionKind,
};
use parking_lot::RwLock;
use protobuf::{Message as _, MessageField};
use url::Url;

use crate::{config::Config, track::TrackInfo, util::escape_md};
use web::WebApi;

/// Big playlists are capped so a 10k-song playlist doesn't stall the bot.
pub const MAX_TRACKS_PER_REQUEST: usize = 500;
/// Songs per batched metadata request.
const BATCH_SIZE: usize = 100;

/// One song's details, retrying politely if Spotify says to slow down.
async fn fetch_track_with_retry(session: &Session, uri: &SpotifyUri) -> Option<TrackInfo> {
    for attempt in 0..3u32 {
        match Track::get(session, uri).await {
            Ok(track) => return track_info(&track),
            Err(e) if e.to_string().contains("exhausted") => {
                tokio::time::sleep(Duration::from_millis(800 * 2u64.pow(attempt))).await;
            }
            Err(e) => {
                tracing::debug!("skipping {uri:?}: {e}");
                return None;
            }
        }
    }
    None
}
/// Audio files librespot may keep on disk to make replays instant.
const AUDIO_CACHE_BYTES: u64 = 1 << 30;

pub struct Spotify {
    cfg: Arc<Config>,
    session: RwLock<Session>,
    pub web: WebApi,
    http: reqwest::Client,
}

pub enum Lookup {
    Track(SpotifyId),
    Album(SpotifyId),
    Playlist(SpotifyId),
    Artist(SpotifyId),
    ShortLink(String),
    Text(String),
}

pub struct Resolved {
    pub tracks: Vec<TrackInfo>,
    /// For albums, playlists and artists: what the tracks came from.
    pub source: Option<String>,
    pub truncated: bool,
}

pub(crate) fn cache(cfg: &Config) -> Result<Cache> {
    let dir = cfg.data_dir.join("librespot");
    Cache::new(
        Some(dir.clone()),
        None,
        Some(dir.join("audio")),
        Some(AUDIO_CACHE_BYTES),
    )
    .map_err(|e| anyhow!("could not open librespot cache: {e}"))
}

pub fn cached_credentials(cfg: &Config) -> Option<Credentials> {
    cache(cfg).ok()?.credentials()
}

pub fn forget_credentials(cfg: &Config) {
    let _ = std::fs::remove_file(cfg.data_dir.join("librespot").join("credentials.json"));
}

async fn open_session(cfg: &Config, credentials: Credentials) -> Result<Session> {
    let session = Session::new(SessionConfig::default(), Some(cache(cfg)?));
    session
        .connect(credentials, true)
        .await
        .map_err(|e| anyhow!("could not connect to Spotify: {e}"))?;
    Ok(session)
}

impl Spotify {
    pub async fn connect(
        cfg: Arc<Config>,
        http: reqwest::Client,
        credentials: Credentials,
        web_token: auth::WebToken,
    ) -> Result<Self> {
        let session = open_session(&cfg, credentials).await?;
        tracing::info!("Spotify session ready");
        Ok(Self {
            web: WebApi::new(cfg.clone(), http.clone(), web_token),
            cfg,
            session: RwLock::new(session),
            http,
        })
    }

    pub fn session(&self) -> Session {
        self.session.read().clone()
    }

    /// Opens a fresh session from stored credentials after the old one died.
    pub async fn reconnect(&self) -> Result<Session> {
        let credentials =
            cached_credentials(&self.cfg).context("stored Spotify credentials are missing")?;
        let session = open_session(&self.cfg, credentials).await?;
        *self.session.write() = session.clone();
        Ok(session)
    }

    pub async fn resolve(&self, lookup: Lookup) -> Result<Option<Resolved>> {
        let session = self.session();
        let resolved = match lookup {
            Lookup::Text(_) => return Ok(None),
            Lookup::ShortLink(link) => {
                let resolved_link = self.expand_short_link(&link).await?;
                return match parse_lookup(&resolved_link) {
                    Lookup::Text(_) | Lookup::ShortLink(_) => Err(anyhow!(
                        "that link doesn't point to a Spotify song, album or playlist"
                    )),
                    other => Box::pin(self.resolve(other)).await,
                };
            }
            Lookup::Track(id) => {
                let track = Track::get(&session, &SpotifyUri::Track { id })
                    .await
                    .map_err(|e| anyhow!("couldn't load that track: {e}"))?;
                Resolved {
                    tracks: track_info(&track).into_iter().collect(),
                    source: None,
                    truncated: false,
                }
            }
            Lookup::Album(id) => {
                let album = Album::get(&session, &SpotifyUri::Album { id })
                    .await
                    .map_err(|e| anyhow!("couldn't load that album: {e}"))?;
                let uris: Vec<SpotifyUri> = album.tracks().cloned().collect();
                self.load_tracks(uris, format!("the album **{}**", escape_md(&album.name)))
                    .await
            }
            Lookup::Playlist(id) => {
                let playlist = Playlist::get(&session, &SpotifyUri::Playlist { user: None, id })
                    .await
                    .map_err(|e| anyhow!("couldn't load that playlist: {e}"))?;
                let uris: Vec<SpotifyUri> = playlist
                    .tracks()
                    .filter(|u| matches!(u, SpotifyUri::Track { .. }))
                    .cloned()
                    .collect();
                let name = match playlist.name() {
                    "" => "that playlist".to_string(),
                    n => format!("the playlist **{}**", escape_md(n)),
                };
                self.load_tracks(uris, name).await
            }
            Lookup::Artist(id) => {
                let artist = Artist::get(&session, &SpotifyUri::Artist { id })
                    .await
                    .map_err(|e| anyhow!("couldn't load that artist: {e}"))?;
                let uris: Vec<SpotifyUri> = artist
                    .top_tracks
                    .for_country(&session.country())
                    .iter()
                    .cloned()
                    .collect();
                self.load_tracks(
                    uris,
                    format!("**{}**'s top tracks", escape_md(&artist.name)),
                )
                .await
            }
        };
        Ok(Some(resolved))
    }

    /// Details for a list of songs, in the same order, skipping any Spotify
    /// no longer has.
    pub async fn load_tracks(&self, mut uris: Vec<SpotifyUri>, source: String) -> Resolved {
        let truncated = uris.len() > MAX_TRACKS_PER_REQUEST;
        uris.truncate(MAX_TRACKS_PER_REQUEST);

        let mut found: HashMap<SpotifyUri, TrackInfo> = HashMap::new();
        for chunk in uris.chunks(BATCH_SIZE) {
            match self.tracks_batch(chunk).await {
                Ok(tracks) => found.extend(tracks.into_iter().map(|t| (t.uri(), t))),
                Err(e) => tracing::debug!("batched track lookup failed: {e:#}"),
            }
        }

        // Anything the batch missed is fetched one by one, gently.
        let missing: Vec<SpotifyUri> = uris
            .iter()
            .filter(|u| !found.contains_key(u))
            .cloned()
            .collect();
        if !missing.is_empty() {
            tracing::debug!("fetching {} songs individually", missing.len());
            let session = self.session();
            let singles: Vec<TrackInfo> = stream::iter(missing)
                .map(|uri| {
                    let session = session.clone();
                    async move { fetch_track_with_retry(&session, &uri).await }
                })
                .buffer_unordered(4)
                .filter_map(|t| async move { t })
                .collect()
                .await;
            found.extend(singles.into_iter().map(|t| (t.uri(), t)));
        }

        let tracks = uris.iter().filter_map(|u| found.remove(u)).collect();
        Resolved {
            tracks,
            source: Some(source),
            truncated,
        }
    }

    /// One request for up to `BATCH_SIZE` songs, through Spotify's extended
    /// metadata endpoint (the same one its own apps use for long lists).
    async fn tracks_batch(&self, uris: &[SpotifyUri]) -> Result<Vec<TrackInfo>> {
        let session = self.session();
        let request = BatchedEntityRequest {
            header: MessageField::some(BatchedEntityRequestHeader {
                country: session.country(),
                catalogue: "premium".to_string(),
                ..Default::default()
            }),
            entity_request: uris
                .iter()
                .filter_map(|u| u.to_uri().ok())
                .map(|uri| EntityRequest {
                    entity_uri: uri,
                    query: vec![ExtensionQuery {
                        extension_kind: ExtensionKind::TRACK_V4.into(),
                        ..Default::default()
                    }],
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        };
        let response = session
            .spclient()
            .get_extended_metadata(request)
            .await
            .map_err(|e| anyhow!("{e}"))?;

        let mut tracks = Vec::new();
        for array in &response.extended_metadata {
            for data in &array.extension_data {
                let bytes = &data.extension_data.value;
                if bytes.is_empty() {
                    continue;
                }
                let parsed = protocol::metadata::Track::parse_from_bytes(bytes)
                    .ok()
                    .and_then(|msg| Track::try_from(&msg).ok());
                if let Some(info) = parsed.as_ref().and_then(track_info) {
                    tracks.push(info);
                }
            }
        }
        Ok(tracks)
    }

    async fn expand_short_link(&self, link: &str) -> Result<String> {
        let resp = self.http.get(link).send().await?;
        let final_url = resp.url().to_string();
        if final_url.contains("open.spotify.com/") {
            return Ok(final_url);
        }
        // Some short links land on an HTML page that contains the real URL.
        let body = resp.text().await.unwrap_or_default();
        let start = body
            .find("https://open.spotify.com/")
            .ok_or_else(|| anyhow!("couldn't follow that short link"))?;
        let end = body[start..]
            .find(|c: char| c == '"' || c == '\'' || c == '?' || c == '<' || c.is_whitespace())
            .map_or(body.len(), |e| start + e);
        Ok(body[start..end].to_string())
    }
}

pub fn parse_lookup(input: &str) -> Lookup {
    let s = input.trim();

    if let Some(rest) = s.strip_prefix("spotify:") {
        // spotify:track:ID, or the old spotify:user:NAME:playlist:ID
        let parts: Vec<&str> = rest.split(':').collect();
        if parts.len() >= 2
            && let Some(lookup) = from_kind(parts[parts.len() - 2], parts[parts.len() - 1])
        {
            return lookup;
        }
    }

    if let Ok(url) = Url::parse(s) {
        match url.host_str() {
            Some("open.spotify.com" | "play.spotify.com") => {
                let segments: Vec<&str> = url
                    .path_segments()
                    .map(|p| {
                        p.filter(|x| !x.is_empty() && !x.starts_with("intl-"))
                            .collect()
                    })
                    .unwrap_or_default();
                for pair in segments.windows(2) {
                    if let Some(lookup) = from_kind(pair[0], pair[1]) {
                        return lookup;
                    }
                }
            }
            Some("spotify.link" | "spotify.app.link") => return Lookup::ShortLink(s.to_string()),
            _ => {}
        }
    }

    Lookup::Text(s.to_string())
}

fn from_kind(kind: &str, id: &str) -> Option<Lookup> {
    let id = SpotifyId::from_base62(id).ok()?;
    match kind {
        "track" => Some(Lookup::Track(id)),
        "album" => Some(Lookup::Album(id)),
        "playlist" => Some(Lookup::Playlist(id)),
        "artist" => Some(Lookup::Artist(id)),
        _ => None,
    }
}

fn track_info(track: &Track) -> Option<TrackInfo> {
    let SpotifyUri::Track { id } = track.id else {
        return None;
    };
    let mut covers: Vec<_> = track
        .album
        .covers
        .iter()
        .filter_map(|c| Some((c.width, c.id.to_base16().ok()?)))
        .collect();
    covers.sort_by_key(|(w, _)| *w);
    let cover = |hex: &String| format!("https://i.scdn.co/image/{hex}");
    Some(TrackInfo {
        id,
        title: track.name.clone(),
        artists: track.artists.iter().map(|a| a.name.clone()).collect(),
        album: track.album.name.clone(),
        duration_ms: track.duration.max(0) as u32,
        cover_url: covers.last().map(|(_, hex)| cover(hex)),
        cover_thumb_url: covers.first().map(|(_, hex)| cover(hex)),
        explicit: track.is_explicit,
        // Missing dates come back as the Unix epoch.
        year: Some(track.album.date.year()).filter(|y| *y > 1900 && *y != 1970),
        from: None,
        requested_by: None,
    })
}

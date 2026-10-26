//! Genres for the artists you listen to, from MusicBrainz, the open music
//! database (Spotify no longer shares artist genres with apps like Loski).
//! Looked up in the background, about one request a second as MusicBrainz
//! asks, and kept in `data/genres.json` so each artist is looked up once.

use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use anyhow::Result;
use parking_lot::Mutex;
use serde::Deserialize;
use tokio::sync::Notify;

use crate::store::write_atomic;

/// MusicBrainz allows one request a second; a little slower to be kind.
const PAUSE: Duration = Duration::from_millis(1_100);
/// Genres kept per artist, the most agreed on first.
const KEEP: usize = 4;

/// Artist (lowercased) → genres. An empty list means MusicBrainz has none.
pub type GenreMap = HashMap<String, Vec<String>>;

pub struct Genres {
    known: Mutex<GenreMap>,
    wanted: Mutex<Vec<String>>,
    wake: Notify,
    path: PathBuf,
}

impl Genres {
    pub fn load(dir: &std::path::Path) -> Arc<Self> {
        let path = dir.join("genres.json");
        let known = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Arc::new(Self {
            known: Mutex::new(known),
            wanted: Mutex::new(Vec::new()),
            wake: Notify::new(),
            path,
        })
    }

    /// Everything looked up so far.
    pub fn known(&self) -> GenreMap {
        self.known.lock().clone()
    }

    /// Asks for these artists to be looked up, the first ones first.
    pub fn want<'a>(&self, artists: impl IntoIterator<Item = &'a str>) {
        let known = self.known.lock();
        let mut wanted = self.wanted.lock();
        let mut added = false;
        for artist in artists {
            let key = artist.to_lowercase();
            if !known.contains_key(&key) && !wanted.contains(&key) {
                wanted.push(key);
                added = true;
            }
        }
        if added {
            self.wake.notify_one();
        }
    }

    fn remember(&self, artist: String, genres: Vec<String>) {
        let json = {
            let mut known = self.known.lock();
            known.insert(artist, genres);
            serde_json::to_string_pretty(&*known).expect("genres serialize")
        };
        if let Err(e) = write_atomic(&self.path, json.as_bytes()) {
            tracing::warn!("could not save genres: {e}");
        }
    }
}

#[derive(Deserialize)]
struct Search {
    #[serde(default)]
    artists: Vec<Found>,
}

#[derive(Deserialize)]
struct Found {
    id: String,
    name: String,
    #[serde(default)]
    score: u32,
}

#[derive(Deserialize)]
struct Lookup {
    #[serde(default)]
    genres: Vec<Genre>,
}

#[derive(Deserialize)]
struct Genre {
    name: String,
    #[serde(default)]
    count: u32,
}

/// The MusicBrainz artist that is `name`, if one clearly is.
fn best_match(name: &str, found: Vec<Found>) -> Option<String> {
    found
        .into_iter()
        .find(|f| f.score >= 90 && f.name.to_lowercase() == name)
        .map(|f| f.id)
}

/// The most agreed-on genres first, like "alternative metal".
fn top_genres(mut genres: Vec<Genre>) -> Vec<String> {
    genres.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    genres.into_iter().take(KEEP).map(|g| g.name).collect()
}

async fn fetch<T: for<'de> Deserialize<'de>>(
    web: &reqwest::Client,
    url: &str,
    query: &[(&str, &str)],
) -> Result<T> {
    let response = web
        .get(url)
        .query(query)
        .header("Accept", "application/json")
        // MusicBrainz asks every app to say who it is and where to find it.
        .header(
            "User-Agent",
            concat!(
                "Loski/",
                env!("CARGO_PKG_VERSION"),
                " ( https://github.com/LucaBarbaLata/Loski )"
            ),
        )
        .timeout(Duration::from_secs(15))
        .send()
        .await?
        .error_for_status()?;
    Ok(response.json().await?)
}

async fn look_up(web: &reqwest::Client, artist: &str) -> Result<Vec<String>> {
    let quoted = format!("artist:\"{}\"", artist.replace('"', ""));
    let search: Search = fetch(
        web,
        "https://musicbrainz.org/ws/2/artist/",
        &[("query", &quoted), ("fmt", "json"), ("limit", "5")],
    )
    .await?;
    let Some(id) = best_match(artist, search.artists) else {
        return Ok(Vec::new());
    };
    tokio::time::sleep(PAUSE).await;
    let lookup: Lookup = fetch(
        web,
        &format!("https://musicbrainz.org/ws/2/artist/{id}"),
        &[("inc", "genres"), ("fmt", "json")],
    )
    .await?;
    Ok(top_genres(lookup.genres))
}

/// Looks up whatever is wanted, one artist at a time.
pub async fn fill_loop(genres: Arc<Genres>, web: reqwest::Client) {
    let mut failed: HashSet<String> = HashSet::new();
    loop {
        let next = genres.wanted.lock().first().cloned();
        let Some(artist) = next else {
            genres.wake.notified().await;
            continue;
        };
        let retry = match look_up(&web, &artist).await {
            Ok(found) => {
                tracing::debug!("genres for {artist}: {found:?}");
                genres.remember(artist.clone(), found);
                false
            }
            Err(e) => {
                tracing::debug!("couldn't look up genres for {artist}: {e:#}");
                // One more try at the back of the line, then it's left out.
                let first_failure = failed.insert(artist.clone());
                if !first_failure {
                    genres.remember(artist.clone(), Vec::new());
                }
                first_failure
            }
        };
        {
            let mut wanted = genres.wanted.lock();
            wanted.retain(|a| *a != artist);
            if retry {
                wanted.push(artist);
            }
        }
        tokio::time::sleep(PAUSE).await;
    }
}

/// "Alternative metal".
pub fn display(genre: &str) -> String {
    let mut chars = genre.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_musicbrainz_answers() {
        let search: Search = serde_json::from_str(
            r#"{"created":"x","count":2,"offset":0,"artists":[
                {"id":"a1","type":"Group","score":100,"name":"Breaking Benjamin","tags":[{"count":3,"name":"rock"}]},
                {"id":"a2","score":60,"name":"Breaking Benjamin Tribute"}]}"#,
        )
        .unwrap();
        assert_eq!(
            best_match("breaking benjamin", search.artists).as_deref(),
            Some("a1")
        );
        let lookup: Lookup = serde_json::from_str(
            r#"{"id":"a1","name":"Breaking Benjamin","genres":[
                {"count":2,"disambiguation":"","id":"g1","name":"post-grunge"},
                {"count":5,"disambiguation":"","id":"g2","name":"alternative metal"},
                {"count":1,"disambiguation":"","id":"g3","name":"hard rock"}]}"#,
        )
        .unwrap();
        assert_eq!(
            top_genres(lookup.genres),
            ["alternative metal", "post-grunge", "hard rock"]
        );
        let none: Search = serde_json::from_str(r#"{"artists":[]}"#).unwrap();
        assert!(best_match("x", none.artists).is_none());
        assert_eq!(display("alternative metal"), "Alternative metal");
    }
}

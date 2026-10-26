//! Blend: a mix of your most played songs of the last four weeks, woven
//! together with what friends liked on the panel or asked for. Rebuilt every
//! Monday (or the first time Loski runs after one) as the mix "Blend", so
//! `/mix load name:Blend` plays it.

use std::{collections::HashSet, sync::Arc, time::Duration};

use chrono::Datelike;
use librespot_core::{SpotifyId, SpotifyUri};

use super::{App, friends, mixes, stats::now};
use crate::{spotify::auth::now_unix, stats, store::HistoryEntry};

pub const NAME: &str = "Blend";
/// Remembers the week the Blend was last made for.
const MARKER: &str = "blend_week";
/// How far back "your top songs" and friends' requests reach.
const WEEKS: i64 = 4;
/// Songs from each side, and in all.
const EACH: usize = 30;
const SIZE: usize = 50;

/// The friends' songs: what they liked (most liked first), then what they
/// asked for lately (newest first).
fn friends_songs(liked: Vec<String>, history: &[HistoryEntry], since: i64) -> Vec<String> {
    let asked = history
        .iter()
        .rev()
        .filter(|e| e.played_at >= since && e.requested_by.is_some())
        .map(|e| e.uri.clone());
    liked.into_iter().chain(asked).collect()
}

/// Yours and theirs, taking turns, each song once.
pub fn weave(yours: &[String], theirs: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut blend = Vec::new();
    let (mut a, mut b) = (yours.iter(), theirs.iter());
    loop {
        let (x, y) = (a.next(), b.next());
        if x.is_none() && y.is_none() {
            break;
        }
        for uri in [x, y].into_iter().flatten() {
            if blend.len() < SIZE && seen.insert(uri.clone()) {
                blend.push(uri.clone());
            }
        }
    }
    blend
}

/// Checks every hour whether it's a new week, and rebuilds the Blend.
pub async fn blend_loop(app: Arc<App>) {
    loop {
        rebuild_if_due(&app).await;
        tokio::time::sleep(Duration::from_secs(3600)).await;
    }
}

async fn rebuild_if_due(app: &App) {
    let week = now(app).iso_week();
    let key = format!("{}-W{:02}", week.year(), week.week());
    if app.store.read_marker(MARKER).as_deref() == Some(key.as_str()) {
        return;
    }
    let Some(core) = app.core.get() else { return };
    let since = now_unix() - WEEKS * 7 * 86_400;
    let history = app.store.all_history();
    let yours: Vec<String> = stats::top_uris(&history, since, EACH);
    let theirs: Vec<String> = friends_songs(friends::friends_liked(app), &history, since)
        .into_iter()
        .take(EACH)
        .collect();
    let uris: Vec<SpotifyUri> = weave(&yours, &theirs)
        .iter()
        .filter_map(|uri| SpotifyId::from_base62(uri.rsplit(':').next()?).ok())
        .map(|id| SpotifyUri::Track { id })
        .collect();
    if uris.len() < 5 {
        // Too little to blend yet; try again next week.
        app.store.write_marker(MARKER, &key);
        return;
    }
    let resolved = core
        .spotify
        .load_tracks(uris, format!("your **{NAME}**"))
        .await;
    let tracks: Vec<Arc<crate::track::TrackInfo>> =
        resolved.tracks.into_iter().map(Arc::new).collect();
    match mixes::save_tracks(app, NAME, &tracks) {
        Ok(_) => {
            tracing::info!(
                "Made this week's Blend: {} songs, {} from friends",
                tracks.len(),
                theirs.len().min(tracks.len())
            );
            app.store.write_marker(MARKER, &key);
        }
        Err(e) => tracing::warn!("couldn't save this week's Blend: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uris(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| format!("spotify:track:{id}")).collect()
    }

    #[test]
    fn takes_turns_without_repeats() {
        let blend = weave(&uris(&["a", "b", "c", "d"]), &uris(&["x", "b", "y"]));
        assert_eq!(blend, uris(&["a", "x", "b", "c", "y", "d"]));
        let many: Vec<String> = (0..80).map(|n| format!("spotify:track:{n}")).collect();
        assert_eq!(weave(&many, &[]).len(), SIZE);
        assert!(weave(&[], &[]).is_empty());
    }

    #[test]
    fn friends_songs_are_likes_then_requests() {
        let entry = |at: i64, id: &str, by: Option<u64>| HistoryEntry {
            played_at: at,
            uri: format!("spotify:track:{id}"),
            title: String::new(),
            artists: String::new(),
            artist_list: Vec::new(),
            cover: None,
            listened_ms: None,
            with: Vec::new(),
            requested_by: by,
        };
        let history = [
            entry(10, "old", Some(1)),
            entry(100, "mine", None),
            entry(200, "asked", Some(2)),
            entry(300, "later", Some(1)),
        ];
        let songs = friends_songs(uris(&["liked"]), &history, 50);
        assert_eq!(songs, uris(&["liked", "later", "asked"]));
    }
}

//! Milestones: small DMs for 100 hours with Loski, the 1,000th song, or 30
//! days in a row with music. Milestones from before this feature (or before
//! Loski first looked) are skipped, so nobody gets a pile of them at once.

use std::{sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};

use super::{
    App, cards,
    stats::{compute, thousands},
};
use crate::{
    stats::Range,
    util::{escape_md, truncate},
};

const HOURS: [u64; 10] = [10, 25, 50, 100, 250, 500, 1_000, 2_500, 5_000, 10_000];
const SONGS: [u64; 9] = [
    100, 500, 1_000, 2_500, 5_000, 10_000, 25_000, 50_000, 100_000,
];
const STREAKS: [u64; 7] = [7, 14, 30, 50, 100, 200, 365];
/// What's been celebrated so far.
const MARKER: &str = "milestones.json";

/// The highest milestone of each kind reached so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reached {
    pub hours: u64,
    pub songs: u64,
    pub streak: u64,
}

/// The highest of `thresholds` that `value` has reached (0 for none).
fn highest(thresholds: &[u64], value: u64) -> u64 {
    thresholds
        .iter()
        .copied()
        .filter(|t| value >= *t)
        .max()
        .unwrap_or(0)
}

/// What's newly reached since `before`, and what to remember now.
pub fn check(before: Reached, hours: u64, songs: u64, streak: u64) -> (Reached, Reached) {
    let now = Reached {
        hours: highest(&HOURS, hours),
        songs: highest(&SONGS, songs),
        streak: highest(&STREAKS, streak),
    };
    let new = Reached {
        hours: if now.hours > before.hours {
            now.hours
        } else {
            0
        },
        songs: if now.songs > before.songs {
            now.songs
        } else {
            0
        },
        streak: if now.streak > before.streak {
            now.streak
        } else {
            0
        },
    };
    let keep = Reached {
        hours: now.hours.max(before.hours),
        songs: now.songs.max(before.songs),
        streak: now.streak.max(before.streak),
    };
    (new, keep)
}

/// Looks every ten minutes, and DMs you when something's reached.
pub async fn milestone_loop(app: Arc<App>) {
    loop {
        celebrate(&app).await;
        tokio::time::sleep(Duration::from_secs(600)).await;
    }
}

async fn celebrate(app: &App) {
    let stats = compute(app, Range::AllTime).await;
    let before: Option<Reached> = app
        .store
        .read_marker(MARKER)
        .and_then(|s| serde_json::from_str(&s).ok());
    let (new, keep) = check(
        before.unwrap_or_default(),
        stats.listened_ms / 3_600_000,
        stats.plays as u64,
        stats.streak as u64,
    );
    let save = |reached: &Reached| {
        app.store
            .write_marker(MARKER, &serde_json::to_string(reached).expect("milestones"));
    };
    let Some(before) = before else {
        // The first look: what's already behind you isn't news.
        save(&keep);
        return;
    };
    if keep == before {
        return;
    }
    let top = stats.top_songs.first();
    let since = stats.first_day.format("%B %-d, %Y");
    let bot = escape_md(&app.name());
    let mut notes = Vec::new();
    if new.hours > 0 {
        let mut text = format!(
            "🎉 **{} hours with {bot}.** That's {} songs since {since}.",
            thousands(new.hours),
            thousands(stats.plays as u64)
        );
        if let Some(song) = top {
            text.push_str(&format!(
                " The one you played most: **{}** by {}.",
                escape_md(&truncate(&song.title, 80)),
                escape_md(&truncate(&song.artists, 60))
            ));
        }
        notes.push((text, top.and_then(|s| s.cover.clone())));
    }
    if new.songs > 0 {
        let latest = app.store.recent_history(1).into_iter().next();
        let text = match &latest {
            Some(song) => format!(
                "🎉 **Your {}th song with {bot}** was **{}** by {}.",
                thousands(new.songs),
                escape_md(&truncate(&song.title, 80)),
                escape_md(&truncate(&song.artists, 60))
            ),
            None => format!("🎉 **{} songs with {bot}.**", thousands(new.songs)),
        };
        notes.push((text, latest.and_then(|s| s.cover)));
    }
    if new.streak > 0 {
        notes.push((
            format!(
                "🔥 **{} days in a row** with music, as of today. Keep it going!",
                new.streak
            ),
            None,
        ));
    }
    for (text, cover) in notes {
        let card = cards::toast(&app.icons, &text, cover.as_deref(), cards::color::BRAND);
        if !app.dm_owner(card).await {
            // Try again later; nothing's remembered yet.
            return;
        }
    }
    save(&keep);
    tracing::info!("Milestones reached: {new:?}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn celebrates_each_milestone_once() {
        let (new, keep) = check(Reached::default(), 101, 990, 6);
        assert_eq!((new.hours, new.songs, new.streak), (100, 500, 0));
        assert_eq!(keep.hours, 100);
        // The next look, a little later: nothing new.
        let (new, keep) = check(keep, 102, 999, 7);
        assert_eq!((new.hours, new.songs, new.streak), (0, 0, 7));
        let (new, _) = check(keep, 102, 1_000, 3);
        assert_eq!(new.songs, 1_000);
        // A broken streak doesn't take back what was reached.
        let (_, keep) = check(keep, 102, 1_000, 3);
        assert_eq!(keep.streak, 7);
    }
}

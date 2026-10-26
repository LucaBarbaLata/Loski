//! The goodbye recap: when Loski leaves voice, the panel turns into a short
//! summary of the session, with buttons to keep it as a mix or see your week.

use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use chrono::{TimeZone, Timelike, Utc};
use twilight_model::channel::message::{Component, component::ButtonStyle};

use super::{
    App, LONG_SECS, SHORT_SECS,
    cards::{self, Btn, color},
    icons::{Icon, Icons},
    interact::Ix,
    mixes, oops,
};
use crate::{
    engine::Played,
    stats::Range,
    track::TrackInfo,
    util::{escape_md, fmt_duration_words, truncate},
};

pub struct Recap {
    pub listened_ms: u64,
    pub plays: usize,
    pub started_at: i64,
    /// The most played song, if any played more than once.
    pub favourite: Option<(Arc<TrackInfo>, usize)>,
    /// The artist behind most of the session, if one stood out.
    pub artist: Option<String>,
    /// Each song once, in the order they first played.
    pub songs: Vec<Arc<TrackInfo>>,
    /// Friends you listened with and for how long (ms), longest first.
    pub with: Vec<(String, u64)>,
    /// Songs liked this session and by whom, most liked first.
    pub liked: Vec<(Arc<TrackInfo>, Vec<String>)>,
}

impl Recap {
    /// Sums up `plays`; `name` turns a friend's user id into their name.
    pub fn from_plays(plays: &[Played], name: impl Fn(u64) -> String) -> Option<Self> {
        let first = plays.first()?;
        let mut together: HashMap<u64, u64> = HashMap::new();
        for play in plays {
            for friend in &play.with {
                *together.entry(*friend).or_default() += play.listened_ms;
            }
        }
        let mut with: Vec<(String, u64)> = together
            .into_iter()
            .map(|(id, ms)| (name(id), ms))
            .collect();
        with.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut counts: HashMap<_, (usize, u64)> = HashMap::new();
        let mut songs: Vec<Arc<TrackInfo>> = Vec::new();
        let mut artists: HashMap<&str, usize> = HashMap::new();
        for play in plays {
            let count = counts.entry(play.track.id).or_default();
            if count.0 == 0 {
                songs.push(play.track.clone());
            }
            count.0 += 1;
            count.1 += play.listened_ms;
            for artist in &play.track.artists {
                *artists.entry(artist).or_default() += 1;
            }
        }
        let favourite = songs
            .iter()
            .map(|t| (t, counts[&t.id]))
            .max_by_key(|(_, (n, ms))| (*n, *ms))
            .filter(|(_, (n, _))| *n > 1)
            .map(|(t, (n, _))| (t.clone(), n));
        // Only worth saying when one artist made up a good part of it.
        let artist = artists
            .into_iter()
            .max_by_key(|(name, n)| (*n, std::cmp::Reverse(*name)))
            .filter(|(_, n)| *n >= 3 && *n * 3 >= plays.len())
            .map(|(name, _)| name.to_string());
        Some(Self {
            listened_ms: plays.iter().map(|p| p.listened_ms).sum(),
            plays: plays.len(),
            started_at: first.started_at,
            favourite,
            artist,
            songs,
            with,
            liked: Vec::new(),
        })
    }
}

pub fn recap_card(icons: &Icons, accent: u32, recap: &Recap, farewell: Option<&str>) -> Component {
    let songs = if recap.plays == 1 {
        "1 song".to_string()
    } else {
        format!("{} songs", recap.plays)
    };
    let mut summary = format!(
        "-# THAT'S A WRAP\n## {} of music\n**{songs}** since <t:{}:t>",
        fmt_duration_words(recap.listened_ms),
        recap.started_at
    );
    if let Some(artist) = &recap.artist {
        summary.push_str(&format!(", mostly **{}**", escape_md(artist)));
    }
    let cover = recap
        .favourite
        .as_ref()
        .map(|(t, _)| t)
        .or(recap.songs.last())
        .and_then(|t| t.cover_thumb_url.as_deref());
    let mut parts = vec![cards::text_with_cover(summary, cover)];

    if let Some((track, plays)) = &recap.favourite {
        parts.push(cards::text(format!(
            "-# ON REPEAT\n[**{}**]({}) · {} · {plays} plays",
            escape_md(&truncate(&track.title, 60)),
            track.url(),
            escape_md(&truncate(&track.artists_joined(), 40))
        )));
    }
    if !recap.with.is_empty() {
        let people: Vec<String> = recap
            .with
            .iter()
            .take(4)
            .map(|(name, ms)| {
                format!(
                    "**{}** {}",
                    escape_md(&truncate(name, 32)),
                    fmt_duration_words(*ms)
                )
            })
            .collect();
        let more = recap.with.len().saturating_sub(4);
        let more = if more > 0 {
            format!(" · and {more} more")
        } else {
            String::new()
        };
        parts.push(cards::text(format!(
            "-# LISTENING WITH\n{}{more}",
            people.join(" · ")
        )));
    }
    if !recap.liked.is_empty() {
        let lines: Vec<String> = recap
            .liked
            .iter()
            .take(3)
            .map(|(track, by)| {
                format!(
                    "{} [**{}**]({}) · {}",
                    icons.md(Icon::Liked),
                    escape_md(&truncate(&track.title, 50)),
                    track.url(),
                    super::friends::names_list(by, 3)
                )
            })
            .collect();
        parts.push(cards::text(format!("-# MOST LIKED\n{}", lines.join("\n"))));
    }
    if let Some(text) = farewell {
        parts.push(cards::text(format!("-# {text}")));
    }
    parts.push(cards::row(vec![
        Btn::icon("loski:recap:save", Icon::Add)
            .with_label("Save as a mix")
            .style(ButtonStyle::Primary)
            .build(icons),
        Btn::icon("loski:recap:stats", Icon::Stats)
            .with_label("Your week")
            .build(icons),
    ]));
    cards::card(Some(accent), parts)
}

/// Turns the panel into the recap. Returns false when there's nothing to
/// sum up (nothing played long enough to count).
pub async fn show(
    app: &App,
    plays: &[Played],
    mut liked: Vec<(Arc<TrackInfo>, Vec<String>)>,
    farewell: Option<&str>,
) -> bool {
    let name = |id| {
        app.room
            .name_of(id)
            .unwrap_or_else(|| "a friend".to_string())
    };
    let (Some(mut recap), Some(home)) = (Recap::from_plays(plays, name), app.ui.home()) else {
        return false;
    };
    // A song liked twice in one night counts once, with everyone who liked it.
    let mut merged: Vec<(Arc<TrackInfo>, Vec<String>)> = Vec::new();
    for (track, by) in liked.drain(..) {
        match merged.iter_mut().find(|(t, _)| t.id == track.id) {
            Some((_, names)) => {
                for name in by {
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
            None => merged.push((track, by)),
        }
    }
    merged.sort_by_key(|(_, by)| std::cmp::Reverse(by.len()));
    recap.liked = merged;
    let components = [recap_card(&app.icons, app.ui.color(), &recap, farewell)];

    let mut posted = None;
    if let Some((channel, message)) = app.ui.take_panel() {
        let edited = app
            .http
            .update_message(channel, message)
            .flags(cards::V2)
            .keep_attachment_ids(&[])
            .components(Some(&components))
            .await;
        match edited {
            Ok(_) => posted = Some((channel, message)),
            Err(_) => {
                let _ = app.http.delete_message(channel, message).await;
            }
        }
    }
    if posted.is_none()
        && let Ok(response) = app
            .http
            .create_message(home)
            .flags(cards::V2)
            .components(&components)
            .await
        && let Ok(message) = response.model().await
    {
        posted = Some((home, message.id));
    }
    let Some(posted) = posted else {
        return false;
    };
    app.ui.set_recap(posted, recap.started_at, recap.songs);
    true
}

/// "Saturday night", "Sunday afternoon": a name for the mix, from when the
/// session began. Just after midnight still counts as the night before.
fn session_name(app: &App, started_at: i64) -> String {
    let Some(at) = Utc.timestamp_opt(started_at, 0).single() else {
        return "Last session".to_string();
    };
    let at = at.with_timezone(&app.timezone());
    let part = match at.hour() {
        5..=11 => "morning",
        12..=16 => "afternoon",
        17..=21 => "evening",
        _ => "night",
    };
    let day = at - chrono::Duration::hours(5);
    format!("{} {part}", day.format("%A"))
}

fn gone() -> anyhow::Error {
    oops("That session is too long ago to save now; I only remember the last one.")
}

/// The recap's "Save as a mix" button: asks for a name first.
pub async fn ask_mix_name(ix: &Ix) -> Result<()> {
    let app = &ix.app;
    let (started_at, songs) = app.ui.last_session().ok_or_else(gone)?;
    let about = if songs.len() == 1 {
        "1 song".to_string()
    } else {
        format!("{} songs, in the order they first played", songs.len())
    };
    ix.modal(
        "loski:m:recapsave",
        "Save this session as a mix",
        vec![cards::text_field(
            "name",
            "Name",
            Some(&about),
            "Saturday night",
            Some(session_name(app, started_at)),
            40,
        )],
    )
    .await
}

/// The name came back from the form.
pub async fn save_mix(ix: &Ix, name: &str) -> Result<()> {
    let app = &ix.app;
    let (_, songs) = app.ui.last_session().ok_or_else(gone)?;
    let text = mixes::save_tracks(app, name, &songs)?;
    ix.reply_temp(
        vec![cards::toast(&app.icons, &text, None, color::SUCCESS)],
        SHORT_SECS,
    )
    .await
}

/// The recap's "Your week" button.
pub async fn show_week(ix: &Ix) -> Result<()> {
    super::stats::show(ix, Range::Week, LONG_SECS * 2).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use librespot_core::SpotifyId;

    fn play(n: u8, artist: &str) -> Played {
        Played {
            track: Arc::new(TrackInfo {
                id: SpotifyId::from_raw(&[n; 16]).unwrap(),
                title: format!("Song {n}"),
                artists: vec![artist.into()],
                album: String::new(),
                duration_ms: 200_000,
                cover_url: None,
                cover_thumb_url: None,
                explicit: false,
                year: None,
                from: None,
                requested_by: None,
            }),
            started_at: 1_760_000_000 + n as i64,
            listened_ms: 180_000,
            with: if n == 1 { vec![7] } else { Vec::new() },
        }
    }

    #[test]
    fn sums_up_a_session() {
        let plays = [
            play(1, "Breaking Benjamin"),
            play(2, "Saliva"),
            play(1, "Breaking Benjamin"),
            play(3, "Breaking Benjamin"),
            play(1, "Breaking Benjamin"),
        ];
        let recap = Recap::from_plays(&plays, |id| format!("Friend {id}")).unwrap();
        assert_eq!(recap.plays, 5);
        // Friend 7 was there for the three plays of Song 1.
        assert_eq!(recap.with, [("Friend 7".to_string(), 3 * 180_000)]);
        assert_eq!(recap.listened_ms, 5 * 180_000);
        assert_eq!(recap.started_at, 1_760_000_001);
        let (favourite, times) = recap.favourite.unwrap();
        assert_eq!((favourite.title.as_str(), times), ("Song 1", 3));
        assert_eq!(recap.artist.as_deref(), Some("Breaking Benjamin"));
        let order: Vec<_> = recap.songs.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(order, ["Song 1", "Song 2", "Song 3"]);
    }

    #[test]
    fn a_mixed_session_names_no_favourites() {
        let plays = [play(1, "A"), play(2, "B"), play(3, "C"), play(4, "D")];
        let recap = Recap::from_plays(&plays, |_| String::new()).unwrap();
        assert!(recap.favourite.is_none());
        assert!(recap.artist.is_none());
        assert!(Recap::from_plays(&[], |_| String::new()).is_none());
    }
}

//! The pictures in the README, made by Loski's own code: the artwork, the
//! stats and Wrapped pictures, and the cards as the JSON Discord gets, which
//! `scripts/readme/render.mjs` turns into screenshots. Run it with
//!
//! ```text
//! LOSKI_README_OUT=<folder> LOSKI_README_COVERS=<folder> cargo test readme -- --ignored
//! ```
//!
//! The covers (`0.jpg` to `5.jpg`, one per song below) are each song's
//! cover from Spotify; without them, drawn gradients stand in.

use std::{path::Path, sync::Arc, time::Duration};

use image::RgbImage;
use librespot_core::SpotifyId;
use twilight_model::{channel::message::Component, id::Id};

use super::{
    friends::RoomView,
    icons::Icons,
    lyrics::{Line, Lyrics},
    panel::{self, Look},
    recap::{self, Recap},
    requests::{Request, requests_card},
    settings::{SettingsView, settings_card},
    sound::{EqView, eq_card},
};
use crate::{
    banner::{self, BannerText},
    color,
    config::AudioBitrate,
    dsp::Filter,
    engine::{QueueView, Repeat, Snapshot, Status},
    picture::{self, Artist, Bar, Song, StatsPicture, Tile, WrappedPicture},
    store::Prefs,
    track::TrackInfo,
};

/// Title, artist, Spotify id and length of the songs in the pictures.
const SONGS: [(&str, &str, &str, u32); 6] = [
    (
        "The Diary of Jane",
        "Breaking Benjamin",
        "0faXHILILebCGnJBPU6KJJ",
        200_000,
    ),
    (
        "Dance With The Devil",
        "Breaking Benjamin",
        "10ASBwZsp7oUUDsJEYz3uS",
        270_880,
    ),
    (
        "Lost in You",
        "Three Days Grace",
        "1uiwlmWedlNpzAxDSRGqdK",
        226_932,
    ),
    (
        "Never Again",
        "Breaking Benjamin",
        "07huQEWGQAe7aJaeR08Cag",
        218_191,
    ),
    ("Stupify", "Disturbed", "40UZ85suwIhop8bJbVwkOg", 267_422),
    (
        "Polyamorous",
        "Breaking Benjamin",
        "0Cx5QWilR60IyyUvGZQtDC",
        177_000,
    ),
];

/// The friends in the pictures, and the ids their mentions use.
const FRIENDS: [(u64, &str); 2] = [(301, "Ana"), (302, "Vlad")];
const MUSIC_CHANNEL: u64 = 111;
const LOUNGE_CHANNEL: u64 = 222;
const DJ_ROLE: u64 = 401;
/// "Now" in the pictures: Tuesday, October 27, 2026, 21:14 in Bucharest.
const NOW: i64 = 1_793_128_440;

/// A stand-in cover: a two-colour gradient with a soft blob of light.
fn drawn(seed: usize) -> RgbImage {
    const PALETTES: [[[f32; 3]; 3]; 3] = [
        [
            [16.0, 40.0, 79.0],
            [240.0, 138.0, 60.0],
            [255.0, 210.0, 90.0],
        ],
        [
            [58.0, 13.0, 46.0],
            [176.0, 48.0, 106.0],
            [255.0, 224.0, 138.0],
        ],
        [
            [37.0, 28.0, 92.0],
            [123.0, 224.0, 214.0],
            [240.0, 240.0, 255.0],
        ],
    ];
    let [from, to, glow] = PALETTES[seed % PALETTES.len()];
    RgbImage::from_fn(640, 640, |x, y| {
        let (u, v) = (x as f32 / 640.0, y as f32 / 640.0);
        let t = (u * 0.4 + v * 0.6).clamp(0.0, 1.0);
        let d = ((u - 0.3).powi(2) + (v - 0.68).powi(2)).sqrt() / 0.42;
        let a = (1.0 - d).clamp(0.0, 1.0).powf(1.6);
        let px = |i: usize| {
            let base = from[i] + (to[i] - from[i]) * t;
            (base + (glow[i] - base) * a * 0.85).clamp(0.0, 255.0) as u8
        };
        image::Rgb([px(0), px(1), px(2)])
    })
}

fn cover(n: usize) -> RgbImage {
    std::env::var("LOSKI_README_COVERS")
        .ok()
        .and_then(|dir| image::open(Path::new(&dir).join(format!("{n}.jpg"))).ok())
        .map_or_else(|| drawn(n), |img| img.to_rgb8())
}

fn song(n: usize) -> Arc<TrackInfo> {
    let (title, artist, id, duration_ms) = SONGS[n];
    Arc::new(TrackInfo {
        id: SpotifyId::from_base62(id).expect("song id"),
        title: title.into(),
        artists: vec![artist.into()],
        album: if n == 0 {
            "Phobia".into()
        } else {
            String::new()
        },
        duration_ms,
        // The renderer finds the covers next to the cards.
        cover_url: Some(format!("covers/{n}.jpg")),
        cover_thumb_url: Some(format!("covers/{n}.jpg")),
        explicit: false,
        year: (n == 0).then_some(2006),
        from: (n == 0).then(|| "the playlist **Friday night**".into()),
        requested_by: None,
    })
}

fn save(dir: &Path, name: &str, card: &Component) {
    let json = serde_json::to_string_pretty(card).expect("card json");
    std::fs::write(dir.join(format!("{name}.json")), json).unwrap();
}

fn pictures(dir: &Path) {
    let jane = cover(0);
    let artwork = banner::render(
        &jane,
        &BannerText {
            title: SONGS[0].0.into(),
            artists: SONGS[0].1.into(),
            album: "Phobia".into(),
            year: Some(2006),
            duration_ms: SONGS[0].3,
            explicit: false,
        },
    )
    .expect("banner");
    std::fs::write(dir.join("banner.jpg"), artwork).unwrap();

    let minutes = [
        42, 65, 0, 38, 120, 95, 30, 0, 55, 80, 140, 160, 72, 0, 45, 90, 110, 64, 35, 20, 75, 130,
        185, 98, 60, 88, 70,
    ];
    let stats = StatsPicture {
        kicker: "OCTOBER SO FAR".into(),
        headline: "31h 12m".into(),
        subline: "of music · 412 plays · 96 songs · 41 artists".into(),
        chips: vec![
            "9-day streak".into(),
            "+4 hr vs Sep 1–27".into(),
            "Mostly alternative metal".into(),
        ],
        bars: minutes
            .iter()
            .enumerate()
            .map(|(i, m)| Bar {
                value: m * 60_000,
                label: match i {
                    0 => Some("Oct 1".into()),
                    26 => Some("Oct 27".into()),
                    _ => None,
                },
            })
            .collect(),
        songs: (0..5)
            .map(|n| Song {
                title: SONGS[n].0.into(),
                artist: SONGS[n].1.into(),
                plays: format!("{} plays", 23 - n * 3),
                cover: Some(cover(n)),
            })
            .collect(),
        artists: vec![
            Artist {
                name: "Breaking Benjamin".into(),
                detail: "41 plays · 2 hr 31 min".into(),
            },
            Artist {
                name: "Three Days Grace".into(),
                detail: "18 plays · 58 min".into(),
            },
            Artist {
                name: "Disturbed".into(),
                detail: "12 plays · 40 min".into(),
            },
        ],
    };
    std::fs::write(dir.join("stats.jpg"), picture::render(&stats)).unwrap();

    let tile = |label: &str, value: &str, detail: &str| Tile {
        label: label.into(),
        value: value.into(),
        detail: detail.into(),
    };
    let wrapped = WrappedPicture {
        kicker: "LOSKI WRAPPED · 2026".into(),
        headline: "41,230 minutes".into(),
        subline: "of music · 9,812 plays · 1,204 songs · 380 artists".into(),
        tiles: vec![
            tile(
                "TOP SONG",
                "The Diary of Jane",
                "Breaking Benjamin · 182 plays",
            ),
            tile("TOP ARTIST", "Breaking Benjamin", "912 plays · 61 hours"),
            tile("BIGGEST DAY", "Saturday, March 14", "5 hours 12 minutes"),
            tile("LONGEST STREAK", "23 days", "in a row with music"),
            tile("LISTENED WITH MOST", "Ana", "41 hours together"),
            tile(
                "TOP GENRE",
                "Alternative metal",
                "then post-grunge and hard rock",
            ),
        ],
        cover: Some(jane),
    };
    std::fs::write(dir.join("wrapped.jpg"), picture::render_wrapped(&wrapped)).unwrap();
}

fn cards(dir: &Path) {
    let icons = Icons::uploaded();
    let accent = color::dominant_of(&cover(0));
    let playing = Snapshot {
        track: Some(song(0)),
        status: Status::Playing,
        position_ms: 72_000,
        up_next: vec![song(1), song(2)],
        upcoming: 14,
        upcoming_ms: 52 * 60_000,
        volume: 70,
        repeat: Repeat::Off,
        crossfade_secs: 6,
        following: false,
        connect: false,
        filter: Filter::Off,
        sleep: None,
    };
    let room = RoomView {
        listeners: vec!["Alex".into(), "Ana".into(), "Vlad".into()],
        liked_by: vec!["Ana".into()],
        votes: None,
    };
    save(
        dir,
        "panel",
        &panel::now_playing_card(
            &icons,
            accent,
            &playing,
            Some("banner.jpg"),
            Look::default(),
            &room,
        ),
    );
    save(
        dir,
        "compact",
        &panel::now_playing_card(
            &icons,
            accent,
            &playing,
            None,
            Look {
                compact: true,
                staying: false,
            },
            &room,
        ),
    );

    // Sample text: the real lyrics aren't Loski's to put in a README.
    let lines = [
        "Lyrics show up here as the song plays,",
        "one line at a time, right on cue.",
        "The line before fades into the background,",
        "the one being sung stands out like this,",
        "and the next few wait underneath it",
        "so everyone in the call can sing along.",
        "(Sample text, not the song's lyrics.)",
    ];
    let lyrics = Lyrics {
        lines: lines
            .iter()
            .enumerate()
            .map(|(i, text)| Line {
                start_ms: Some(i as u32 * 4_000),
                text: text.to_string(),
            })
            .collect(),
        synced: true,
        instrumental: false,
        source: "Spotify".into(),
        color: Some(accent),
    };
    save(
        dir,
        "lyrics",
        &super::lyrics::live_card(&icons, Some(&song(0)), Some(&lyrics), Some(3)),
    );

    let queue = QueueView {
        current: Some(song(0)),
        status: Status::Playing,
        upcoming: (1..6).map(song).collect(),
        repeat: Repeat::Off,
        following: false,
        connect: false,
        duplicates: 0,
    };
    save(
        dir,
        "queue",
        &panel::queue_card(&icons, accent, &queue, 0, Some(2)),
    );

    let settings = SettingsView {
        panel_channel: Id::new_checked(MUSIC_CHANNEL),
        compact: false,
        always_on: true,
        staying_in: Id::new_checked(LOUNGE_CHANNEL),
        leave_after: Duration::from_secs(60),
        idle_leave: Duration::from_secs(300),
        crossfade_secs: 6,
        normalise: true,
        bitrate: AudioBitrate::Auto,
        timezone: "Europe/Bucharest".into(),
    };
    save(dir, "settings", &settings_card(&icons, &settings));

    let prefs = Prefs {
        co_djs: FRIENDS.iter().map(|(id, _)| *id).collect(),
        dj_role: Some(DJ_ROLE),
        ..Prefs::default()
    };
    save(
        dir,
        "friends",
        &super::friends::friends_card(&icons, &prefs),
    );

    save(
        dir,
        "eq",
        &eq_card(
            &icons,
            &EqView {
                gains: [4, 2, 0, 2, 4],
                on: true,
            },
            false,
        ),
    );

    let asked = |n: usize, by: usize, minutes_ago: i64| Request {
        nonce: n as u64,
        track: (*song(n)).clone(),
        by: FRIENDS[by].1.into(),
        by_id: FRIENDS[by].0,
        asked_at: NOW - minutes_ago * 60,
        at: std::time::Instant::now(),
    };
    let open = [asked(3, 0, 6), asked(4, 1, 3), asked(5, 0, 1)];
    save(dir, "requests", &requests_card(&icons, &open, None));

    let session = Recap {
        listened_ms: 2 * 3_600_000 + 14 * 60_000,
        plays: 31,
        started_at: NOW - 2 * 3_600 - 20 * 60,
        favourite: Some((song(0), 3)),
        artist: Some("Breaking Benjamin".into()),
        songs: (0..6).map(song).collect(),
        with: vec![("Ana".into(), 5_400_000), ("Vlad".into(), 2_700_000)],
        liked: vec![
            (song(0), vec!["Ana".into(), "Vlad".into()]),
            (song(2), vec!["Ana".into()]),
        ],
    };
    save(
        dir,
        "recap",
        &recap::recap_card(&icons, accent, &session, Some("Off I go. That was fun!")),
    );

    // What the renderer needs to know besides the cards.
    let names = serde_json::json!({
        "now": NOW,
        "channels": { MUSIC_CHANNEL.to_string(): "music", LOUNGE_CHANNEL.to_string(): "Lounge" },
        "users": FRIENDS.iter().map(|(id, name)| (id.to_string(), name.to_string())).collect::<std::collections::HashMap<_, _>>(),
        "roles": { DJ_ROLE.to_string(): "DJ" },
        "emoji": super::icons::Icon::ALL.iter().enumerate().map(|(n, icon)| ((n + 1).to_string(), icon.name())).collect::<std::collections::HashMap<_, _>>(),
    });
    std::fs::write(
        dir.join("names.json"),
        serde_json::to_string_pretty(&names).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore]
fn readme() {
    let Ok(out) = std::env::var("LOSKI_README_OUT") else {
        return;
    };
    let out = Path::new(&out);
    let card_dir = out.join("cards");
    std::fs::create_dir_all(&card_dir).unwrap();
    pictures(out);
    cards(&card_dir);
    let (setup, saved) = crate::setup::sample_pages(5588, "482913");
    std::fs::write(card_dir.join("setup.html"), setup).unwrap();
    std::fs::write(card_dir.join("setup-saved.html"), saved).unwrap();
}

//! Every card Loski sends, checked against Discord's limits as twilight
//! encodes them (component counts, text lengths, ids, nesting rules).

use std::sync::Arc;

use librespot_core::SpotifyId;
use twilight_model::channel::message::Component;

use chrono::TimeZone;

use super::{Pick, cards, commands, icons::Icons, mixes, panel, stats, status};
use crate::{
    engine::{Health, QueueView, Repeat, Snapshot, Status},
    spotify::web::{SearchItem, SearchKind, Target},
    stats::Range,
    store::{HistoryEntry, Mix, MixTrack},
    track::TrackInfo,
};

fn track(n: u8, long: bool) -> Arc<TrackInfo> {
    let title = if long {
        format!(
            "{n} {}",
            "An Extremely Long Song Title With *Markdown* ".repeat(6)
        )
    } else {
        format!("Song {n}")
    };
    Arc::new(TrackInfo {
        id: SpotifyId::from_raw(&[n; 16]).unwrap(),
        title,
        artists: vec!["Artist One".into(), "Artist_Two".into()],
        album: if long {
            "Album ".repeat(40)
        } else {
            "Album".into()
        },
        duration_ms: 213_000,
        cover_url: Some("https://i.scdn.co/image/ab67616d0000b273aaaa".into()),
        cover_thumb_url: Some("https://i.scdn.co/image/ab67616d00004851aaaa".into()),
        explicit: n % 2 == 0,
        year: Some(2011),
        from: Some("the playlist **Late night, long drives**".into()),
    })
}

fn text_len(component: &Component) -> usize {
    match component {
        Component::TextDisplay(t) => t.content.chars().count(),
        Component::Container(c) => c.components.iter().map(text_len).sum(),
        Component::Section(s) => s.components.iter().map(text_len).sum(),
        _ => 0,
    }
}

fn assert_valid(card: Component) {
    if let Err(e) = twilight_validate::message::components(std::slice::from_ref(&card), true) {
        panic!("invalid card: {e}\n{card:#?}");
    }
    // Discord also caps all the text in one message at 4,000 characters.
    assert!(
        text_len(&card) <= 4_000,
        "card text too long: {}",
        text_len(&card)
    );
}

fn snapshot(status: Status, following: bool, repeat: Repeat, long: bool) -> Snapshot {
    let up_next: Vec<_> = (2..5).map(|n| track(n, long)).collect();
    Snapshot {
        track: Some(track(1, long)),
        status,
        position_ms: 95_000,
        up_next,
        upcoming: 37,
        upcoming_ms: 7_400_000,
        volume: 70,
        repeat,
        crossfade_secs: 6,
        following,
        connect: false,
        filter: crate::dsp::Filter::BassBoost,
        sleep: Some(crate::engine::SleepState::In(
            std::time::Duration::from_secs(1500),
        )),
    }
}

#[test]
fn now_playing_card_is_valid_in_every_state() {
    let icons = Icons::default();
    for status in [
        Status::Idle,
        Status::Loading,
        Status::Playing,
        Status::Paused,
    ] {
        for following in [false, true] {
            for repeat in [Repeat::Off, Repeat::Track, Repeat::Queue] {
                for long in [false, true] {
                    for banner in [None, Some("attachment://banner.jpg")] {
                        assert_valid(panel::now_playing_card(
                            &icons,
                            0x336699,
                            &snapshot(status, following, repeat, long),
                            banner,
                        ));
                        let connect = Snapshot {
                            connect: true,
                            ..snapshot(status, false, repeat, long)
                        };
                        assert_valid(panel::now_playing_card(&icons, 0x336699, &connect, banner));
                    }
                }
            }
        }
    }
    let empty = Snapshot {
        track: None,
        up_next: Vec::new(),
        upcoming: 0,
        ..snapshot(Status::Idle, false, Repeat::Off, false)
    };
    assert_valid(panel::now_playing_card(&icons, 0x336699, &empty, None));
}

#[test]
fn queue_card_is_valid_on_every_page() {
    let icons = Icons::default();
    let view = QueueView {
        current: Some(track(1, true)),
        status: Status::Playing,
        upcoming: (2..40).map(|n| track(n, true)).collect(),
        repeat: Repeat::Queue,
        following: false,
        connect: false,
        duplicates: 3,
    };
    for page in 0..6 {
        for selected in [None, Some(1), Some(page * 10 + 3), Some(999)] {
            assert_valid(panel::queue_card(&icons, 0x112233, &view, page, selected));
        }
    }
    let empty = QueueView {
        current: None,
        status: Status::Idle,
        upcoming: Vec::new(),
        repeat: Repeat::Off,
        following: false,
        connect: false,
        duplicates: 0,
    };
    assert_valid(panel::queue_card(&icons, 0x112233, &empty, 0, None));
}

#[test]
fn history_and_toasts_are_valid() {
    let icons = Icons::default();
    let entries: Vec<_> = (0..15)
        .map(|n| HistoryEntry {
            played_at: 1_760_000_000 + n,
            uri: "spotify:track:4uLU6hMCjMI75M1A2tKUQC".into(),
            title: "A Song With A Fairly Long Name (Remastered 2011)".into(),
            artists: "Some Artist, Another Artist".into(),
            artist_list: Vec::new(),
            cover: None,
            listened_ms: Some(200_000),
        })
        .collect();
    let id = entries[0].track_id().to_string();
    for selected in [None, Some(id.as_str()), Some("gone")] {
        assert_valid(panel::history_card(&icons, 0x445566, &entries, selected));
    }
    assert_valid(panel::history_card(&icons, 0x445566, &[], None));
    assert_valid(cards::toast(
        &icons,
        "Queued **Song**",
        Some("https://i.scdn.co/image/x"),
        0x23A55A,
    ));
    assert_valid(cards::toast(&icons, "Queued **Song**", None, 0x23A55A));
    assert_valid(cards::info(&icons, "Paused."));
    assert_valid(cards::error(&icons, "You're not in a voice channel."));
}

#[test]
fn search_picker_is_valid() {
    let icons = Icons::default();
    let mut pick = Pick {
        query: "a very long search query ".repeat(20),
        kind: SearchKind::Songs,
        items: (1..=5)
            .map(|n| SearchItem::from_track((*track(n, true)).clone()))
            .collect(),
        selected: 0,
        created: std::time::Instant::now(),
    };
    for selected in 0..5 {
        pick.selected = selected;
        assert_valid(commands::picker_card(&icons, &pick, u64::MAX));
    }
    pick.kind = SearchKind::Albums;
    pick.items = vec![SearchItem {
        title: "Hurry Up, We're Dreaming".into(),
        subtitle: "M83 · 2011 · 22 songs".into(),
        cover: None,
        explicit: false,
        target: Target::Album(librespot_core::SpotifyId::from_raw(&[3; 16]).unwrap()),
    }];
    pick.selected = 0;
    assert_valid(commands::picker_card(&icons, &pick, u64::MAX));
    pick.items.clear();
    assert_valid(commands::picker_card(&icons, &pick, u64::MAX));
}

#[test]
fn help_card_is_valid() {
    assert_valid(commands::help_card(&Icons::default()));
}

#[test]
fn command_definitions_are_valid() {
    let definitions = commands::definitions();
    assert_eq!(definitions.len(), 33);
    for command in &definitions {
        if let Err(e) = twilight_validate::command::command(command) {
            panic!("/{} is invalid: {e}", command.name);
        }
    }
}

/// A busy month of listening with long, markdown-heavy names.
fn busy_history() -> Vec<HistoryEntry> {
    let tz = chrono_tz::Europe::Bucharest;
    (0..600u32)
        .map(|n| HistoryEntry {
            played_at: tz
                .with_ymd_and_hms(2026, 9 + n % 2, 1 + n % 28, n % 24, 0, 0)
                .unwrap()
                .timestamp(),
            uri: format!("spotify:track:{:0>22}", n % 40),
            title: format!("{n} {}", "A *Very* Long_Song Title [Remastered] ".repeat(4)),
            artists: "Artist One, Artist_Two, Artist Three".into(),
            artist_list: vec![
                format!("Artist {} With A Long Name **", n % 9),
                "Second Artist".into(),
            ],
            cover: Some("https://i.scdn.co/image/ab67616d00004851aaaa".into()),
            listened_ms: Some(180_000 + n as u64 * 1000),
        })
        .collect()
}

#[test]
fn stats_cards_are_valid() {
    let icons = Icons::default();
    let now = chrono_tz::Europe::Bucharest
        .with_ymd_and_hms(2026, 10, 21, 20, 0, 0)
        .unwrap();
    let history = busy_history();
    for range in Range::TABS {
        for entries in [&history[..], &[]] {
            let stats = crate::stats::compute(entries, range, now);
            assert_valid(stats::stats_card(&icons, &stats, None));
            assert_valid(stats::stats_card(
                &icons,
                &stats,
                Some("attachment://stats.jpg"),
            ));
        }
    }
    let recap = crate::stats::compute(
        &history,
        Range::WholeMonth {
            year: 2026,
            month: 9,
        },
        now,
    );
    assert!(recap.plays > 0);
    assert_valid(stats::recap_card(&icons, &recap, None));
    assert_valid(stats::recap_card(
        &icons,
        &recap,
        Some("attachment://stats.jpg"),
    ));
}

fn mix(n: usize, songs: usize) -> Mix {
    Mix {
        name: format!("{n} {}", "a *long* mix name ".repeat(3))
            .chars()
            .take(40)
            .collect(),
        saved_at: 1_760_000_000,
        tracks: (0..songs)
            .map(|i| MixTrack {
                id: "4uLU6hMCjMI75M1A2tKUQC".into(),
                title: format!("A *long* song title number {i} (Live at Wembley)"),
                artists: "Some Artist".into(),
                duration_ms: 213_000,
            })
            .collect(),
    }
}

#[test]
fn mix_list_is_valid_however_many_there_are() {
    let icons = Icons::default();
    for count in [0, 1, 10, 11, 25] {
        let mixes: Vec<Mix> = (0..count).map(|n| mix(n, 500)).collect();
        assert_valid(mixes::list_card(&icons, &mixes, None));
        if let Some(first) = mixes.first() {
            assert_valid(mixes::list_card(&icons, &mixes, Some(&first.name)));
        }
    }
}

#[test]
fn status_card_is_valid_in_every_state() {
    let icons = Icons::default();
    let health = |status| Health {
        status,
        mode: "Discord queue",
        buffered_ms: 20,
        buffer_capacity_ms: 500,
        connect_ready: false,
    };
    let reports = [
        status::Report {
            spotify: status::Spotify::Connected {
                username: "luca".into(),
                plan: Some("premium".into()),
                country: "RO".into(),
            },
            web_api: Some(Ok(std::time::Duration::from_millis(140))),
            gateway_ping: Some(std::time::Duration::from_millis(42)),
            voice: Some(status::VoiceReport {
                channel: "General".into(),
                bitrate_kbps: 128,
                region: Some("Frankfurt".into()),
            }),
            health: Some(health(Status::Playing)),
            uptime: std::time::Duration::from_secs(11_000),
            memory_mb: Some(58),
            timezone: "Europe/Bucharest".into(),
            device_name: "Loski".into(),
        },
        status::Report {
            spotify: status::Spotify::Reconnecting,
            web_api: Some(Err("x".repeat(1500))),
            gateway_ping: None,
            voice: None,
            health: Some(health(Status::Idle)),
            uptime: std::time::Duration::ZERO,
            memory_mb: None,
            timezone: "UTC".into(),
            device_name: "Loski".into(),
        },
        status::Report {
            spotify: status::Spotify::LoggedOut,
            web_api: None,
            gateway_ping: None,
            voice: None,
            health: None,
            uptime: std::time::Duration::ZERO,
            memory_mb: None,
            timezone: "UTC".into(),
            device_name: "Loski".into(),
        },
    ];
    for report in &reports {
        assert_valid(status::status_card(&icons, report));
    }
}

#[test]
fn recap_card_is_valid() {
    let icons = Icons::default();
    let plays: Vec<crate::engine::Played> = (0..60u8)
        .map(|n| crate::engine::Played {
            track: track(n % 7, true),
            started_at: 1_760_000_000 + n as i64 * 200,
            listened_ms: 190_000,
        })
        .collect();
    let recap = super::recap::Recap::from_plays(&plays).unwrap();
    assert!(recap.favourite.is_some());
    for farewell in [None, Some("Off I go. That was a good one.")] {
        assert_valid(super::recap::recap_card(&icons, 0x334455, &recap, farewell));
    }
    let one = super::recap::Recap::from_plays(&plays[..1]).unwrap();
    assert_valid(super::recap::recap_card(&icons, 0x334455, &one, None));
}

#[test]
fn request_card_is_valid() {
    let icons = Icons::default();
    for long in [false, true] {
        let song = (*track(4, long)).clone();
        let by = if long {
            "A Very Long Nickname_With *Stars* ".repeat(3)
        } else {
            "Ana".to_string()
        };
        assert_valid(super::requests::request_card(&icons, &song, &by, u64::MAX));
    }
}

//! Every card Loski sends, checked against Discord's limits as twilight
//! encodes them (component counts, text lengths, ids, nesting rules).

use std::sync::Arc;

use librespot_core::SpotifyId;
use twilight_model::channel::message::Component;

use chrono::TimeZone;

use super::{Pick, cards, commands, icons::Icons, mixes, panel, settings, sound, stats, status};
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
        requested_by: None,
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
                        for (look, room) in looks().into_iter().zip(rooms(long)) {
                            assert_valid(panel::now_playing_card(
                                &icons,
                                0x336699,
                                &snapshot(status, following, repeat, long),
                                banner,
                                look,
                                &room,
                            ));
                            let connect = Snapshot {
                                connect: true,
                                filter: crate::dsp::Filter::Mine([4, 2, 0, -2, 8]),
                                ..snapshot(status, false, repeat, long)
                            };
                            assert_valid(panel::now_playing_card(
                                &icons, 0x336699, &connect, banner, look, &room,
                            ));
                        }
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
    for (look, room) in looks().into_iter().zip(rooms(true)) {
        assert_valid(panel::now_playing_card(
            &icons, 0x336699, &empty, None, look, &room,
        ));
    }
}

/// Nobody (Loski isn't in voice), a few people, and a crowded call.
fn rooms(long: bool) -> [super::friends::RoomView; 3] {
    let names = |n: usize| -> Vec<String> {
        (0..n)
            .map(|i| {
                if long {
                    format!("{i} A Very Long Nickname_With *Stars* and more")
                } else {
                    format!("Friend {i}")
                }
            })
            .collect()
    };
    [
        super::friends::RoomView::default(),
        super::friends::RoomView {
            listeners: names(3),
            liked_by: names(1),
            votes: None,
        },
        super::friends::RoomView {
            listeners: names(25),
            liked_by: names(25),
            votes: Some((7, 13)),
        },
    ]
}

fn looks() -> [panel::Look; 3] {
    [
        panel::Look::default(),
        panel::Look {
            compact: true,
            staying: false,
            ..Default::default()
        },
        panel::Look {
            compact: true,
            staying: true,
            ..Default::default()
        },
    ]
}

#[test]
fn settings_and_my_sound_cards_are_valid() {
    use crate::config::AudioBitrate;
    use std::time::Duration;
    let icons = Icons::default();
    let channel = twilight_model::id::Id::new(42);
    for always_on in [false, true] {
        for (bitrate, leave, crossfade) in [
            (AudioBitrate::Auto, 60, 0),
            (AudioBitrate::Fixed(160_000), 45, 5),
            (AudioBitrate::Fixed(384_000), 0, 12),
        ] {
            let view = settings::SettingsView {
                panel_channel: always_on.then_some(channel),
                compact: always_on,
                style: if crossfade == 5 {
                    crate::banner::Style::Vinyl
                } else {
                    crate::banner::Style::Minimal
                },
                always_on,
                staying_in: always_on.then_some(channel),
                leave_after: Duration::from_secs(leave),
                idle_leave: Duration::from_secs(7_200),
                crossfade_secs: crossfade,
                normalise: always_on,
                bitrate,
                timezone: "America/Argentina/ComodRivadavia".into(),
            };
            assert_valid(settings::settings_card(&icons, &view));
        }
    }
    for gains in [[0; 5], [8, -8, 2, -2, 6]] {
        for on in [false, true] {
            for from_settings in [false, true] {
                let view = sound::EqView { gains, on };
                assert_valid(sound::eq_card(&icons, &view, from_settings));
            }
        }
    }
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
    let last = view.upcoming.len();
    for page in 0..6 {
        for selected in [None, Some(1), Some(page * 10 + 3), Some(last), Some(999)] {
            assert_valid(panel::queue_card(&icons, 0x112233, &view, page, selected));
        }
    }
    // A song that moves keeps its page in view.
    assert_eq!(panel::queue_page_of(1), 0);
    assert_eq!(panel::queue_page_of(10), 0);
    assert_eq!(panel::queue_page_of(11), 1);
    let one = QueueView {
        upcoming: vec![track(2, false)],
        ..view
    };
    assert_valid(panel::queue_card(&icons, 0x112233, &one, 0, Some(1)));
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
            title: "A Song With A Fairly Long Name (Remastered 2011) - Deluxe Edition Bonus".into(),
            artists: "Some Artist, Another Artist, And A Third Artist Too".into(),
            artist_list: Vec::new(),
            // The newest ones show their covers; some songs have none.
            cover: (n % 3 != 1).then(|| "https://i.scdn.co/image/ab67616d00004851cover".into()),
            listened_ms: Some(200_000),
            with: Vec::new(),
            requested_by: (n % 4 == 0).then_some(9),
        })
        .collect();
    let id = entries[0].track_id().to_string();
    let about = "“a *very* long search”".repeat(4) + " · the last 7 days · with **Ana**";
    for search in [None, Some((about.as_str(), 12_345))] {
        for selected in [None, Some(id.as_str()), Some("gone")] {
            assert_valid(panel::history_card(
                &icons, 0x445566, &entries, selected, search,
            ));
        }
        assert_valid(panel::history_card(&icons, 0x445566, &[], None, search));
    }
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
    use super::{
        friends::Access,
        help::{HelpView, Mentions, Topic, found_card, home_card, topic_card},
    };
    let icons = Icons::default();
    let ids = commands::definitions("Loski")
        .into_iter()
        .enumerate()
        .map(|(i, c)| (c.name, twilight_model::id::Id::new(1_000_000 + i as u64)))
        .collect();
    let mut mentions = Some(Mentions::new(ids));
    for access in [Access::Listener, Access::Queue, Access::Full, Access::Owner] {
        for in_server in [true, false] {
            for bot in ["Loski", "DJ ✨ Bob", "Thirty Two Characters Long Name!"] {
                let view = HelpView {
                    bot: bot.to_string(),
                    access,
                    in_server,
                    mentions: mentions.take().unwrap_or_default(),
                };
                assert_valid(home_card(&icons, &view));
                for topic in Topic::ALL {
                    assert_valid(topic_card(&icons, &view, topic));
                }
                for query in ["skip a few songs", "zzz", &"a very long query ".repeat(10)] {
                    assert_valid(found_card(&icons, &view, query));
                }
            }
        }
    }
}

#[test]
fn command_definitions_are_valid() {
    // Any name Discord allows a bot: long ones, emoji, dots.
    for bot in [
        "Loski",
        "DJ Bob",
        "DJ ✨ Bob",
        "Bob.exe",
        "Thirty Two Characters Long Name!",
    ] {
        let definitions = commands::definitions(bot);
        assert_eq!(definitions.len(), 40);
        for command in &definitions {
            if let Err(e) = twilight_validate::command::command(command) {
                panic!("/{} is invalid for {bot}: {e}", command.name);
            }
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
            with: vec![n as u64 % 3 + 1],
            requested_by: None,
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
    // Every artist with a few long genres.
    let genres: crate::genres::GenreMap = (0..9)
        .map(|n| {
            (
                format!("artist {n} with a long name **"),
                vec![format!("{n} very *long* genre name indeed"); 4],
            )
        })
        .collect();
    for range in Range::TABS {
        for entries in [&history[..], &[]] {
            let stats = crate::stats::compute(entries, range, now, &genres);
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
        &genres,
    );
    assert!(recap.plays > 0);
    assert_valid(stats::recap_card(&icons, &recap, None));
    assert_valid(stats::recap_card(
        &icons,
        &recap,
        Some("attachment://stats.jpg"),
    ));
    assert!(!recap.top_genres.is_empty() && !recap.together.is_empty());

    let name = |id: u64| format!("Friend_{id} with a *really* long nickname");
    let all = crate::stats::compute(&history, Range::AllTime, now, &genres);
    assert_valid(stats::together_card(&icons, &all.together, name));
    assert_valid(stats::together_card(&icons, &[], name));

    let year = crate::stats::compute(&history, Range::Year(2026), now, &genres);
    for pictures in [
        &[][..],
        &["attachment://wrapped.jpg"][..],
        &["attachment://wrapped.jpg", "attachment://stats.jpg"][..],
    ] {
        for so_far in [false, true] {
            assert_valid(stats::wrapped_card(
                &icons, "Loski", &year, 2026, so_far, pictures, name,
            ));
        }
    }
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
            newer: Some("2.1.0".into()),
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
            newer: None,
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
            newer: None,
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
            with: (0..n as u64 % 12).collect(),
        })
        .collect();
    let mut recap =
        super::recap::Recap::from_plays(&plays, |id| format!("Friend_{id} *with a long name*"))
            .unwrap();
    recap.liked = (0..5)
        .map(|n| (track(n, true), vec!["Ana".to_string(), "Vlad *".repeat(20)]))
        .collect();
    assert!(recap.favourite.is_some());
    for farewell in [None, Some("Off I go. That was a good one.")] {
        assert_valid(super::recap::recap_card(&icons, 0x334455, &recap, farewell));
    }
    let one = super::recap::Recap::from_plays(&plays[..1], |_| String::new()).unwrap();
    assert_valid(super::recap::recap_card(&icons, 0x334455, &one, None));
}

#[test]
fn requests_card_is_valid() {
    use super::requests::{Request, requests_card};
    let icons = Icons::default();
    for long in [false, true] {
        let open: Vec<Request> = (0..30u8)
            .map(|n| Request {
                nonce: u64::MAX - n as u64,
                track: (*track(n, long)).clone(),
                by: if long {
                    "A Very Long Nickname_With *Stars* ".repeat(3)
                } else {
                    "Ana".to_string()
                },
                by_id: n as u64,
                asked_at: 1_760_000_000,
                at: std::time::Instant::now(),
                votes: (0..n as u64 % 4).collect(),
            })
            .collect();
        for count in [1, 2, 6, 7, 30] {
            for party in [false, true] {
                assert_valid(requests_card(&icons, &open[..count], None, party));
                assert_valid(requests_card(
                    &icons,
                    &open[..count],
                    Some(open[0].nonce),
                    party,
                ));
            }
        }
    }
}

#[test]
fn friends_card_is_valid() {
    let icons = Icons::default();
    let mut prefs = crate::store::Prefs::default();
    assert_valid(super::friends::friends_card(&icons, &prefs));
    prefs.co_djs = (1..=10).collect();
    prefs.dj_role = Some(77);
    prefs.djs_queue_only = true;
    prefs.vote_skip_off = true;
    assert_valid(super::friends::friends_card(&icons, &prefs));
}

#[test]
fn update_card_is_valid() {
    let icons = Icons::default();
    for body in [
        None,
        Some("## What's new\n".to_string() + &"- A *long* change line\n".repeat(200)),
    ] {
        let release = super::updates::Release {
            tag_name: "v2.1.0".into(),
            body,
            html_url: "https://github.com/LucaBarbaLata/Loski/releases/tag/v2.1.0".into(),
        };
        assert_valid(super::updates::update_card(&icons, &release));
    }
}

#[test]
fn lyrics_cards_are_valid() {
    use super::lyrics::{Line, Lyrics, live_card, page_card};
    let icons = Icons::default();
    let song = (*track(5, true)).clone();
    let synced = Lyrics {
        lines: (0..80)
            .map(|i| Line {
                start_ms: Some(i * 3_000),
                text: if i % 9 == 0 {
                    String::new()
                } else {
                    format!("A *made up* line number {i} {}", "la ".repeat(40))
                },
            })
            .collect(),
        synced: true,
        instrumental: false,
        source: "Musixmatch".into(),
        color: Some(0x335577),
    };
    let unsynced = Lyrics {
        synced: false,
        ..synced.clone()
    };
    let instrumental = Lyrics {
        lines: Vec::new(),
        instrumental: true,
        ..synced.clone()
    };
    for line in [None, Some(0), Some(40), Some(79)] {
        assert_valid(live_card(&icons, Some(&song), Some(&synced), line));
    }
    assert_valid(live_card(&icons, Some(&song), Some(&unsynced), None));
    assert_valid(live_card(&icons, Some(&song), Some(&instrumental), None));
    assert_valid(live_card(&icons, Some(&song), None, None));
    assert_valid(live_card(&icons, None, None, None));
    for page in [0, 1, 99] {
        assert_valid(page_card(&icons, &song, &unsynced, page));
    }
    assert_valid(page_card(&icons, &song, &instrumental, 0));
}

#[test]
fn login_card_is_valid() {
    use super::login::{AGAIN, FIRST, login_card};
    let link = |n: u8| {
        format!(
            "https://accounts.spotify.com/authorize?client_id={}&state=x{n}",
            "a".repeat(300)
        )
    };
    for links in [
        (Some(link(1)), Some(link(2))),
        (None, Some(link(2))),
        (Some(link(1)), None),
        (None, None),
    ] {
        for intro in [FIRST, AGAIN] {
            assert_valid(login_card(intro, "http://127.0.0.1:5588/", &links));
        }
    }
}

#[test]
fn customize_card_is_valid() {
    use super::customize::{HereView, ProfileView, customize_card};
    let icons = Icons::default();
    let view = |banner: bool, here: Option<HereView>, long: bool| ProfileView {
        name: if long {
            "Thirty Two Characters Long Name!"
        } else {
            "Loski"
        }
        .to_string(),
        about: if long {
            "*About* me ".repeat(60)
        } else {
            String::new()
        },
        picture: "https://cdn.discordapp.com/avatars/1/abc.png?size=256".to_string(),
        banner: banner
            .then(|| "https://cdn.discordapp.com/banners/1/abc.png?size=1024".to_string()),
        default_name: "Loski".to_string(),
        here,
    };
    let dressed = || HereView {
        nick: Some("DJ *Bob*".to_string()),
        picture: true,
        banner: true,
    };
    for long in [false, true] {
        for banner in [false, true] {
            assert_valid(customize_card(&icons, &view(banner, None, long)));
            assert_valid(customize_card(
                &icons,
                &view(banner, Some(HereView::default()), long),
            ));
            assert_valid(customize_card(&icons, &view(banner, Some(dressed()), long)));
        }
    }
}

#[test]
fn party_mode_plays_the_most_voted_first() {
    use super::requests::{Request, party_order};
    let request = |n: u8, votes: &[u64]| Request {
        nonce: n as u64,
        track: (*track(n, false)).clone(),
        by: "Ana".to_string(),
        by_id: 1,
        asked_at: 1_760_000_000,
        at: std::time::Instant::now(),
        votes: votes.iter().copied().collect(),
    };
    // Asked in order 0, 1, 2, 3.
    let open = [
        request(0, &[1]),
        request(1, &[1, 2, 3]),
        request(2, &[1]),
        request(3, &[1, 2, 3]),
    ];
    let order: Vec<u64> = party_order(&open).iter().map(|r| r.nonce).collect();
    assert_eq!(order, [1, 3, 0, 2]);
}

#[test]
fn voice_card_is_valid() {
    use super::hearing::{VoiceView, voice_card};
    let icons = Icons::default();
    for owner in [false, true] {
        for (on, busy, cpu_ok) in [
            (false, false, true),
            (false, true, true),
            (true, false, true),
            (false, false, false),
        ] {
            for me in [false, true] {
                for heard_here in [Vec::new(), vec!["Luca".to_string(), "David *DJ*".repeat(9)]] {
                    let view = VoiceView {
                        bot: "Thirty Two Characters Long Name!".to_string(),
                        owner,
                        on,
                        busy,
                        cpu_ok,
                        me,
                        heard_here,
                    };
                    assert_valid(voice_card(&icons, &view));
                }
            }
        }
    }
}

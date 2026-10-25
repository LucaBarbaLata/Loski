//! Listening stats from the history: totals, top songs and artists, streaks
//! and when you listen, counted in calendar days of your own timezone.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, Days, Months, NaiveDate, TimeZone, Timelike};
use chrono_tz::Tz;

use crate::store::HistoryEntry;

/// Songs played before listening time was tracked count as an average song.
const LEGACY_LISTEN_MS: u64 = 210_000;
/// "All time" charts at most this many months.
const MAX_MONTH_BARS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    /// The last seven days, today included.
    Week,
    /// This calendar month so far.
    Month,
    AllTime,
    /// One whole calendar month, for the monthly recap.
    WholeMonth {
        year: i32,
        month: u32,
    },
}

impl Range {
    pub const TABS: [Range; 3] = [Range::Week, Range::Month, Range::AllTime];

    pub fn key(self) -> &'static str {
        match self {
            Range::Week => "week",
            Range::Month => "month",
            Range::AllTime => "all",
            Range::WholeMonth { .. } => "recap",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        Self::TABS.into_iter().find(|r| r.key() == key)
    }

    pub fn label(self) -> &'static str {
        match self {
            Range::Week => "Week",
            Range::Month => "Month",
            Range::AllTime => "All time",
            Range::WholeMonth { .. } => "Recap",
        }
    }
}

#[derive(Debug, Clone)]
pub struct TopSong {
    pub uri: String,
    pub title: String,
    pub artists: String,
    pub cover: Option<String>,
    pub plays: u32,
    pub listened_ms: u64,
}

impl TopSong {
    pub fn track_id(&self) -> &str {
        self.uri.rsplit(':').next().unwrap_or_default()
    }
}

#[derive(Debug, Clone)]
pub struct TopArtist {
    pub name: String,
    pub plays: u32,
    pub listened_ms: u64,
}

/// One bar of the activity chart: a day, or a month for "all time".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    pub first_day: NaiveDate,
    pub listened_ms: u64,
}

#[derive(Debug, Clone)]
pub struct Stats {
    pub range: Range,
    /// The days counted, both included.
    pub first_day: NaiveDate,
    pub last_day: NaiveDate,
    pub plays: u32,
    pub listened_ms: u64,
    pub songs: usize,
    pub artists: usize,
    /// Every song and artist in the range, most played first.
    pub top_songs: Vec<TopSong>,
    pub top_artists: Vec<TopArtist>,
    pub bars: Vec<Bar>,
    /// Bars are months rather than days.
    pub monthly_bars: bool,
    /// Days in a row with music, up to today (or yesterday, if today is
    /// still quiet). Counted over the whole history.
    pub streak: u32,
    /// The longest run of days with music inside the range.
    pub best_streak: u32,
    /// The hour of the day you listen most, 0 to 23.
    pub peak_hour: Option<u32>,
    /// The same stretch just before this one, and how much you listened then.
    pub previous: Option<(NaiveDate, NaiveDate, u64)>,
}

struct Play<'a> {
    entry: &'a HistoryEntry,
    day: NaiveDate,
    hour: u32,
    ms: u64,
}

fn listened(entry: &HistoryEntry) -> u64 {
    entry.listened_ms.unwrap_or(LEGACY_LISTEN_MS)
}

fn first_of_month(day: NaiveDate) -> NaiveDate {
    day.with_day(1).unwrap_or(day)
}

fn last_of_month(day: NaiveDate) -> NaiveDate {
    first_of_month(day)
        .checked_add_months(Months::new(1))
        .and_then(|d| d.pred_opt())
        .unwrap_or(day)
}

fn days(first: NaiveDate, last: NaiveDate) -> impl Iterator<Item = NaiveDate> {
    first.iter_days().take_while(move |d| *d <= last)
}

/// The days a range covers and the stretch before it to compare with.
type Span = (NaiveDate, NaiveDate, Option<(NaiveDate, NaiveDate)>);

fn span(range: Range, today: NaiveDate, first_play: Option<NaiveDate>) -> Span {
    match range {
        Range::Week => {
            let first = today - Days::new(6);
            let previous = (first - Days::new(7), first - Days::new(1));
            (first, today, Some(previous))
        }
        Range::Month => {
            let first = first_of_month(today);
            let previous = first
                .checked_sub_months(Months::new(1))
                .zip(today.checked_sub_months(Months::new(1)));
            (first, today, previous)
        }
        Range::AllTime => (first_play.unwrap_or(today).min(today), today, None),
        Range::WholeMonth { year, month } => {
            let first = NaiveDate::from_ymd_opt(year, month, 1).unwrap_or(today);
            let previous = first
                .checked_sub_months(Months::new(1))
                .map(|p| (p, last_of_month(p)));
            (first, last_of_month(first), previous)
        }
    }
}

fn longest_run(active: &HashSet<NaiveDate>, first: NaiveDate, last: NaiveDate) -> u32 {
    let (mut best, mut run) = (0, 0);
    for day in days(first, last) {
        run = if active.contains(&day) { run + 1 } else { 0 };
        best = best.max(run);
    }
    best
}

fn current_streak(active: &HashSet<NaiveDate>, today: NaiveDate) -> u32 {
    let mut day = if active.contains(&today) {
        today
    } else {
        match today.pred_opt() {
            Some(d) => d,
            None => return 0,
        }
    };
    let mut streak = 0;
    while active.contains(&day) {
        streak += 1;
        match day.pred_opt() {
            Some(d) => day = d,
            None => break,
        }
    }
    streak
}

pub fn compute(history: &[HistoryEntry], range: Range, now: DateTime<Tz>) -> Stats {
    let tz = now.timezone();
    let today = now.date_naive();
    let plays: Vec<Play> = history
        .iter()
        .filter_map(|entry| {
            let at = tz.timestamp_opt(entry.played_at, 0).single()?;
            Some(Play {
                entry,
                day: at.date_naive(),
                hour: at.hour(),
                ms: listened(entry),
            })
        })
        .collect();

    let active: HashSet<NaiveDate> = plays.iter().map(|p| p.day).collect();
    let first_play = plays.iter().map(|p| p.day).min();
    let (first_day, last_day, previous) = span(range, today, first_play);
    let inside = |p: &&Play| p.day >= first_day && p.day <= last_day;

    let mut songs: HashMap<&str, TopSong> = HashMap::new();
    let mut artists: HashMap<&str, TopArtist> = HashMap::new();
    let mut hours = [0u64; 24];
    let mut by_day: HashMap<NaiveDate, u64> = HashMap::new();
    let (mut count, mut total) = (0u32, 0u64);
    for play in plays.iter().filter(inside) {
        let entry = play.entry;
        count += 1;
        total += play.ms;
        hours[play.hour as usize] += play.ms;
        *by_day.entry(play.day).or_default() += play.ms;

        let song = songs.entry(&entry.uri).or_insert_with(|| TopSong {
            uri: entry.uri.clone(),
            title: String::new(),
            artists: String::new(),
            cover: None,
            plays: 0,
            listened_ms: 0,
        });
        song.plays += 1;
        song.listened_ms += play.ms;
        // Later plays carry the freshest name and cover.
        song.title.clone_from(&entry.title);
        song.artists.clone_from(&entry.artists);
        if entry.cover.is_some() {
            song.cover.clone_from(&entry.cover);
        }

        for name in entry.artist_names() {
            let artist = artists.entry(name).or_insert_with(|| TopArtist {
                name: name.to_string(),
                plays: 0,
                listened_ms: 0,
            });
            artist.plays += 1;
            artist.listened_ms += play.ms;
        }
    }

    let distinct_songs = songs.len();
    let distinct_artists = artists.len();
    let mut top_songs: Vec<TopSong> = songs.into_values().collect();
    top_songs.sort_by(|a, b| {
        (b.plays, b.listened_ms, &a.title).cmp(&(a.plays, a.listened_ms, &b.title))
    });
    let mut top_artists: Vec<TopArtist> = artists.into_values().collect();
    top_artists
        .sort_by(|a, b| (b.plays, b.listened_ms, &a.name).cmp(&(a.plays, a.listened_ms, &b.name)));

    // All time goes by month once there are a couple of months to show.
    let monthly_bars = range == Range::AllTime && (last_day - first_day).num_days() > 62;
    let bars = if monthly_bars {
        let mut months: Vec<NaiveDate> = Vec::new();
        let mut month = first_of_month(first_day);
        while month <= last_day {
            months.push(month);
            match month.checked_add_months(Months::new(1)) {
                Some(next) => month = next,
                None => break,
            }
        }
        let skip = months.len().saturating_sub(MAX_MONTH_BARS);
        months
            .into_iter()
            .skip(skip)
            .map(|month| Bar {
                first_day: month,
                listened_ms: by_day
                    .iter()
                    .filter(|(day, _)| first_of_month(**day) == month)
                    .map(|(_, ms)| ms)
                    .sum(),
            })
            .collect()
    } else {
        days(first_day, last_day)
            .map(|day| Bar {
                first_day: day,
                listened_ms: by_day.get(&day).copied().unwrap_or(0),
            })
            .collect()
    };

    let peak_hour = (total > 0).then(|| {
        (0..24u32)
            .max_by_key(|&h| (hours[h as usize], std::cmp::Reverse(h)))
            .unwrap_or(0)
    });

    let previous = previous.map(|(first, last)| {
        let ms = plays
            .iter()
            .filter(|p| p.day >= first && p.day <= last)
            .map(|p| p.ms)
            .sum();
        (first, last, ms)
    });

    Stats {
        range,
        first_day,
        last_day,
        plays: count,
        listened_ms: total,
        songs: distinct_songs,
        artists: distinct_artists,
        top_songs,
        top_artists,
        bars,
        monthly_bars,
        streak: current_streak(&active, today),
        best_streak: longest_run(&active, first_day, last_day),
        peak_hour,
        previous,
    }
}

/// The month to send a recap for, as "YYYY-MM", once a new month has begun.
/// `last_sent` is the month the previous recap covered.
pub fn recap_due(now: DateTime<Tz>, last_sent: Option<&str>) -> Option<(i32, u32, String)> {
    let previous = first_of_month(now.date_naive()).checked_sub_months(Months::new(1))?;
    let key = format!("{:04}-{:02}", previous.year(), previous.month());
    (last_sent != Some(key.as_str())).then(|| (previous.year(), previous.month(), key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono_tz::Europe::Bucharest;

    fn at(day: u32, hour: u32) -> i64 {
        Bucharest
            .with_ymd_and_hms(2026, 10, day, hour, 0, 0)
            .unwrap()
            .timestamp()
    }

    fn entry(played_at: i64, id: &str, artists: &[&str], minutes: u64) -> HistoryEntry {
        HistoryEntry {
            played_at,
            uri: format!("spotify:track:{id}"),
            title: format!("Song {id}"),
            artists: artists.join(", "),
            artist_list: artists.iter().map(|a| a.to_string()).collect(),
            cover: None,
            listened_ms: Some(minutes * 60_000),
        }
    }

    fn history() -> Vec<HistoryEntry> {
        vec![
            entry(at(1, 12), "a", &["Breaking Benjamin"], 4),
            entry(at(14, 22), "b", &["Saliva"], 3),
            entry(at(15, 23), "a", &["Breaking Benjamin"], 4),
            entry(
                at(16, 23),
                "c",
                &["Three Days Grace", "Breaking Benjamin"],
                3,
            ),
            entry(at(17, 22), "a", &["Breaking Benjamin"], 4),
            // Still the 18th in UTC, but the 19th where you are.
            Bucharest
                .with_ymd_and_hms(2026, 10, 19, 1, 30, 0)
                .map(|t| entry(t.timestamp(), "b", &["Saliva"], 3))
                .unwrap(),
            entry(at(20, 21), "d", &["Tyler, The Creator"], 2),
            entry(at(21, 10), "a", &["Breaking Benjamin"], 4),
        ]
    }

    fn now() -> DateTime<Tz> {
        Bucharest.with_ymd_and_hms(2026, 10, 21, 18, 0, 0).unwrap()
    }

    #[test]
    fn week_counts_the_last_seven_local_days() {
        let stats = compute(&history(), Range::Week, now());
        assert_eq!(stats.first_day.day(), 15);
        assert_eq!(stats.bars.len(), 7);
        assert_eq!(stats.plays, 6);
        assert_eq!(stats.listened_ms, 20 * 60_000);
        assert_eq!(stats.top_songs[0].uri, "spotify:track:a");
        assert_eq!(stats.top_songs[0].plays, 3);
        assert_eq!(stats.top_artists[0].name, "Breaking Benjamin");
        assert_eq!(stats.top_artists[0].plays, 4);
        // "Tyler, The Creator" stays one artist.
        assert!(
            stats
                .top_artists
                .iter()
                .any(|a| a.name == "Tyler, The Creator")
        );
        assert_eq!(stats.artists, 4);
        // The 18th was quiet: the run since the 19th is three days.
        assert_eq!(stats.streak, 3);
        assert_eq!(stats.best_streak, 3);
        assert_eq!(stats.peak_hour, Some(23));
        // 8 to 14 October: only the 14th had music.
        assert_eq!(stats.previous.map(|p| p.2), Some(3 * 60_000));
    }

    #[test]
    fn month_and_all_time() {
        let month = compute(&history(), Range::Month, now());
        assert_eq!(month.plays, 8);
        assert_eq!(month.bars.len(), 21);
        assert_eq!(month.best_streak, 4);
        let (first, last, _) = month.previous.unwrap();
        assert_eq!((first.month(), first.day(), last.day()), (9, 1, 21));

        // Three weeks of history: still charted day by day.
        let all = compute(&history(), Range::AllTime, now());
        assert!(!all.monthly_bars);
        assert_eq!(all.bars.len(), 21);
        assert!(all.previous.is_none());

        // A year on, by month.
        let later = Bucharest.with_ymd_and_hms(2027, 9, 2, 12, 0, 0).unwrap();
        let all = compute(&history(), Range::AllTime, later);
        assert!(all.monthly_bars);
        assert_eq!(all.bars.len(), 12);
        assert_eq!(all.bars[0].first_day.month(), 10);
        assert_eq!(all.bars[0].listened_ms, all.listened_ms);
    }

    #[test]
    fn whole_month_recap_and_when_it_is_due() {
        let recap = compute(
            &history(),
            Range::WholeMonth {
                year: 2026,
                month: 10,
            },
            Bucharest.with_ymd_and_hms(2026, 11, 1, 9, 0, 0).unwrap(),
        );
        assert_eq!(recap.plays, 8);
        assert_eq!(recap.bars.len(), 31);
        assert_eq!(recap.streak, 0);

        let november = Bucharest.with_ymd_and_hms(2026, 11, 1, 0, 5, 0).unwrap();
        let (year, month, key) = recap_due(november, Some("2026-09")).unwrap();
        assert_eq!((year, month, key.as_str()), (2026, 10, "2026-10"));
        assert!(recap_due(november, Some("2026-10")).is_none());
        let january = Bucharest.with_ymd_and_hms(2027, 1, 3, 12, 0, 0).unwrap();
        assert_eq!(recap_due(january, None).unwrap().2, "2026-12");
    }

    #[test]
    fn older_entries_without_listening_time_still_count() {
        let mut old = entry(at(21, 9), "z", &[], 0);
        old.listened_ms = None;
        old.artist_list.clear();
        old.artists = "Tyler, The Creator".into();
        let stats = compute(&[old], Range::Week, now());
        assert_eq!(stats.plays, 1);
        assert_eq!(stats.listened_ms, LEGACY_LISTEN_MS);
        // Without the list, the joined name is split the old way.
        assert_eq!(stats.artists, 2);
    }

    #[test]
    fn nothing_played_is_fine() {
        let stats = compute(&[], Range::AllTime, now());
        assert_eq!(stats.plays, 0);
        assert_eq!(stats.bars.len(), 1);
        assert!(stats.peak_hour.is_none());
        assert_eq!(stats.streak, 0);
    }
}

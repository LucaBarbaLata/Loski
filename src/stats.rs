//! Listening stats from the history: totals, top songs, artists and genres,
//! streaks, who you listened with and when you listen, counted in calendar
//! days of your own timezone. Also the search behind /history.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Datelike, Days, Months, NaiveDate, TimeZone, Timelike};
use chrono_tz::Tz;

use crate::{genres::GenreMap, store::HistoryEntry};

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
    /// A calendar year (so far, if it's this one), for Loski Wrapped.
    Year(i32),
}

impl Range {
    pub const TABS: [Range; 3] = [Range::Week, Range::Month, Range::AllTime];

    pub fn key(self) -> &'static str {
        match self {
            Range::Week => "week",
            Range::Month => "month",
            Range::AllTime => "all",
            Range::WholeMonth { .. } => "recap",
            Range::Year(_) => "year",
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
            Range::Year(_) => "Year",
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

/// A friend you listened with, and what you played together.
#[derive(Debug, Clone)]
pub struct Together {
    /// Their Discord user id.
    pub friend: u64,
    pub plays: u32,
    pub listened_ms: u64,
    /// The songs you played most with them.
    pub top_songs: Vec<TopSong>,
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
    /// The day you listened most, and how long.
    pub best_day: Option<(NaiveDate, u64)>,
    /// Genres by listening time, most first (only artists with known genres).
    pub top_genres: Vec<(String, u64)>,
    /// Friends you listened with, most time together first.
    pub together: Vec<Together>,
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
        Range::Year(year) => {
            let first = NaiveDate::from_ymd_opt(year, 1, 1).unwrap_or(today);
            let end = NaiveDate::from_ymd_opt(year, 12, 31).unwrap_or(today);
            let previous = NaiveDate::from_ymd_opt(year - 1, 1, 1).zip(NaiveDate::from_ymd_opt(
                year - 1,
                12,
                31,
            ));
            (first, end.min(today.max(first)), previous)
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

/// Adds one play of `entry` to the song tally.
fn tally_song<'a>(songs: &mut HashMap<&'a str, TopSong>, entry: &'a HistoryEntry, ms: u64) {
    let song = songs.entry(&entry.uri).or_insert_with(|| TopSong {
        uri: entry.uri.clone(),
        title: String::new(),
        artists: String::new(),
        cover: None,
        plays: 0,
        listened_ms: 0,
    });
    song.plays += 1;
    song.listened_ms += ms;
    // Later plays carry the freshest name and cover.
    song.title.clone_from(&entry.title);
    song.artists.clone_from(&entry.artists);
    if entry.cover.is_some() {
        song.cover.clone_from(&entry.cover);
    }
}

fn ranked_songs(songs: HashMap<&str, TopSong>) -> Vec<TopSong> {
    let mut top: Vec<TopSong> = songs.into_values().collect();
    top.sort_by(|a, b| (b.plays, b.listened_ms, &a.title).cmp(&(a.plays, a.listened_ms, &b.title)));
    top
}

/// Counts what played in `range`. `genres` holds what's known about each
/// artist's genres (see `genres.rs`).
pub fn compute(
    history: &[HistoryEntry],
    range: Range,
    now: DateTime<Tz>,
    genres: &GenreMap,
) -> Stats {
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
    let mut genre_ms: HashMap<&str, u64> = HashMap::new();
    let mut friends: HashMap<u64, (u32, u64, HashMap<&str, TopSong>)> = HashMap::new();
    let mut hours = [0u64; 24];
    let mut by_day: HashMap<NaiveDate, u64> = HashMap::new();
    let (mut count, mut total) = (0u32, 0u64);
    for play in plays.iter().filter(inside) {
        let entry = play.entry;
        count += 1;
        total += play.ms;
        hours[play.hour as usize] += play.ms;
        *by_day.entry(play.day).or_default() += play.ms;
        tally_song(&mut songs, entry, play.ms);

        for name in entry.artist_names() {
            let artist = artists.entry(name).or_insert_with(|| TopArtist {
                name: name.to_string(),
                plays: 0,
                listened_ms: 0,
            });
            artist.plays += 1;
            artist.listened_ms += play.ms;
            if let Some(known) = genres.get(&name.to_lowercase()) {
                // The artist's main genres; the rest are often too broad.
                for genre in known.iter().take(2) {
                    *genre_ms.entry(genre).or_default() += play.ms;
                }
            }
        }

        for friend in &entry.with {
            let (plays, ms, songs) = friends.entry(*friend).or_default();
            *plays += 1;
            *ms += play.ms;
            tally_song(songs, entry, play.ms);
        }
    }

    let distinct_songs = songs.len();
    let distinct_artists = artists.len();
    let top_songs = ranked_songs(songs);
    let mut top_artists: Vec<TopArtist> = artists.into_values().collect();
    top_artists
        .sort_by(|a, b| (b.plays, b.listened_ms, &a.name).cmp(&(a.plays, a.listened_ms, &b.name)));
    let mut top_genres: Vec<(String, u64)> = genre_ms
        .into_iter()
        .map(|(genre, ms)| (genre.to_string(), ms))
        .collect();
    top_genres.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut together: Vec<Together> = friends
        .into_iter()
        .map(|(friend, (plays, listened_ms, songs))| Together {
            friend,
            plays,
            listened_ms,
            top_songs: ranked_songs(songs).into_iter().take(3).collect(),
        })
        .collect();
    together.sort_by(|a, b| {
        b.listened_ms
            .cmp(&a.listened_ms)
            .then(a.friend.cmp(&b.friend))
    });
    let best_day = by_day
        .iter()
        .max_by_key(|(day, ms)| (**ms, std::cmp::Reverse(**day)))
        .map(|(day, ms)| (*day, *ms));

    // All time goes by month once there are a couple of months to show; a
    // year always does.
    let monthly_bars = matches!(range, Range::Year(_))
        || (range == Range::AllTime && (last_day - first_day).num_days() > 62);
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
        best_day,
        top_genres,
        together,
    }
}

/// What /history looks for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryFilter {
    /// Words that all have to be in the title or the artists.
    pub search: Option<String>,
    /// Only plays from this time on (Unix seconds).
    pub since: Option<i64>,
    /// Only songs played with this friend in the call, or asked for by them.
    pub with: Option<u64>,
    /// Only songs a friend asked for.
    pub requests: bool,
}

impl HistoryFilter {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    pub fn matches(&self, entry: &HistoryEntry) -> bool {
        if self.since.is_some_and(|since| entry.played_at < since) {
            return false;
        }
        if self.requests && entry.requested_by.is_none() {
            return false;
        }
        if let Some(friend) = self.with
            && !entry.with.contains(&friend)
            && entry.requested_by != Some(friend)
        {
            return false;
        }
        match &self.search {
            Some(words) => {
                let haystack = format!("{} {}", entry.title, entry.artists).to_lowercase();
                words
                    .to_lowercase()
                    .split_whitespace()
                    .all(|word| haystack.contains(word))
            }
            None => true,
        }
    }
}

/// The newest plays matching `filter` (up to `limit`), and how many match in all.
pub fn search_history(
    history: Vec<HistoryEntry>,
    filter: &HistoryFilter,
    limit: usize,
) -> (Vec<HistoryEntry>, usize) {
    let mut found: Vec<HistoryEntry> = history
        .into_iter()
        .rev()
        .filter(|e| filter.matches(e))
        .collect();
    let total = found.len();
    found.truncate(limit);
    (found, total)
}

/// Your most played songs since `since` (Unix seconds), most played first.
pub fn top_uris(history: &[HistoryEntry], since: i64, limit: usize) -> Vec<String> {
    let mut plays: HashMap<&str, (u32, i64)> = HashMap::new();
    for entry in history.iter().filter(|e| e.played_at >= since) {
        let song = plays.entry(&entry.uri).or_default();
        song.0 += 1;
        song.1 = song.1.max(entry.played_at);
    }
    let mut songs: Vec<_> = plays.into_iter().collect();
    songs.sort_by(|a, b| (b.1.0, b.1.1, a.0).cmp(&(a.1.0, a.1.1, b.0)));
    songs
        .into_iter()
        .take(limit)
        .map(|(uri, _)| uri.to_string())
        .collect()
}

/// The year to send Loski Wrapped for, once a new year has begun.
/// `last_sent` is the year the previous Wrapped covered.
pub fn wrapped_due(now: DateTime<Tz>, last_sent: Option<&str>) -> Option<i32> {
    let year = now.year() - 1;
    (last_sent != Some(year.to_string().as_str())).then_some(year)
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
            with: Vec::new(),
            requested_by: None,
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
        let stats = compute(&history(), Range::Week, now(), &GenreMap::new());
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
        let month = compute(&history(), Range::Month, now(), &GenreMap::new());
        assert_eq!(month.plays, 8);
        assert_eq!(month.bars.len(), 21);
        assert_eq!(month.best_streak, 4);
        let (first, last, _) = month.previous.unwrap();
        assert_eq!((first.month(), first.day(), last.day()), (9, 1, 21));

        // Three weeks of history: still charted day by day.
        let all = compute(&history(), Range::AllTime, now(), &GenreMap::new());
        assert!(!all.monthly_bars);
        assert_eq!(all.bars.len(), 21);
        assert!(all.previous.is_none());

        // A year on, by month.
        let later = Bucharest.with_ymd_and_hms(2027, 9, 2, 12, 0, 0).unwrap();
        let all = compute(&history(), Range::AllTime, later, &GenreMap::new());
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
            &GenreMap::new(),
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
        let stats = compute(&[old], Range::Week, now(), &GenreMap::new());
        assert_eq!(stats.plays, 1);
        assert_eq!(stats.listened_ms, LEGACY_LISTEN_MS);
        // Without the list, the joined name is split the old way.
        assert_eq!(stats.artists, 2);
    }

    #[test]
    fn genres_friends_and_the_best_day() {
        let mut history = history();
        history[2].with = vec![7];
        history[4].with = vec![7, 8];
        history[5].with = vec![8];
        let genres: GenreMap = [
            (
                "breaking benjamin".to_string(),
                vec!["alternative metal".to_string(), "post-grunge".to_string()],
            ),
            ("saliva".to_string(), vec!["nu metal".to_string()]),
        ]
        .into_iter()
        .collect();
        let stats = compute(&history, Range::Month, now(), &genres);
        assert_eq!(
            stats.top_genres[0],
            ("alternative metal".to_string(), 19 * 60_000)
        );
        assert_eq!(stats.top_genres[2], ("nu metal".to_string(), 6 * 60_000));
        assert_eq!(stats.together.len(), 2);
        assert_eq!(stats.together[0].friend, 7);
        assert_eq!(stats.together[0].listened_ms, 8 * 60_000);
        assert_eq!(stats.together[0].top_songs[0].uri, "spotify:track:a");
        assert_eq!(stats.best_day.map(|(d, _)| d.day()), Some(1));
    }

    #[test]
    fn a_year_goes_by_month() {
        let year = compute(&history(), Range::Year(2026), now(), &GenreMap::new());
        assert!(year.monthly_bars);
        assert_eq!(year.bars.len(), 10, "January to October so far");
        assert_eq!(year.plays, 8);
        let last_year = compute(
            &history(),
            Range::Year(2026),
            Bucharest.with_ymd_and_hms(2027, 1, 1, 9, 0, 0).unwrap(),
            &GenreMap::new(),
        );
        assert_eq!(last_year.bars.len(), 12);
        let new_year = Bucharest.with_ymd_and_hms(2027, 1, 1, 0, 5, 0).unwrap();
        assert_eq!(wrapped_due(new_year, Some("2025")), Some(2026));
        assert_eq!(wrapped_due(new_year, Some("2026")), None);
    }

    #[test]
    fn history_search_and_filters() {
        let mut history = history();
        history[3].with = vec![7];
        history[6].requested_by = Some(8);
        let words = HistoryFilter {
            search: Some("benjamin SONG".into()),
            ..HistoryFilter::default()
        };
        let (found, total) = search_history(history.clone(), &words, 2);
        assert_eq!((found.len(), total), (2, 5));
        // Newest first.
        assert_eq!(found[0].played_at, at(21, 10));
        let with = HistoryFilter {
            with: Some(7),
            ..HistoryFilter::default()
        };
        assert_eq!(search_history(history.clone(), &with, 10).1, 1);
        let asked = HistoryFilter {
            requests: true,
            since: Some(at(20, 0)),
            ..HistoryFilter::default()
        };
        assert_eq!(search_history(history, &asked, 10).1, 1);
        assert!(HistoryFilter::default().is_empty());
    }

    #[test]
    fn nothing_played_is_fine() {
        let stats = compute(&[], Range::AllTime, now(), &GenreMap::new());
        assert_eq!(stats.plays, 0);
        assert_eq!(stats.bars.len(), 1);
        assert!(stats.peak_hour.is_none());
        assert_eq!(stats.streak, 0);
    }
}

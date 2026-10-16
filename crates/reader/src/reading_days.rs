//! Reading sessions bucketed into local days, for home's heatmap and the
//! reading activity tab: seconds a day over a range, the heatmap's levels
//! (the reading days' quartiles), month names, streaks, time by document,
//! and one day's sessions and hours.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone, Timelike};
use glyphst_library::ReadingSession;

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// The local day and hour a session started.
fn local_start(session: &ReadingSession) -> Option<(NaiveDate, u32)> {
    let at = Local
        .timestamp_opt(i64::try_from(session.start).ok()?, 0)
        .single()?;
    Some((at.date_naive(), at.hour()))
}

/// Days from `first` to `last` laid out in weeks from the Monday on or
/// before `first`, with the seconds read on each.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadingDays {
    /// The grid's first day, a Monday.
    pub start: NaiveDate,
    pub first: NaiveDate,
    pub last: NaiveDate,
    /// Seconds read on each day of the grid, from `start`.
    pub seconds: Vec<u64>,
}

impl ReadingDays {
    pub fn new(sessions: &[ReadingSession], first: NaiveDate, last: NaiveDate) -> Self {
        let start = first - Duration::days(i64::from(first.weekday().num_days_from_monday()));
        let days = (last - start).num_days().max(0) as usize + 1;
        let mut seconds = vec![0; days.div_ceil(7) * 7];
        for session in sessions {
            if let Some((day, _)) = local_start(session)
                && (first..=last).contains(&day)
            {
                seconds[(day - start).num_days() as usize] += session.seconds;
            }
        }
        Self {
            start,
            first,
            last,
            seconds,
        }
    }

    /// The `weeks` weeks ending with `today`'s.
    pub fn weeks_to(sessions: &[ReadingSession], today: NaiveDate, weeks: usize) -> Self {
        let monday = today - Duration::days(i64::from(today.weekday().num_days_from_monday()));
        let first = monday - Duration::weeks(weeks.saturating_sub(1) as i64);
        Self::new(sessions, first, today)
    }

    /// Calendar year `year`, up to `today` when it is this year.
    pub fn year(sessions: &[ReadingSession], year: i32, today: NaiveDate) -> Self {
        let first = NaiveDate::from_ymd_opt(year, 1, 1).unwrap_or(today);
        let last = NaiveDate::from_ymd_opt(year, 12, 31)
            .unwrap_or(today)
            .min(today);
        Self::new(sessions, first, last.max(first))
    }

    pub fn date(&self, index: usize) -> NaiveDate {
        self.start + Duration::days(index as i64)
    }

    pub fn index(&self, date: NaiveDate) -> Option<usize> {
        (self.first..=self.last)
            .contains(&date)
            .then(|| (date - self.start).num_days() as usize)
    }

    fn in_range(&self, index: usize) -> bool {
        (self.first..=self.last).contains(&self.date(index))
    }

    /// Each day's level: 0 without reading, then 1 to 4 by the quartile
    /// of the reading days' seconds it falls in; `None` outside the range.
    pub fn levels(&self) -> Vec<Option<u8>> {
        let mut read: Vec<u64> = self.seconds.iter().copied().filter(|&s| s > 0).collect();
        read.sort_unstable();
        let quartile = |q: usize| read.get((read.len() * q / 4).min(read.len().saturating_sub(1)));
        let bounds =
            [quartile(1), quartile(2), quartile(3)].map(|bound| bound.copied().unwrap_or(0));
        (0..self.seconds.len())
            .map(|index| {
                self.in_range(index).then(|| match self.seconds[index] {
                    0 => 0,
                    s => 1 + bounds.iter().filter(|&&bound| s > bound).count() as u8,
                })
            })
            .collect()
    }

    /// The months' names and the week each starts in. A month starting in
    /// the grid's last two weeks has no room for its name.
    pub fn months(&self) -> Vec<(usize, &'static str)> {
        let weeks = self.seconds.len() / 7;
        (0..weeks.saturating_sub(2))
            .filter_map(|week| {
                let monday = self.date(week * 7);
                let sunday = monday + Duration::days(6);
                let turns = sunday.month() != monday.month();
                let starts = turns || monday.day() == 1 || (week == 0 && monday.day() <= 7);
                let month = if turns { sunday } else { monday };
                starts.then(|| (week, MONTHS[month.month0() as usize]))
            })
            .collect()
    }

    pub fn total(&self) -> u64 {
        self.seconds.iter().sum()
    }

    pub fn reading_days(&self) -> usize {
        self.seconds.iter().filter(|&&s| s > 0).count()
    }
}

/// The current streak (days in a row up to today, or up to yesterday
/// while today has none yet) and the longest, over every session.
pub fn streaks(sessions: &[ReadingSession], today: NaiveDate) -> (usize, usize) {
    let mut days: Vec<NaiveDate> = sessions
        .iter()
        .filter(|session| session.seconds > 0)
        .filter_map(|session| local_start(session).map(|(day, _)| day))
        .collect();
    days.sort_unstable();
    days.dedup();
    let mut longest = 0;
    let mut run = 0;
    let mut previous: Option<NaiveDate> = None;
    for &day in &days {
        run = match previous {
            Some(before) if day - before == Duration::days(1) => run + 1,
            _ => 1,
        };
        longest = longest.max(run);
        previous = Some(day);
    }
    let last = days.last().copied();
    let current = if last == Some(today) || last == Some(today - Duration::days(1)) {
        run
    } else {
        0
    };
    (current, longest)
}

/// Seconds read in each document over `days`, most first.
pub fn by_document(sessions: &[ReadingSession], days: &ReadingDays) -> Vec<(PathBuf, u64)> {
    let mut totals: HashMap<&Path, u64> = HashMap::new();
    for session in sessions {
        if let Some((day, _)) = local_start(session)
            && (days.first..=days.last).contains(&day)
        {
            *totals.entry(&session.path).or_default() += session.seconds;
        }
    }
    let mut totals: Vec<(PathBuf, u64)> = totals
        .into_iter()
        .map(|(path, seconds)| (path.to_path_buf(), seconds))
        .collect();
    totals.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    totals
}

/// The sessions that started on `day`, in order.
pub fn sessions_on(sessions: &[ReadingSession], day: NaiveDate) -> Vec<&ReadingSession> {
    sessions
        .iter()
        .filter(|session| local_start(session).is_some_and(|(start, _)| start == day))
        .collect()
}

/// Seconds read in each hour of `day`, by the hour each session started.
pub fn hours_on(sessions: &[&ReadingSession]) -> [u64; 24] {
    let mut hours = [0; 24];
    for session in sessions {
        if let Some((_, hour)) = local_start(session) {
            hours[hour as usize] += session.seconds;
        }
    }
    hours
}

/// `09:42–10:03`, a session's local start and end.
pub fn span_label(session: &ReadingSession) -> String {
    let time = |at: u64| {
        i64::try_from(at)
            .ok()
            .and_then(|at| Local.timestamp_opt(at, 0).single())
            .map(|at| at.format("%H:%M").to_string())
            .unwrap_or_default()
    };
    format!("{}–{}", time(session.start), time(session.end()))
}

/// `wed 15 oct`.
pub fn day_label(day: NaiveDate) -> String {
    day.format("%a %-d %b").to_string().to_lowercase()
}

/// `38 min`, `1h 12m`, `142h`.
pub fn duration_label(seconds: u64) -> String {
    let minutes = seconds / 60;
    match minutes {
        0 => "<1 min".to_owned(),
        1..60 => format!("{minutes} min"),
        60..600 => format!("{}h {:02}m", minutes / 60, minutes % 60),
        _ => format!("{}h", minutes / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(day: NaiveDate, hour: u32, seconds: u64, path: &str) -> ReadingSession {
        let start = Local
            .from_local_datetime(&day.and_hms_opt(hour, 0, 0).unwrap())
            .single()
            .unwrap()
            .timestamp() as u64;
        ReadingSession {
            id: 1,
            start,
            seconds,
            path: PathBuf::from(path),
            pages: None,
        }
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn days_fill_weeks_from_a_monday_and_end_today() {
        let today = date(2025, 10, 15); // a Wednesday
        let sessions = [
            session(today, 9, 600, "/a.pdf"),
            session(today, 18, 300, "/b.pdf"),
            session(date(2025, 10, 13), 9, 60, "/a.pdf"),
            session(date(2024, 1, 1), 9, 60, "/a.pdf"),
        ];
        let days = ReadingDays::weeks_to(&sessions, today, 53);
        assert_eq!(days.start.weekday(), chrono::Weekday::Mon);
        assert_eq!(days.seconds.len(), 53 * 7);
        assert_eq!(days.index(today), Some(52 * 7 + 2));
        assert_eq!(days.seconds[52 * 7 + 2], 900);
        assert_eq!(days.total(), 960, "a session out of range is left out");
        let levels = days.levels();
        assert_eq!(levels[52 * 7 + 3], None, "after today");
        assert_eq!(levels[52 * 7 + 1], Some(0));
        assert_eq!(levels[52 * 7], Some(1));
        assert_eq!(levels[52 * 7 + 2], Some(2));
    }

    #[test]
    fn levels_split_reading_days_into_quartiles() {
        let first = date(2025, 1, 6);
        let sessions: Vec<_> = (0..8)
            .map(|day| {
                session(
                    first + Duration::days(day),
                    10,
                    (day as u64 + 1) * 60,
                    "/a.pdf",
                )
            })
            .collect();
        let days = ReadingDays::new(&sessions, first, first + Duration::days(9));
        let levels: Vec<u8> = days.levels().into_iter().flatten().collect();
        assert_eq!(levels, [1, 1, 1, 2, 2, 3, 3, 4, 0, 0]);
    }

    #[test]
    fn a_year_starts_on_its_monday_and_names_its_months() {
        let days = ReadingDays::year(&[], 2025, date(2026, 3, 1));
        assert_eq!(days.start, date(2024, 12, 30));
        assert_eq!(days.levels()[0], None, "december 2024 is outside");
        assert_eq!(days.levels()[2], Some(0));
        let months = days.months();
        assert_eq!(months[0], (0, "jan"));
        assert_eq!(months[1].1, "feb");
        assert_eq!(months.len(), 12);
    }

    #[test]
    fn streaks_count_days_in_a_row() {
        let today = date(2025, 10, 15);
        let day = |back: i64| session(today - Duration::days(back), 9, 60, "/a.pdf");
        // Yesterday and the two days before, then a gap, then five days.
        let sessions = [
            day(1),
            day(2),
            day(3),
            day(10),
            day(11),
            day(12),
            day(13),
            day(14),
        ];
        assert_eq!(streaks(&sessions, today), (3, 5));
        assert_eq!(streaks(&sessions[3..], today), (0, 5));
    }

    #[test]
    fn documents_and_hours_of_a_day() {
        let today = date(2025, 10, 15);
        let sessions = [
            session(today, 9, 600, "/a.pdf"),
            session(today, 9, 300, "/b.pdf"),
            session(today, 18, 1200, "/b.pdf"),
        ];
        let days = ReadingDays::weeks_to(&sessions, today, 1);
        assert_eq!(
            by_document(&sessions, &days),
            [
                (PathBuf::from("/b.pdf"), 1500),
                (PathBuf::from("/a.pdf"), 600)
            ]
        );
        let on = sessions_on(&sessions, today);
        assert_eq!(on.len(), 3);
        let hours = hours_on(&on);
        assert_eq!((hours[9], hours[18]), (900, 1200));
        assert_eq!(day_label(today), "wed 15 oct");
        assert_eq!(duration_label(72 * 60), "1h 12m");
        assert_eq!(duration_label(142 * 3600), "142h");
    }
}

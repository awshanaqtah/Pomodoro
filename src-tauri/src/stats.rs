//! Views over the session history for the Progress page: the forest calendar, a single day's
//! sessions with their notes, and where this week's focus time went.

use std::collections::BTreeMap;

use chrono::{Datelike, Local, NaiveDate, TimeDelta};
use serde::Serialize;

use crate::tasks::History;

#[derive(Serialize)]
pub struct DayCell {
    pub date: String,
    pub minutes: u32,
    pub sessions: u32,
}

/// Every day of a month with its focus time, for the forest calendar.
pub fn month(history: &History, year: i32, month: u32) -> Vec<DayCell> {
    let Some(first) = NaiveDate::from_ymd_opt(year, month, 1) else { return Vec::new() };
    let mut totals: BTreeMap<String, (u32, u32)> = BTreeMap::new();
    for s in &history.sessions {
        if s.day.starts_with(&first.format("%Y-%m-").to_string()) {
            let entry = totals.entry(s.day.clone()).or_default();
            entry.0 += s.minutes;
            entry.1 += 1;
        }
    }
    first
        .iter_days()
        .take_while(|d| d.month() == month)
        .map(|d| {
            let date = d.format("%Y-%m-%d").to_string();
            let (minutes, sessions) = totals.get(&date).copied().unwrap_or_default();
            DayCell { date, minutes, sessions }
        })
        .collect()
}

#[derive(Serialize)]
pub struct SessionView {
    /// Position in the history, used to attach a note.
    pub index: usize,
    pub at: Option<String>,
    pub minutes: u32,
    pub task: Option<String>,
    pub note: Option<String>,
}

pub fn day(history: &History, date: &str) -> Vec<SessionView> {
    history
        .sessions
        .iter()
        .enumerate()
        .filter(|(_, s)| s.day == date)
        .map(|(index, s)| SessionView {
            index,
            at: s.at.clone(),
            minutes: s.minutes,
            task: s.task.clone(),
            note: s.note.clone().filter(|n| !n.is_empty()),
        })
        .collect()
}

#[derive(Serialize)]
pub struct TaskTime {
    pub title: String,
    pub minutes: u32,
}

/// Focus minutes per task over the last `days` days, biggest first.
pub fn by_task(history: &History, days: i64) -> Vec<TaskTime> {
    let since = (Local::now().date_naive() - TimeDelta::days(days - 1)).format("%Y-%m-%d").to_string();
    let mut totals: BTreeMap<String, u32> = BTreeMap::new();
    for s in history.sessions.iter().filter(|s| s.day >= since) {
        let title = s.task.clone().unwrap_or_else(|| "No task picked".into());
        *totals.entry(title).or_default() += s.minutes;
    }
    let mut list: Vec<TaskTime> = totals.into_iter().map(|(title, minutes)| TaskTime { title, minutes }).collect();
    list.sort_by(|a, b| b.minutes.cmp(&a.minutes));
    list.truncate(6);
    list
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::Session;

    fn session(day: &str, minutes: u32, task: Option<&str>) -> Session {
        Session { day: day.into(), minutes, task: task.map(Into::into), at: None, note: None }
    }

    #[test]
    fn month_has_every_day_with_totals() {
        let h = History {
            sessions: vec![
                session("2026-02-03", 25, None),
                session("2026-02-03", 25, None),
                session("2026-03-01", 50, None),
            ],
        };
        let feb = month(&h, 2026, 2);
        assert_eq!(feb.len(), 28);
        assert_eq!((feb[2].minutes, feb[2].sessions), (50, 2));
        assert_eq!(feb[0].minutes, 0);
    }

    #[test]
    fn this_week_by_task() {
        let today = Local::now().date_naive();
        let d = |back: i64| (today - TimeDelta::days(back)).format("%Y-%m-%d").to_string();
        let h = History {
            sessions: vec![
                session(&d(0), 25, Some("Report")),
                session(&d(2), 50, Some("Report")),
                session(&d(1), 25, None),
                session(&d(30), 99, Some("Old")),
            ],
        };
        let list = by_task(&h, 7);
        assert_eq!(list.len(), 2);
        assert_eq!((list[0].title.as_str(), list[0].minutes), ("Report", 75));
        assert_eq!(list[1].title, "No task picked");
    }
}

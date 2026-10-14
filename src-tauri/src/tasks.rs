//! Tasks and session history, stored as `tasks.json` and `history.json` next to the settings.

use std::path::Path;

use chrono::{Local, NaiveDate, TimeDelta};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Comes back unticked every morning.
    Daily,
    /// Disappears the day after it is ticked off.
    Once,
    /// Imported from the calendar for today only; replaced on every sync.
    Calendar,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub kind: Kind,
    /// Planned focus sessions.
    pub estimate: u32,
    /// Focus sessions finished on this task today.
    pub sessions: u32,
    pub done: bool,
    /// "09:00–10:30" for timed calendar events.
    #[serde(default)]
    pub time: Option<String>,
    /// Minutes after midnight, used to order calendar events.
    #[serde(default)]
    pub start: Option<u32>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Tasks {
    pub day: String,
    pub active: Option<String>,
    pub list: Vec<Task>,
}

impl Tasks {
    /// Starts a new day: daily tasks reset, finished one-offs and yesterday's events go.
    /// Returns true if anything changed.
    pub fn roll_over(&mut self) -> bool {
        let today = today();
        if self.day == today {
            return false;
        }
        self.day = today;
        self.list.retain(|t| match t.kind {
            Kind::Daily => true,
            Kind::Once => !t.done,
            Kind::Calendar => false,
        });
        for t in &mut self.list {
            if t.kind == Kind::Daily {
                t.done = false;
            }
            t.sessions = 0;
        }
        self.fix_active();
        true
    }

    pub fn add(&mut self, title: &str, kind: Kind, estimate: u32) {
        self.list.push(Task {
            id: new_id(),
            title: title.trim().chars().take(80).collect(),
            kind,
            estimate: estimate.clamp(1, 12),
            sessions: 0,
            done: false,
            time: None,
            start: None,
        });
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut Task> {
        self.list.iter_mut().find(|t| t.id == id)
    }

    pub fn remove(&mut self, id: &str) {
        self.list.retain(|t| t.id != id);
        self.fix_active();
    }

    pub fn active_task(&mut self) -> Option<&mut Task> {
        let id = self.active.clone()?;
        self.get_mut(&id)
    }

    /// Swaps in a fresh set of calendar events, keeping progress on events seen before.
    pub fn replace_calendar(&mut self, mut events: Vec<Task>) {
        for e in &mut events {
            if let Some(old) = self.list.iter().find(|t| t.id == e.id) {
                e.done = old.done;
                e.sessions = old.sessions;
                e.estimate = old.estimate;
            }
        }
        self.list.retain(|t| t.kind != Kind::Calendar);
        self.list.extend(events);
        self.fix_active();
    }

    fn fix_active(&mut self) {
        if let Some(id) = &self.active {
            if !self.list.iter().any(|t| &t.id == id) {
                self.active = None;
            }
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Session {
    pub day: String,
    pub minutes: u32,
    pub task: Option<String>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    pub sessions: Vec<Session>,
}

pub fn today() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("t{nanos:x}{:x}", COUNTER.fetch_add(1, Ordering::Relaxed))
}

pub fn load<T: DeserializeOwned + Default>(path: &Path) -> T {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save<T: Serialize>(path: &Path, value: &T) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let json = serde_json::to_string_pretty(value).expect("state is always serializable");
    if let Err(e) = std::fs::write(path, json) {
        eprintln!("could not save {}: {e}", path.display());
    }
}

#[derive(Serialize)]
pub struct DayStat {
    pub label: String,
    pub minutes: u32,
    pub today: bool,
}

#[derive(Serialize)]
pub struct Progress {
    pub today_sessions: u32,
    pub today_minutes: u32,
    /// Days in a row with at least one finished focus session, counting today once it has one.
    pub streak: u32,
    pub best_streak: u32,
    /// The last seven days, oldest first.
    pub week: Vec<DayStat>,
}

impl History {
    pub fn progress(&self) -> Progress {
        use std::collections::BTreeMap;
        let mut by_day: BTreeMap<NaiveDate, (u32, u32)> = BTreeMap::new();
        for s in &self.sessions {
            if let Ok(day) = NaiveDate::parse_from_str(&s.day, "%Y-%m-%d") {
                let entry = by_day.entry(day).or_default();
                entry.0 += 1;
                entry.1 += s.minutes;
            }
        }
        let today = Local::now().date_naive();
        let (today_sessions, today_minutes) = by_day.get(&today).copied().unwrap_or_default();

        // A streak survives until the end of today, so it counts back from yesterday if today is empty.
        let mut day = if today_sessions > 0 { today } else { today - TimeDelta::days(1) };
        let mut streak = 0;
        while by_day.contains_key(&day) {
            streak += 1;
            day -= TimeDelta::days(1);
        }

        let (mut best_streak, mut run, mut prev): (u32, u32, Option<NaiveDate>) = (0, 0, None);
        for &d in by_day.keys() {
            run = if prev == Some(d - TimeDelta::days(1)) { run + 1 } else { 1 };
            best_streak = best_streak.max(run);
            prev = Some(d);
        }

        let week = (0..7)
            .rev()
            .map(|back| {
                let d = today - TimeDelta::days(back);
                DayStat {
                    label: d.format("%a").to_string(),
                    minutes: by_day.get(&d).map_or(0, |e| e.1),
                    today: back == 0,
                }
            })
            .collect();

        Progress { today_sessions, today_minutes, streak, best_streak, week }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn history(days_ago: &[i64]) -> History {
        let today = Local::now().date_naive();
        History {
            sessions: days_ago
                .iter()
                .map(|&n| Session {
                    day: (today - TimeDelta::days(n)).format("%Y-%m-%d").to_string(),
                    minutes: 25,
                    task: None,
                })
                .collect(),
        }
    }

    #[test]
    fn streak_counts_back_from_today() {
        let p = history(&[0, 0, 1, 2, 4]).progress();
        assert_eq!((p.today_sessions, p.today_minutes), (2, 50));
        assert_eq!(p.streak, 3);
        assert_eq!(p.best_streak, 3);
        assert_eq!(p.week.len(), 7);
        assert_eq!(p.week[6].minutes, 50);
        assert!(p.week[6].today);
    }

    #[test]
    fn streak_survives_until_today_ends() {
        let p = history(&[1, 2, 10, 11, 12, 13]).progress();
        assert_eq!(p.today_sessions, 0);
        assert_eq!(p.streak, 2, "yesterday's streak is still alive today");
        assert_eq!(p.best_streak, 4);
    }

    #[test]
    fn daily_tasks_reset_and_finished_one_offs_clear() {
        let mut t = Tasks { day: "2000-01-01".into(), ..Default::default() };
        t.add("Read", Kind::Daily, 2);
        t.add("Email", Kind::Once, 1);
        t.add("Call mum", Kind::Once, 1);
        for task in &mut t.list {
            task.done = task.title != "Call mum";
            task.sessions = 1;
        }
        assert!(t.roll_over());
        let titles: Vec<_> = t.list.iter().map(|x| (x.title.as_str(), x.done, x.sessions)).collect();
        assert_eq!(titles, [("Read", false, 0), ("Call mum", false, 0)]);
        assert!(!t.roll_over(), "same day again changes nothing");
    }
}

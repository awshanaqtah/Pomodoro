//! Calendar import: reads a private iCal feed (Google Calendar's "secret address in iCal format",
//! or the same from Outlook or Apple) and turns today's events into tasks.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeDelta, TimeZone, Timelike, Utc};

use crate::tasks::{Kind, Task};

/// Downloads the feed. `webcal://` links are fetched over https.
pub fn fetch(url: &str) -> Result<String, String> {
    let url = url.trim();
    let url = match url.strip_prefix("webcal://") {
        Some(rest) => format!("https://{rest}"),
        None => url.to_string(),
    };
    if !url.starts_with("https://") {
        return Err("That doesn't look like an iCal link (it should start with https://).".into());
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(20)))
        .build()
        .into();
    let mut response = agent.get(&url).call().map_err(|e| match e {
        ureq::Error::StatusCode(404 | 403 | 401) => {
            "Calendar link not found. Copy the secret iCal address again.".to_string()
        }
        other => format!("Couldn't reach the calendar ({other})."),
    })?;
    let body = response
        .body_mut()
        .with_config()
        .limit(30 * 1024 * 1024)
        .read_to_string()
        .map_err(|e| format!("Couldn't read the calendar ({e})."))?;
    if !body.contains("BEGIN:VCALENDAR") {
        return Err("That link didn't return a calendar. Use the iCal address, not the web link.".into());
    }
    Ok(body)
}

/// Today's events as tasks, ordered by start time. Each hour of event time is planned as
/// `60 / session_minutes` focus sessions (at least one).
pub fn today_tasks(ics: &str, today: NaiveDate, session_minutes: u32) -> Vec<Task> {
    let Some(day_start) = local_midnight(today) else { return Vec::new() };
    let Some(day_end) = local_midnight(today + TimeDelta::days(1)) else { return Vec::new() };
    let events = parse_events(ics);

    // Moved or edited instances of a repeating event arrive as separate VEVENTs that carry a
    // RECURRENCE-ID; the series itself must skip those dates.
    let mut moved: HashMap<&str, Vec<&Prop>> = HashMap::new();
    for e in &events {
        if let (Some(uid), Some(rid)) = (e.value("UID"), e.prop("RECURRENCE-ID")) {
            moved.entry(uid).or_default().push(rid);
        }
    }

    let mut out: Vec<Task> = Vec::new();
    for e in &events {
        if e.value("STATUS").is_some_and(|s| s.eq_ignore_ascii_case("CANCELLED")) {
            continue;
        }
        let Some(start) = e.prop("DTSTART").and_then(parse_when) else { continue };
        let end = e.prop("DTEND").and_then(parse_when);
        let length = match (start, end) {
            (When::Day(a), Some(When::Day(b))) if b > a => b - a,
            (When::Day(_), _) => TimeDelta::days(1),
            (When::At(a), Some(When::At(b))) if b > a => b - a,
            (When::At(_), _) => TimeDelta::zero(),
        };

        let starts = if e.prop("RRULE").is_some() && e.prop("RECURRENCE-ID").is_none() {
            let skip = e.value("UID").and_then(|uid| moved.get(uid)).map(Vec::as_slice).unwrap_or(&[]);
            expand(e, start, skip, day_start - length - TimeDelta::days(1), day_end)
        } else {
            vec![start]
        };

        for occurrence in starts {
            let (from, to) = match occurrence {
                When::Day(d) => match (local_midnight(d), local_midnight(d + length)) {
                    (Some(a), Some(b)) => (a, b),
                    _ => continue,
                },
                When::At(t) => (t, t + length),
            };
            let overlaps = from < day_end && (to > day_start || (length.is_zero() && from >= day_start));
            if !overlaps {
                continue;
            }
            out.push(to_task(e, occurrence, from, to, day_start, length, session_minutes, today));
        }
    }
    out.sort_by_key(|t| t.start.unwrap_or(0));
    out
}

#[allow(clippy::too_many_arguments)]
fn to_task(
    e: &Event,
    occurrence: When,
    from: DateTime<Local>,
    to: DateTime<Local>,
    day_start: DateTime<Local>,
    length: TimeDelta,
    session_minutes: u32,
    today: NaiveDate,
) -> Task {
    let title = e.value("SUMMARY").map(unescape).filter(|s| !s.trim().is_empty());
    let uid = e.value("UID").unwrap_or("event");
    let (time, start, key) = match occurrence {
        When::Day(_) => (None, None, "day".to_string()),
        When::At(_) => {
            let label = if length.is_zero() {
                from.format("%H:%M").to_string()
            } else {
                format!("{}–{}", from.format("%H:%M"), to.format("%H:%M"))
            };
            // An event that started yesterday sorts to the top of today.
            let minute = if from < day_start { 0 } else { from.hour() * 60 + from.minute() };
            (Some(label), Some(minute), from.format("%H%M").to_string())
        }
    };
    let estimate = match occurrence {
        When::Day(_) => 1,
        When::At(_) => (length.num_minutes() as f32 / session_minutes.max(1) as f32).round().clamp(1.0, 8.0) as u32,
    };
    Task {
        id: format!("cal:{uid}:{today}:{key}"),
        title: title.unwrap_or_else(|| "(untitled event)".into()).chars().take(80).collect(),
        kind: Kind::Calendar,
        estimate,
        sessions: 0,
        done: false,
        time,
        start,
    }
}

/// Expands a repeating event within [from, to] using the iCal recurrence rules.
fn expand(e: &Event, start: When, skip: &[&Prop], from: DateTime<Local>, to: DateTime<Local>) -> Vec<When> {
    let mut rules = vec![e.prop("DTSTART").map(Prop::line).unwrap_or_default()];
    for name in ["RRULE", "RDATE", "EXDATE", "EXRULE"] {
        rules.extend(e.props_named(name).map(Prop::line));
    }
    rules.extend(skip.iter().map(|rid| format!("EXDATE{}:{}", rid.params_part(), rid.value)));
    let text = rules.join("\n");

    // Some calendars (Outlook) use Windows time zone names that the rule parser doesn't know;
    // retry with the zone dropped, which reads the times as local.
    let set = text
        .parse::<rrule::RRuleSet>()
        .or_else(|_| strip_tzid(&text).parse::<rrule::RRuleSet>());
    let Ok(set) = set else {
        return vec![start]; // unreadable rule: fall back to the first occurrence only
    };
    let result = set
        .after(from.with_timezone(&rrule::Tz::LOCAL))
        .before(to.with_timezone(&rrule::Tz::LOCAL))
        .all(64);
    result
        .dates
        .into_iter()
        .map(|d| match start {
            When::Day(_) => When::Day(d.naive_local().date()),
            When::At(_) => When::At(d.with_timezone(&Local)),
        })
        .collect()
}

fn strip_tzid(text: &str) -> String {
    text.lines()
        .map(|line| {
            let Some((head, value)) = line.split_once(':') else { return line.to_string() };
            let mut parts = head.split(';');
            let name = parts.next().unwrap_or_default();
            let params: Vec<&str> = parts.filter(|p| !p.starts_with("TZID=")).collect();
            let mut out = name.to_string();
            for p in params {
                out.push(';');
                out.push_str(p);
            }
            format!("{out}:{value}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn local_midnight(day: NaiveDate) -> Option<DateTime<Local>> {
    Local.from_local_datetime(&day.and_hms_opt(0, 0, 0)?).earliest()
}

// ---------------------------------------------------------------- minimal iCal reader

#[derive(Clone, Copy)]
enum When {
    Day(NaiveDate),
    At(DateTime<Local>),
}

struct Prop {
    name: String,
    params: String,
    value: String,
}

impl Prop {
    fn param(&self, key: &str) -> Option<&str> {
        self.params
            .split(';')
            .find_map(|p| p.strip_prefix(key)?.strip_prefix('='))
            .map(|v| v.trim_matches('"'))
    }
    fn params_part(&self) -> String {
        if self.params.is_empty() { String::new() } else { format!(";{}", self.params) }
    }
    fn line(&self) -> String {
        format!("{}{}:{}", self.name, self.params_part(), self.value)
    }
}

#[derive(Default)]
struct Event {
    props: Vec<Prop>,
}

impl Event {
    fn prop(&self, name: &str) -> Option<&Prop> {
        self.props.iter().find(|p| p.name == name)
    }
    fn props_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Prop> {
        self.props.iter().filter(move |p| p.name == name)
    }
    fn value(&self, name: &str) -> Option<&str> {
        self.prop(name).map(|p| p.value.as_str())
    }
}

fn parse_events(ics: &str) -> Vec<Event> {
    // Long lines are folded onto continuation lines that start with a space or tab.
    let mut lines: Vec<String> = Vec::new();
    for raw in ics.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        match raw.strip_prefix([' ', '\t']) {
            Some(rest) if !lines.is_empty() => lines.last_mut().unwrap().push_str(rest),
            _ => lines.push(raw.to_string()),
        }
    }

    let mut events = Vec::new();
    let mut current: Option<Event> = None;
    let mut nested = 0; // VALARM and friends inside an event
    for line in &lines {
        match line.as_str() {
            "BEGIN:VEVENT" => {
                current = Some(Event::default());
                nested = 0;
            }
            "END:VEVENT" => events.extend(current.take()),
            l if l.starts_with("BEGIN:") && current.is_some() => nested += 1,
            l if l.starts_with("END:") && current.is_some() => nested -= 1,
            l => {
                if let (Some(e), 0) = (current.as_mut(), nested) {
                    e.props.extend(parse_prop(l));
                }
            }
        }
    }
    events
}

fn parse_prop(line: &str) -> Option<Prop> {
    // The value starts at the first ':' outside a quoted parameter.
    let mut quoted = false;
    let colon = line.char_indices().find_map(|(i, c)| match c {
        '"' => {
            quoted = !quoted;
            None
        }
        ':' if !quoted => Some(i),
        _ => None,
    })?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let (name, params) = head.split_once(';').unwrap_or((head, ""));
    Some(Prop { name: name.to_ascii_uppercase(), params: params.to_string(), value: value.to_string() })
}

fn parse_when(p: &Prop) -> Option<When> {
    let v = p.value.trim();
    if p.param("VALUE") == Some("DATE") || v.len() == 8 {
        return NaiveDate::parse_from_str(v, "%Y%m%d").ok().map(When::Day);
    }
    let (body, utc) = match v.strip_suffix('Z') {
        Some(b) => (b, true),
        None => (v, false),
    };
    let naive = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S").ok()?;
    let local = if utc {
        Utc.from_utc_datetime(&naive).with_timezone(&Local)
    } else if let Some(tz) = p.param("TZID").and_then(|t| t.parse::<chrono_tz::Tz>().ok()) {
        tz.from_local_datetime(&naive).earliest()?.with_timezone(&Local)
    } else {
        Local.from_local_datetime(&naive).earliest()? // floating or unknown zone: local time
    };
    Some(When::At(local))
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push(' '),
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FEED: &str = "BEGIN:VCALENDAR\r
BEGIN:VEVENT\r
UID:standup\r
DTSTART;TZID=Asia/Amman:20260105T090000\r
DTEND;TZID=Asia/Amman:20260105T100000\r
RRULE:FREQ=WEEKLY;BYDAY=MO,TU,WE,TH,FR\r
EXDATE;TZID=Asia/Amman:20261015T090000\r
SUMMARY:Stand-up\\, team\r
BEGIN:VALARM\r
SUMMARY:alarm text that must be ignored\r
END:VALARM\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:standup\r
RECURRENCE-ID;TZID=Asia/Amman:20261014T090000\r
DTSTART;TZID=Asia/Amman:20261014T110000\r
DTEND;TZID=Asia/Amman:20261014T120000\r
SUMMARY:Stand-up (moved)\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:gym\r
DTSTART;VALUE=DATE:20261014\r
DTEND;VALUE=DATE:20261015\r
SUMMARY:Gym day\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:study\r
DTSTART:20261014T150000Z\r
DTEND:20261014T170000Z\r
SUMMARY:Study\r
 ing session\r
END:VEVENT\r
BEGIN:VEVENT\r
UID:cancelled\r
DTSTART:20261014T150000Z\r
STATUS:CANCELLED\r
SUMMARY:Nope\r
END:VEVENT\r
END:VCALENDAR\r
";

    fn day(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn moved_instance_replaces_series_occurrence() {
        let titles: Vec<String> =
            today_tasks(FEED, day("2026-10-14"), 25).into_iter().map(|t| t.title).collect();
        assert!(titles.contains(&"Stand-up (moved)".to_string()), "{titles:?}");
        assert!(!titles.contains(&"Stand-up, team".to_string()), "{titles:?}");
        assert!(titles.contains(&"Gym day".to_string()));
        assert!(titles.contains(&"Studying session".to_string()));
        assert!(!titles.contains(&"Nope".to_string()));
        assert_eq!(titles.len(), 3, "{titles:?}");
    }

    #[test]
    fn series_repeats_and_honours_exdate() {
        let on = |d: &str| today_tasks(FEED, day(d), 25).iter().any(|t| t.title == "Stand-up, team");
        assert!(on("2026-10-13"), "Tuesday");
        assert!(!on("2026-10-15"), "EXDATE Thursday");
        assert!(on("2026-10-16"), "Friday");
        assert!(!on("2026-10-17"), "Saturday");
    }

    #[test]
    fn hour_long_event_plans_two_sessions() {
        let tasks = today_tasks(FEED, day("2026-10-13"), 30);
        let standup = tasks.iter().find(|t| t.title == "Stand-up, team").unwrap();
        assert_eq!(standup.estimate, 2);
        assert!(standup.time.is_some());
    }
}

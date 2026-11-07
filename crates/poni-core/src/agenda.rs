//! The Today page's agenda: the Mac's calendar events of one day, and the
//! little form that makes a new one. The Mac's Calendar (EventKit) fills
//! it; on a platform without one the page keeps its stat tiles.

/// Where the person's answer to "may Motchy read your calendars" stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Access {
    /// macOS hasn't asked yet.
    #[default]
    Ask,
    Granted,
    /// Said no, or Calendars is off for the app (System Settings changes it).
    Denied,
}

/// One event in a day's list.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub title: String,
    /// Unix seconds.
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub calendar: String,
    /// The calendar's colour (sRGB, 0..1).
    pub rgb: [f32; 3],
    /// A video call to join (https, a known service), found in the event's
    /// link, place or notes.
    pub link: Option<String>,
}

/// A calendar a new event can go into.
#[derive(Clone, Debug, PartialEq)]
pub struct Calendar {
    pub id: String,
    pub title: String,
}

/// What the Today page keeps about the agenda.
#[derive(Clone, Debug, Default)]
pub struct Agenda {
    /// A calendar is on offer (only the Mac sets it): the page shows the
    /// agenda where the stat tiles were.
    pub on: bool,
    pub access: Access,
    /// The day shown, as days since 1970-01-01 (0 = today).
    pub day: i64,
    pub events: Vec<Event>,
    pub calendars: Vec<Calendar>,
    /// Which of `calendars` a new event goes into.
    pub calendar: usize,
    /// The form is open.
    pub adding: bool,
    pub title: String,
    pub time: String,
    /// What the last save said, if it failed.
    pub error: Option<String>,
    /// How far the list is scrolled (DIPs).
    pub scroll: f32,
}

/// Where Tasks stands with the Mac's Reminders (the Tasks page's footer).
#[derive(Clone, Debug, Default)]
pub struct Reminders {
    /// Reminders can be synced (only the Mac sets it).
    pub available: bool,
    pub access: Access,
    /// A sync is under way.
    pub busy: bool,
    /// What the last sync said, if it failed.
    pub error: Option<String>,
}

/// "9:30", "09:30" or "930" → minutes since midnight.
pub fn parse_time(s: &str) -> Option<i64> {
    let s = s.trim();
    let (h, m) = match s.split_once(':') {
        Some((h, m)) => (h, m),
        None if s.len() >= 3 && s.len() <= 4 && s.is_char_boundary(s.len() - 2) => s.split_at(s.len() - 2),
        None => (s, "0"),
    };
    let (h, m) = (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?);
    ((0..24).contains(&h) && (0..60).contains(&m)).then_some(h * 60 + m)
}

/// The next whole hour after `minutes` into the day, as "HH:00" (the form's
/// first suggestion), wrapping to 23:00 at the end of the day.
pub fn next_hour(minutes: i64) -> String {
    format!("{:02}:00", ((minutes / 60) + 1).min(23))
}

/// "9:30" for a clock of `minutes` since midnight.
pub fn clock(minutes: i64, use_24h: bool) -> String {
    let (h, m) = ((minutes / 60) % 24, minutes % 60);
    if use_24h {
        format!("{h:02}:{m:02}")
    } else {
        format!("{}:{m:02} {}", if h % 12 == 0 { 12 } else { h % 12 }, if h < 12 { "AM" } else { "PM" })
    }
}

/// Video-call services whose links get a Join button.
const CALL_HOSTS: [&str; 5] = ["zoom.us", "meet.google.com", "teams.microsoft.com", "teams.live.com", "webex.com"];

/// The first https link to a known video-call service in `texts` (an
/// event's link, place and notes, in that order). Other links, and other
/// schemes, are never offered: Join opens a page in the browser.
pub fn find_link(texts: &[&str]) -> Option<String> {
    texts.iter().flat_map(|t| t.split(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"' | '\'' | '(' | ')'))).find_map(|w| {
        let url = w.trim_end_matches(['.', ',', ';', '!', '?']);
        let rest = url.strip_prefix("https://")?;
        let host = rest.split(['/', '?', '#']).next()?.to_ascii_lowercase();
        let host = host.split('@').next_back()?;
        let host = host.split(':').next()?;
        CALL_HOSTS.iter().any(|d| host == *d || host.ends_with(&format!(".{d}"))).then(|| url.to_string())
    })
}

/// How an event stands at `now`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Phase {
    /// All day: no clock to count.
    AllDay,
    Ended,
    /// On now: how far through it is (0..1) and the minutes left.
    Now { progress: f32, left_min: i64 },
    /// Not yet: minutes until it starts.
    Soon { in_min: i64 },
}

pub fn phase(e: &Event, now: i64) -> Phase {
    if e.all_day {
        Phase::AllDay
    } else if now >= e.end {
        Phase::Ended
    } else if now >= e.start {
        let len = (e.end - e.start).max(1) as f32;
        Phase::Now { progress: ((now - e.start) as f32 / len).clamp(0.0, 1.0), left_min: (e.end - now + 59) / 60 }
    } else {
        Phase::Soon { in_min: (e.start - now + 59) / 60 }
    }
}

/// "12 min", "1 h 20 min", "2 h".
fn span(min: i64) -> String {
    match (min / 60, min % 60) {
        (0, m) => format!("{m} min"),
        (h, 0) => format!("{h} h"),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// The line under an event's title: "8 min left" (on now), "in 1 h 20 min",
/// "Ended", "All day".
pub fn status(e: &Event, now: i64) -> String {
    match phase(e, now) {
        Phase::AllDay => "All day".into(),
        Phase::Ended => "Ended".into(),
        Phase::Now { left_min, .. } => format!("{} left", span(left_min)),
        Phase::Soon { in_min } if in_min < 24 * 60 => format!("in {}", span(in_min)),
        Phase::Soon { .. } => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(start: i64, end: i64, all_day: bool) -> Event {
        Event { title: "x".into(), start, end, all_day, calendar: String::new(), rgb: [0.0; 3], link: None }
    }

    #[test]
    fn phases_and_their_words() {
        let e = ev(1000, 1000 + 900, false);
        assert_eq!(phase(&e, 500), Phase::Soon { in_min: 9 });
        assert_eq!(status(&e, 500), "in 9 min");
        assert_eq!(status(&e, 1000 - 4800), "in 1 h 20 min");
        assert_eq!(status(&e, 1000 - 7200), "in 2 h");
        match phase(&e, 1000 + 300) {
            Phase::Now { progress, left_min } => assert!((progress - 1.0 / 3.0).abs() < 1e-5 && left_min == 10),
            p => panic!("{p:?}"),
        }
        assert_eq!(status(&e, 1000 + 300), "10 min left");
        assert_eq!(status(&e, 5000), "Ended");
        assert_eq!(status(&ev(0, 86_400, true), 10), "All day");
        assert_eq!(status(&e, 1000 - 3 * 86_400), "");
    }

    #[test]
    fn only_known_call_links_get_join() {
        let zoom = "https://us02web.zoom.us/j/123456?pwd=abc";
        assert_eq!(find_link(&["", "Room 4", &format!("Join: {zoom}.")]), Some(zoom.into()));
        assert_eq!(find_link(&["see (https://meet.google.com/abc-defg-hij)"]), Some("https://meet.google.com/abc-defg-hij".into()));
        assert_eq!(find_link(&["https://teams.microsoft.com/l/meetup-join/19"]), Some("https://teams.microsoft.com/l/meetup-join/19".into()));
        // Not a call service, not https, or a look-alike host.
        assert_eq!(find_link(&["https://example.com/zoom.us"]), None);
        assert_eq!(find_link(&["http://zoom.us/j/1"]), None);
        assert_eq!(find_link(&["https://zoom.us.evil.com/j/1"]), None);
        assert_eq!(find_link(&["https://notzoom.us/j/1"]), None);
        assert_eq!(find_link(&["https://zoom.us@evil.com/j/1"]), None);
        assert_eq!(find_link(&[]), None);
    }

    #[test]
    fn times_parse() {
        assert_eq!(parse_time("9:30"), Some(570));
        assert_eq!(parse_time("09:05"), Some(545));
        assert_eq!(parse_time("930"), Some(570));
        assert_eq!(parse_time("14"), Some(840));
        assert_eq!(parse_time("24:00"), None);
        assert_eq!(parse_time("9:60"), None);
        assert_eq!(parse_time("abc"), None);
        assert_eq!(parse_time(""), None);
    }

    #[test]
    fn next_hour_stops_at_the_end_of_the_day() {
        assert_eq!(next_hour(8 * 60 + 15), "09:00");
        assert_eq!(next_hour(23 * 60 + 30), "23:00");
    }

    #[test]
    fn clock_words() {
        assert_eq!(clock(0, false), "12:00 AM");
        assert_eq!(clock(13 * 60 + 5, false), "1:05 PM");
        assert_eq!(clock(13 * 60 + 5, true), "13:05");
    }
}

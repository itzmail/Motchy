//! Coding time, kept by the app from WakaTime's own editor plugins: no
//! WakaTime account or key is read. No I/O here.
//!
//! The plugins post "heartbeats" (a file was touched, at a time, in a
//! project and language) to the `api_url` in `~/.wakatime.cfg`. Pointed at
//! this Mac, the app receives them (`services::wakatime`), counts the gaps
//! between them the way WakaTime does (a gap of up to fifteen minutes was
//! coding, a longer one a break), and keeps one line per day in a file per
//! month: `activity/wakatime/2026-11.jsonl` in the person's repository.
//! Raw heartbeats are never stored.
//!
//! A heartbeat with the category "ai coding" (wakatime-cli adds them for
//! what a coding agent edited) is counted in a stream of its own, so its
//! gaps never mix with the person's. Its seconds are in the day's total and
//! also kept apart in [`Day::ai`].

use crate::datetime::days_from_civil;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Where the plugins send to: this Mac only.
pub const PORT: u16 = 43729;
/// A gap between two heartbeats up to this long counts as coding
/// (WakaTime's default of fifteen minutes).
pub const TIMEOUT_S: f64 = 900.0;
/// Projects, languages and editors kept for each day.
const KEEP_TOP: usize = 8;
/// Heartbeats taken from one request.
pub const MAX_BEATS: usize = 1000;
/// Dates the tracker remembers a last heartbeat for.
const KEEP_DATES: usize = 800;

/// A name and the seconds spent on it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Share {
    pub name: String,
    pub secs: u32,
}

/// One day of coding, the Mac's own date.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Day {
    /// "2026-11-06".
    pub date: String,
    /// Seconds of coding.
    pub total: u32,
    pub projects: Vec<Share>,
    pub languages: Vec<Share>,
    /// Of `total`, the seconds a coding agent worked (heartbeats of the
    /// category "ai coding"). Days kept before this was told apart have 0.
    pub ai: u32,
    /// "VS Code", "Zed", "Neovim": from the plugin's user agent.
    pub editors: Vec<Share>,
}

// ---------------------------------------------------------------- the config file

/// The `api_url` in `~/.wakatime.cfg`'s `[settings]` section, if any. Lines
/// that start with `;` or `#` are comments; the last live one counts.
pub fn api_url_in(text: &str) -> Option<String> {
    let mut found = None;
    for (_, line) in settings_lines(text) {
        if let Some(v) = key_value(line, "api_url") {
            found = Some(v);
        }
    }
    found
}

/// Does the file send to this Mac's app?
pub fn points_here(text: &str) -> bool {
    api_url_in(text).is_some_and(|u| u.trim_end_matches('/') == local_api_url())
}

/// Where the plugins are told to send.
pub fn local_api_url() -> String {
    format!("http://127.0.0.1:{PORT}/api")
}

/// The file with its `api_url` set to `url`: the live line is replaced in
/// place (and the old value kept as a comment above it), or one is added
/// under `[settings]`, which is made if the file has none. Nothing else in
/// the file moves.
pub fn with_api_url(text: &str, url: &str) -> String {
    let mut out = Vec::new();
    let (mut in_settings, mut done) = (false, false);
    for line in text.lines() {
        let t = line.trim();
        if let Some(name) = t.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if in_settings && !done {
                out.push(format!("api_url = {url}"));
                done = true;
            }
            in_settings = name.trim().eq_ignore_ascii_case("settings");
            out.push(line.to_string());
            continue;
        }
        if in_settings && !done {
            if let Some(old) = key_value(t, "api_url") {
                if old.trim_end_matches('/') == url.trim_end_matches('/') {
                    // Already there: the line stays as it is.
                    out.push(line.to_string());
                } else {
                    out.push(format!("; {t}"));
                    out.push(format!("api_url = {url}"));
                }
                done = true;
                continue;
            }
        }
        out.push(line.to_string());
    }
    if !done {
        match out.iter().position(|l| l.trim().eq_ignore_ascii_case("[settings]")) {
            Some(i) => out.insert(i + 1, format!("api_url = {url}")),
            None => {
                out.insert(0, "[settings]".to_string());
                out.insert(1, format!("api_url = {url}"));
            }
        }
    }
    out.join("\n") + "\n"
}

/// The lines of the `[settings]` section (with their numbers), comments left out.
fn settings_lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut in_settings = false;
    text.lines().enumerate().filter_map(move |(i, l)| {
        let l = l.trim();
        if l.is_empty() || l.starts_with(';') || l.starts_with('#') {
            return None;
        }
        if let Some(name) = l.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            in_settings = name.trim().eq_ignore_ascii_case("settings");
            return None;
        }
        in_settings.then_some((i, l))
    })
}

fn key_value(line: &str, key: &str) -> Option<String> {
    let (k, v) = line.split_once('=')?;
    (k.trim().eq_ignore_ascii_case(key) && !v.trim().is_empty()).then(|| v.trim().to_string())
}

// ---------------------------------------------------------------- what the plugins send

/// One heartbeat, reduced to what is counted.
#[derive(Clone, Debug, PartialEq)]
pub struct Beat {
    /// Unix seconds.
    pub time: f64,
    pub project: String,
    pub language: String,
    pub editor: String,
    /// The category was "ai coding".
    pub ai: bool,
}

/// The editor a plugin's user agent names: `wakatime/v2.28.4 (darwin-26.0-arm64)
/// go1.25.0 vscode/1.100.0 vscode-wakatime/25.0.0` is "VS Code". "Other" when
/// it names none.
pub fn editor_of(user_agent: &str) -> String {
    let tokens: Vec<&str> = user_agent.split_whitespace().collect();
    // What follows the Go version is the editor and then its plugin (named
    // `...-wakatime`); a coding agent names the model first and itself last
    // (`haiku/4-5 claude-code/2.1`). So: the last one that isn't a plugin.
    let after = match tokens.iter().position(|t| t.strip_prefix("go").is_some_and(|v| v.starts_with(|c: char| c.is_ascii_digit()))) {
        Some(i) => &tokens[i + 1..],
        None => &tokens[..],
    };
    let name = after.iter().rev().find(|t| !t.starts_with("wakatime/") && !t.starts_with('(') && !t.split('/').next().is_some_and(|n| n.to_ascii_lowercase().ends_with("wakatime"))).and_then(|t| t.split('/').next()).unwrap_or("").trim();
    if name.is_empty() {
        return "Other".into();
    }
    let known = match name.to_ascii_lowercase().as_str() {
        "vscode" | "code" => "VS Code",
        "vscode-insiders" => "VS Code Insiders",
        "neovim" | "nvim" => "Neovim",
        "vim" => "Vim",
        "zed" => "Zed",
        "cursor" => "Cursor",
        "emacs" => "Emacs",
        "sublime" | "sublime text" => "Sublime Text",
        "xcode" => "Xcode",
        "windsurf" => "Windsurf",
        _ => "",
    };
    if !known.is_empty() {
        return known.into();
    }
    // "claude-code" is "Claude Code".
    let words: Vec<String> = name
        .split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
        })
        .collect();
    let name: String = words.join(" ").chars().take(40).collect();
    if name.is_empty() { "Other".into() } else { name }
}

fn name_of(v: &Value, key: &str) -> String {
    let s: String = v.get(key).and_then(Value::as_str).unwrap_or("").trim().chars().take(60).collect();
    if s.is_empty() { "Other".into() } else { s }
}

/// The heartbeats in a request body (one object, or an array of them) and
/// how many entries it held. Entries without a plausible time are skipped,
/// but still answered, one answer per entry.
pub fn parse_beats(body: &str) -> Result<(Vec<Beat>, usize), String> {
    let v: Value = serde_json::from_str(body).map_err(|_| "the body isn't JSON".to_string())?;
    let list: Vec<&Value> = match &v {
        Value::Array(a) => a.iter().take(MAX_BEATS).collect(),
        Value::Object(_) => vec![&v],
        _ => return Err("expected heartbeats".into()),
    };
    let total = list.len();
    let beats = list
        .into_iter()
        .filter_map(|b| {
            let time = b.get("time").and_then(Value::as_f64).filter(|t| t.is_finite() && (946_684_800.0..4_102_444_800.0).contains(t))?;
            let ai = b.get("category").and_then(Value::as_str).is_some_and(|c| c.trim().eq_ignore_ascii_case("ai coding"));
            let editor = b.get("user_agent").and_then(Value::as_str).map_or_else(|| "Other".to_string(), editor_of);
            Some(Beat { time, project: name_of(b, "project"), language: name_of(b, "language"), editor, ai })
        })
        .collect();
    Ok((beats, total))
}

/// The answer to `heartbeats.bulk`: one "created" per heartbeat.
pub fn bulk_response(n: usize) -> String {
    let one = r#"[{"data":{"id":"motchy"}},201]"#;
    format!(r#"{{"responses":[{}]}}"#, vec![one; n].join(","))
}

/// The answer to a single heartbeat.
pub const BEAT_RESPONSE: &str = r#"{"data":{"id":"motchy"}}"#;

/// "1 hr 5 mins", "5 mins", "30 secs": WakaTime's own wording.
pub fn fmt_text(secs: u32) -> String {
    let (h, m) = (secs / 3600, secs % 3600 / 60);
    let plural = |n: u32, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    match (h, m) {
        (0, 0) => plural(secs, "sec"),
        (0, m) => plural(m, "min"),
        (h, 0) => plural(h, "hr"),
        (h, m) => format!("{} {}", plural(h, "hr"), plural(m, "min")),
    }
}

/// The answer to the status bar's "today" (editors show it next to the
/// file): today's time from the app's own numbers.
pub fn status_bar_response(secs: u32) -> String {
    let (h, m) = (secs / 3600, secs % 3600 / 60);
    format!(
        r#"{{"data":{{"grand_total":{{"decimal":"{:.2}","digital":"{h}:{m:02}","hours":{h},"minutes":{m},"text":"{}","total_seconds":{secs}}},"categories":[],"editors":[],"languages":[],"machines":[],"operating_systems":[],"projects":[],"range":{{"text":"Today","timezone":"UTC"}}}}}}"#,
        secs as f64 / 3600.0,
        fmt_text(secs)
    )
}

// ---------------------------------------------------------------- counting

/// The last heartbeat seen on a date.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Last {
    pub time: f64,
    pub project: String,
    pub language: String,
    pub editor: String,
}

/// What the counting remembers between heartbeats and runs: the last one of
/// each date, so a gap is measured from it. Kept apart from the days.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tracker {
    pub last: BTreeMap<String, Last>,
    /// The same for the coding agent's heartbeats ("ai coding"), counted apart.
    pub ai_last: BTreeMap<String, Last>,
}

impl Tracker {
    /// Count a heartbeat that happened on `date` into `days`. The gap since
    /// the last one of its stream (the person's, or the coding agent's) that
    /// day, if it was short enough, was spent on what that last one was
    /// about. A heartbeat that isn't newer is ignored (the same one sent
    /// twice, or an older one that arrives late).
    pub fn add(&mut self, days: &mut Vec<Day>, beat: &Beat, date: &str) {
        let last = if beat.ai { &mut self.ai_last } else { &mut self.last };
        let spent = match last.get(date) {
            Some(l) if beat.time <= l.time => return,
            Some(l) if beat.time - l.time <= TIMEOUT_S => Some(((beat.time - l.time).round() as u32, l.project.clone(), l.language.clone(), l.editor.clone())),
            _ => None,
        };
        if days.iter().all(|d| d.date != date) {
            days.push(Day { date: date.to_string(), ..Day::default() });
        }
        if let Some((secs, project, language, editor)) = spent.filter(|(s, ..)| *s > 0) {
            if let Some(d) = days.iter_mut().find(|d| d.date == date) {
                d.total += secs;
                if beat.ai {
                    d.ai += secs;
                }
                bump(&mut d.projects, &project, secs);
                bump(&mut d.languages, &language, secs);
                bump(&mut d.editors, &editor, secs);
            }
        }
        last.insert(date.to_string(), Last { time: beat.time, project: beat.project.clone(), language: beat.language.clone(), editor: beat.editor.clone() });
        for map in [&mut self.last, &mut self.ai_last] {
            while map.len() > KEEP_DATES {
                let oldest = map.keys().next().cloned().unwrap_or_default();
                map.remove(&oldest);
            }
        }
    }

    /// Count a request's heartbeats, oldest first. `date_of` gives the
    /// Mac's own date for a Unix time.
    pub fn add_all(&mut self, days: &mut Vec<Day>, mut beats: Vec<Beat>, date_of: impl Fn(f64) -> String) {
        beats.sort_by(|a, b| a.time.total_cmp(&b.time));
        for b in &beats {
            self.add(days, b, &date_of(b.time));
        }
        days.sort_by(|a, b| a.date.cmp(&b.date));
    }
}

fn bump(shares: &mut Vec<Share>, name: &str, secs: u32) {
    match shares.iter_mut().find(|s| s.name == name) {
        Some(s) => s.secs += secs,
        None => shares.push(Share { name: name.to_string(), secs }),
    }
    shares.sort_by(|a, b| b.secs.cmp(&a.secs).then_with(|| a.name.cmp(&b.name)));
    shares.truncate(KEEP_TOP);
}

// ---------------------------------------------------------------- dates

pub(crate) fn parse_date(d: &str) -> Option<i64> {
    let b = d.as_bytes();
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let (y, m, day) = (d[..4].parse::<i32>().ok()?, d[5..7].parse::<u8>().ok()?, d[8..].parse::<u8>().ok()?);
    ((1..=12).contains(&m) && (1..=31).contains(&day)).then(|| days_from_civil(y, m, day))
}

/// The month's file name for a day: "2026-11.jsonl".
pub fn month_file(date: &str) -> String {
    format!("{}.jsonl", date.get(..7).unwrap_or("0000-00"))
}

// ---------------------------------------------------------------- keeping days

/// The days of two copies (this Mac's and the repository's) joined: for a
/// date both have, the one with more time, since a day's total only grows.
/// Both sides then hold the same days.
pub fn union(a: Vec<Day>, b: Vec<Day>) -> Vec<Day> {
    let mut all = a;
    for d in b {
        match all.iter_mut().find(|x| x.date == d.date) {
            Some(x) if d.total > x.total => *x = d,
            Some(_) => {}
            None => all.push(d),
        }
    }
    all.sort_by(|a, b| a.date.cmp(&b.date));
    all
}

/// One line per day, in date order.
pub fn to_jsonl(days: &[Day]) -> String {
    days.iter().filter_map(|d| serde_json::to_string(d).ok()).map(|l| l + "\n").collect()
}

/// The days in a file; a line that doesn't read is skipped.
pub fn from_jsonl(text: &str) -> Vec<Day> {
    text.lines().filter_map(|l| serde_json::from_str::<Day>(l).ok()).filter(|d| parse_date(&d.date).is_some()).collect()
}

// ---------------------------------------------------------------- what the page shows

/// The page's numbers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Seconds today.
    pub today: u32,
    /// The last seven days, oldest first, today last.
    pub week: [u32; 7],
    pub week_total: u32,
    /// The last 30 days.
    pub month_total: u32,
    /// Of today and of the last seven days, the part a coding agent worked.
    pub ai_today: u32,
    pub ai_week: u32,
    /// Where the last seven days went, most first.
    pub projects: Vec<Share>,
    pub languages: Vec<Share>,
    pub editors: Vec<Share>,
    /// Days we keep, and the first of them.
    pub kept: usize,
    pub since: String,
}

pub fn summarize(days: &[Day], today: &str) -> Summary {
    let Some(t) = parse_date(today) else { return Summary::default() };
    let mut s = Summary { kept: days.len(), since: days.iter().map(|d| d.date.clone()).min().unwrap_or_default(), ..Summary::default() };
    let (mut projects, mut languages, mut editors): (Vec<Share>, Vec<Share>, Vec<Share>) = (Vec::new(), Vec::new(), Vec::new());
    for d in days {
        let Some(age) = parse_date(&d.date).map(|z| t - z).filter(|a| (0..30).contains(a)) else { continue };
        s.month_total += d.total;
        if age < 7 {
            s.week[(6 - age) as usize] = d.total;
            s.week_total += d.total;
            add_shares(&mut projects, &d.projects);
            add_shares(&mut languages, &d.languages);
            add_shares(&mut editors, &d.editors);
            s.ai_week += d.ai;
        }
        if age == 0 {
            s.today = d.total;
            s.ai_today = d.ai;
        }
    }
    for v in [&mut projects, &mut languages, &mut editors] {
        v.sort_by(|a, b| b.secs.cmp(&a.secs).then_with(|| a.name.cmp(&b.name)));
    }
    s.projects = projects;
    s.languages = languages;
    s.editors = editors;
    s
}

fn add_shares(into: &mut Vec<Share>, from: &[Share]) {
    for f in from {
        match into.iter_mut().find(|s| s.name == f.name) {
            Some(s) => s.secs += f.secs,
            None => into.push(f.clone()),
        }
    }
}

/// "2h 05m", "45m", "0m".
pub fn fmt_hm(secs: u32) -> String {
    let m = (secs + 30) / 60;
    if m >= 60 {
        format!("{}h {:02}m", m / 60, m % 60)
    } else {
        format!("{m}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(date: &str, total: u32, project: &str) -> Day {
        Day { date: date.into(), total, projects: vec![Share { name: project.into(), secs: total }], ..Day::default() }
    }

    fn beat(time: f64, project: &str, language: &str) -> Beat {
        Beat { time, project: project.into(), language: language.into(), editor: "Other".into(), ai: false }
    }

    const T0: f64 = 1_793_000_000.0;

    #[test]
    fn the_config_file_tells_where_plugins_send() {
        // The shape of a real file.
        let text = "[settings]\napi_key = waka_x\n; api_key = waka_old\n\nstatus_bar_coding_activity = true\n";
        assert_eq!(api_url_in(text), None);
        assert!(!points_here(text));
        let set = with_api_url(text, &local_api_url());
        assert!(points_here(&set));
        assert!(set.contains("api_key = waka_x") && set.contains("; api_key = waka_old") && set.contains("status_bar_coding_activity = true"), "nothing else moves: {set}");
        assert_eq!(set.lines().next(), Some("[settings]"));
        // Setting it again changes nothing.
        assert_eq!(with_api_url(&set, &local_api_url()).matches("api_url = http://127").count(), 1);
        // A live address is replaced in place and kept as a comment.
        let other = "[settings]\napi_url = https://wakapi.example/api\nfoo = 1\n";
        assert_eq!(api_url_in(other).as_deref(), Some("https://wakapi.example/api"));
        let moved = with_api_url(other, &local_api_url());
        assert!(moved.contains("; api_url = https://wakapi.example/api") && points_here(&moved));
        // Only [settings] counts; a file with no such section gets one.
        assert_eq!(api_url_in("[other]\napi_url = http://x\n"), None);
        assert!(points_here(&with_api_url("", &local_api_url())));
        assert!(points_here(&with_api_url("[other]\na = 1\n", &local_api_url())));
        assert!(points_here("[settings]\napi_url = http://127.0.0.1:43729/api/\n"), "a trailing slash is the same address");
        assert!(!points_here("[settings]\napi_url = http://127.0.0.1:43730/api\n"));
    }

    #[test]
    fn heartbeats_are_read_from_one_object_or_an_array() {
        let one = r#"{"entity":"/a.rs","type":"file","time":1793000000.5,"project":"motchy","language":"Rust","category":"coding"}"#;
        assert_eq!(parse_beats(one).unwrap(), (vec![beat(1_793_000_000.5, "motchy", "Rust")], 1));
        let many = r#"[{"time":1793000000,"project":"a"},{"time":"x"},{"time":5},{"time":1793000100,"language":"Go"}]"#;
        let (got, total) = parse_beats(many).unwrap();
        assert_eq!(got, vec![beat(1_793_000_000.0, "a", "Other"), beat(1_793_000_100.0, "Other", "Go")], "implausible times are skipped");
        assert_eq!(total, 4, "but each entry is answered");
        assert!(parse_beats("nope").is_err() && parse_beats("3").is_err());
        let huge = format!("[{}]", vec![r#"{"time":1793000000}"#; MAX_BEATS + 50].join(","));
        assert_eq!(parse_beats(&huge).unwrap().1, MAX_BEATS);
    }

    #[test]
    fn a_short_gap_is_coding_and_a_long_one_is_a_break() {
        let (mut t, mut days) = (Tracker::default(), Vec::new());
        let date = |_: f64| "2026-11-06".to_string();
        t.add_all(&mut days, vec![beat(T0, "a", "Rust"), beat(T0 + 120.0, "a", "Rust"), beat(T0 + 240.0, "b", "Go"), beat(T0 + 240.0 + 3600.0, "b", "Go"), beat(T0 + 240.0 + 3660.0, "b", "Go")], date);
        assert_eq!(days.len(), 1);
        // 120 s on a, 120 s on a (the gap belongs to what came first), nothing for the hour, 60 s on b.
        assert_eq!(days[0].total, 300);
        let by = |v: &[Share], n: &str| v.iter().find(|s| s.name == n).map(|s| s.secs);
        assert_eq!((by(&days[0].projects, "a"), by(&days[0].projects, "b")), (Some(240), Some(60)));
        assert_eq!(by(&days[0].languages, "Rust"), Some(240));
    }

    #[test]
    fn repeats_late_arrivals_and_batches_in_any_order_count_once() {
        let date = |_: f64| "2026-11-06".to_string();
        let beats = vec![beat(T0, "a", "x"), beat(T0 + 60.0, "a", "x"), beat(T0 + 120.0, "a", "x")];
        let (mut t1, mut d1) = (Tracker::default(), Vec::new());
        t1.add_all(&mut d1, beats.clone(), date);
        let (mut t2, mut d2) = (Tracker::default(), Vec::new());
        t2.add_all(&mut d2, beats.iter().rev().cloned().collect(), date);
        assert_eq!(d1, d2, "a batch is sorted first");
        assert_eq!(d1[0].total, 120);
        // The same batch again adds nothing.
        t1.add_all(&mut d1, beats, date);
        assert_eq!(d1[0].total, 120);
        // A heartbeat older than the last one is ignored.
        t1.add(&mut d1, &beat(T0 + 10.0, "a", "x"), "2026-11-06");
        assert_eq!(d1[0].total, 120);
    }

    #[test]
    fn each_date_is_counted_apart_and_the_tracker_survives_a_restart() {
        let date = |t: f64| if t < T0 + 1000.0 { "2026-11-05".to_string() } else { "2026-11-06".to_string() };
        let (mut t, mut days) = (Tracker::default(), Vec::new());
        t.add_all(&mut days, vec![beat(T0, "a", "x"), beat(T0 + 60.0, "a", "x"), beat(T0 + 2000.0, "a", "x")], date);
        assert_eq!(days.iter().map(|d| (d.date.as_str(), d.total)).collect::<Vec<_>>(), [("2026-11-05", 60), ("2026-11-06", 0)]);
        // Written and read back, it measures the next gap from the last beat.
        let back: Tracker = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        let mut t = back;
        t.add(&mut days, &beat(T0 + 2090.0, "a", "x"), "2026-11-06");
        assert_eq!(days[1].total, 90);
    }

    #[test]
    fn the_answers_are_what_the_plugins_expect() {
        assert_eq!(bulk_response(2), r#"{"responses":[[{"data":{"id":"motchy"}},201],[{"data":{"id":"motchy"}},201]]}"#);
        assert!(serde_json::from_str::<Value>(&bulk_response(0)).is_ok());
        assert_eq!((fmt_text(0), fmt_text(1), fmt_text(60), fmt_text(125), fmt_text(3600), fmt_text(3900), fmt_text(7500)), ("0 secs".into(), "1 sec".into(), "1 min".into(), "2 mins".into(), "1 hr".into(), "1 hr 5 mins".into(), "2 hrs 5 mins".into()));
        let v: Value = serde_json::from_str(&status_bar_response(3900)).unwrap();
        assert_eq!(v["data"]["grand_total"]["text"], "1 hr 5 mins");
        assert_eq!(v["data"]["grand_total"]["total_seconds"], 3900);
        assert_eq!(v["data"]["grand_total"]["digital"], "1:05");
    }

    #[test]
    fn the_union_of_two_copies_keeps_the_bigger_day() {
        let a = vec![day("2026-11-01", 600, "a"), day("2026-11-03", 100, "a")];
        let b = vec![day("2026-11-01", 900, "a"), day("2026-11-02", 50, "b"), day("2026-11-03", 100, "z")];
        let u = union(a.clone(), b.clone());
        assert_eq!(u.iter().map(|d| (d.date.as_str(), d.total)).collect::<Vec<_>>(), [("2026-11-01", 900), ("2026-11-02", 50), ("2026-11-03", 100)]);
        assert_eq!(u[2].projects[0].name, "a", "a tie keeps the first copy's");
        let totals = |v: Vec<Day>| v.into_iter().map(|d| (d.date, d.total)).collect::<Vec<_>>();
        assert_eq!(totals(union(a, b.clone())), totals(union(b, vec![day("2026-11-01", 600, "a"), day("2026-11-03", 100, "a")])));
    }

    #[test]
    fn days_survive_a_round_trip_through_a_file() {
        let days = vec![day("2026-10-31", 60, "x"), day("2026-11-01", 120, "y")];
        let text = to_jsonl(&days);
        assert_eq!(text.lines().count(), 2);
        assert_eq!(from_jsonl(&text), days);
        assert_eq!(from_jsonl("garbage\n{\"date\":\"2026-11-01\",\"total\":5}\n{\"date\":\"x\"}\n").len(), 1);
        assert_eq!(month_file("2026-11-06"), "2026-11.jsonl");
    }

    #[test]
    fn the_summary_counts_the_week_the_month_and_where_it_went() {
        let mut d = vec![day("2026-11-06", 3600, "motchy"), day("2026-11-05", 1800, "motchy"), day("2026-11-01", 600, "other"), day("2026-10-20", 900, "old"), day("2026-09-01", 99999, "ancient")];
        d[2].projects = vec![Share { name: "other".into(), secs: 600 }];
        let s = summarize(&d, "2026-11-06");
        assert_eq!(s.today, 3600);
        assert_eq!(s.week, [0, 600, 0, 0, 0, 1800, 3600], "oldest first, today last");
        assert_eq!((s.week_total, s.month_total), (6000, 6900), "the month is the last 30 days: Oct 20 is in, Sep 1 is out");
        assert_eq!(s.projects[0], Share { name: "motchy".into(), secs: 5400 });
        assert_eq!((s.kept, s.since.as_str()), (5, "2026-09-01"));
        assert_eq!(summarize(&d, "bad"), Summary::default());
    }

    fn ai_beat(time: f64, project: &str) -> Beat {
        Beat { ai: true, editor: "Claude Code".into(), ..beat(time, project, "Rust") }
    }

    #[test]
    fn the_coding_agent_is_counted_in_a_stream_of_its_own() {
        let date = |_: f64| "2026-11-06".to_string();
        let (mut t, mut days) = (Tracker::default(), Vec::new());
        // You type for 120 s while the agent edits 300 s later in the same
        // stretch: each stream measures its own gaps, so neither eats the other's.
        t.add_all(&mut days, vec![beat(T0, "a", "Rust"), beat(T0 + 60.0, "a", "Rust"), ai_beat(T0 + 30.0, "a"), ai_beat(T0 + 330.0, "a"), beat(T0 + 120.0, "a", "Rust")], date);
        assert_eq!(days[0].total, 120 + 300, "both streams are in the total");
        assert_eq!(days[0].ai, 300, "and the agent's part is told apart");
        // A gap between a person's and an agent's beat is nobody's time.
        let (mut t, mut days) = (Tracker::default(), Vec::new());
        t.add_all(&mut days, vec![beat(T0, "a", "Rust"), ai_beat(T0 + 10.0, "a")], date);
        assert_eq!((days[0].total, days[0].ai), (0, 0));
        // The agent's last beat is remembered across a restart like the person's.
        let mut back: Tracker = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        back.add(&mut days, &ai_beat(T0 + 70.0, "a"), "2026-11-06");
        assert_eq!((days[0].total, days[0].ai), (60, 60));
        // An old tracker file (no agent stream) still reads.
        let old: Tracker = serde_json::from_str(r#"{"last":{"2026-11-06":{"time":1.0,"project":"a","language":"b"}}}"#).unwrap();
        assert!(old.ai_last.is_empty() && old.last["2026-11-06"].editor.is_empty());
    }

    #[test]
    fn the_editor_comes_from_the_plugins_user_agent() {
        for (ua, want) in [
            ("wakatime/v2.28.4 (darwin-26.0-arm64) go1.25.0 vscode/1.100.0 vscode-wakatime/25.0.0", "VS Code"),
            ("wakatime/v1.130.1 (darwin-24.0.0-arm64) go1.23.2 neovim/0.10.1 vim-wakatime/11.2.0", "Neovim"),
            ("wakatime/v1.1 (darwin-1-arm64) go1.20 zed/0.150 zed-wakatime/1.0", "Zed"),
            ("wakatime/v1.1 (darwin-1-arm64) go1.20 phpstorm/2024.1 phpstorm-wakatime/1.0", "Phpstorm"),
            ("wakatime/v1.1 (darwin-1-arm64) go1.20", "Other"),
            ("", "Other"),
            ("sometool/1.0", "Sometool"),
            ("wakatime/v1 (darwin-1-arm64) go1.20 -/1", "Other"),
            // A coding agent's: the model first, the agent last.
            ("wakatime/v2.28.4 (darwin-27.2.0-arm64) go1.27.3 haiku/4-5-20251001 claude-code/2.1.241", "Claude Code"),
            ("wakatime/v2.28.4 (darwin-27.2.0-arm64) go1.27.3 claude-code/2.1.241", "Claude Code"),
        ] {
            assert_eq!(editor_of(ua), want, "{ua}");
        }
        let one = r#"{"time":1793000000,"project":"a","language":"Rust","category":"AI Coding","user_agent":"wakatime/v1 (darwin-1-arm64) go1.20 vscode/1 vscode-wakatime/1"}"#;
        let (got, _) = parse_beats(one).unwrap();
        assert!(got[0].ai && got[0].editor == "VS Code", "{got:?}");
        let human = r#"{"time":1793000000,"category":"coding"}"#;
        assert!(!parse_beats(human).unwrap().0[0].ai);
    }

    #[test]
    fn editors_and_the_agent_show_in_the_summary() {
        let mut a = day("2026-11-06", 3600, "x");
        a.ai = 900;
        a.editors = vec![Share { name: "Zed".into(), secs: 2700 }, Share { name: "Claude Code".into(), secs: 900 }];
        let mut b = day("2026-11-05", 600, "x");
        b.ai = 100;
        b.editors = vec![Share { name: "Zed".into(), secs: 600 }];
        let s = summarize(&[a, b, day("2026-10-01", 50, "x")], "2026-11-06");
        assert_eq!((s.ai_today, s.ai_week), (900, 1000));
        assert_eq!(s.editors, vec![Share { name: "Zed".into(), secs: 3300 }, Share { name: "Claude Code".into(), secs: 900 }]);
        // Days kept before the agent was told apart read as 0.
        assert_eq!(from_jsonl("{\"date\":\"2026-11-01\",\"total\":5}\n")[0].ai, 0);
    }

    #[test]
    fn durations_read_as_hours_and_minutes() {
        assert_eq!((fmt_hm(0), fmt_hm(29), fmt_hm(31), fmt_hm(2700), fmt_hm(7500)), ("0m".into(), "0m".into(), "1m".into(), "45m".into(), "2h 05m".into()));
    }
}

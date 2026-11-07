//! The coding report in the sync repository's `README.md`: what the kept days
//! add up to, as Markdown a host shows on the repository's page. No I/O.
//!
//! The README's front page shows this month only, in one block between two
//! marker comments (whatever the person wrote above or below stays). The
//! whole history is a few generated files in `coding-activity/`: an index and
//! one file per year. It holds no
//! clock time, only the date it was made for, so the same days and the same
//! date give the same text and a sync with nothing new changes no file.

use crate::datetime::{civil_from_days, days_from_civil, days_in_month, weekday_from_days, MONTH_SHORT, WEEKDAY_SHORT};
use crate::wakatime::{fmt_hm, parse_date, Day, Share};

pub const START: &str = "<!-- motchy:start -->";
pub const END: &str = "<!-- motchy:end -->";
/// How wide a bar is, in characters.
const BAR: usize = 20;
const SHARE_BAR: usize = 10;
/// Rows in each list.
const TOP: usize = 8;

/// A bar `frac` full (0..=1): `█` for the filled part and `░` for the rest.
/// Anything above zero shows at least one block.
fn bar(frac: f64, width: usize) -> String {
    let frac = if frac.is_finite() { frac.clamp(0.0, 1.0) } else { 0.0 };
    let filled = ((frac * width as f64).round() as usize).max(usize::from(frac > 0.0)).min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

/// A name safe inside a Markdown table: no pipe, no tag, one line.
fn cell(name: &str) -> String {
    name.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>().replace('|', "\\|").replace('<', "&lt;").replace('`', "'")
}

fn pct(part: u64, whole: u64) -> u32 {
    if whole == 0 { 0 } else { (part as f64 * 100.0 / whole as f64).round() as u32 }
}

/// "Nov 06".
fn short_date(z: i64) -> String {
    let (_, m, d) = civil_from_days(z);
    format!("{} {d:02}", MONTH_SHORT[(m as usize).clamp(1, 12) - 1])
}

fn add(into: &mut Vec<Share>, from: &[Share]) {
    for f in from {
        match into.iter_mut().find(|s| s.name == f.name) {
            Some(s) => s.secs += f.secs,
            None => into.push(f.clone()),
        }
    }
    into.sort_by(|a, b| b.secs.cmp(&a.secs).then_with(|| a.name.cmp(&b.name)));
}

/// A table of shares: name, time and its share of the list.
fn table(out: &mut String, h: &str, heading: &str, first: &str, shares: &[Share]) {
    let whole: u64 = shares.iter().map(|s| s.secs as u64).sum();
    if whole == 0 {
        return;
    }
    out.push_str(&format!("{h} {heading}\n\n| {first} | Time | Share |\n|---|---:|---|\n"));
    for s in shares.iter().take(TOP) {
        out.push_str(&format!("| {} | {} | `{}` {}% |\n", cell(&s.name), fmt_hm(s.secs), bar(s.secs as f64 / whole as f64, SHARE_BAR), pct(s.secs as u64, whole)));
    }
    out.push('\n');
}

const MONTH_NAME: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
/// Where the whole history goes, next to the README.
pub const HISTORY_DIR: &str = "coding-activity";

type Set<'a> = Vec<(i64, &'a Day)>;

fn secs(set: &[(i64, &Day)]) -> u64 {
    set.iter().map(|(_, d)| d.total as u64).sum()
}

fn between<'a>(all: &[(i64, &'a Day)], from: i64, to: i64) -> Set<'a> {
    all.iter().filter(|(z, _)| (from..=to).contains(z)).cloned().collect()
}

fn by_day(set: &[(i64, &Day)], z: i64) -> u32 {
    set.iter().filter(|(x, _)| *x == z).map(|(_, d)| d.total).sum()
}

/// First and last day number of a month.
fn month_span(y: i32, m: u8) -> (i64, i64) {
    let a = days_from_civil(y, m, 1);
    (a, a + days_in_month(y, m) as i64 - 1)
}

fn full_date(z: i64) -> String {
    let (y, _, _) = civil_from_days(z);
    format!("{} {y}", short_date(z))
}

/// Days in a row with time, counting back from `t` (or from yesterday while
/// `t` has nothing yet).
fn streak(set: &[(i64, &Day)], t: i64) -> u32 {
    let mut z = if by_day(set, t) > 0 { t } else { t - 1 };
    let mut n = 0;
    while by_day(set, z) > 0 {
        n += 1;
        z -= 1;
    }
    n
}

fn days_word(n: u32) -> String {
    if n == 1 { "1 day".into() } else { format!("{n} days") }
}

/// The busiest day of `set`: "Nov 06 · 2h 00m" (the earlier one on a tie).
fn best_of(set: &[(i64, &Day)], year: bool) -> String {
    set.iter()
        .filter(|(_, d)| d.total > 0)
        .max_by(|a, b| a.1.total.cmp(&b.1.total).then(b.0.cmp(&a.0)))
        .map_or_else(|| "–".into(), |(z, d)| format!("{} · {}", if year { full_date(*z) } else { short_date(*z) }, fmt_hm(d.total)))
}

fn average_of(set: &[(i64, &Day)]) -> String {
    let active = active_of(set) as u64;
    fmt_hm(if active == 0 { 0 } else { (secs(set) / active) as u32 })
}

fn active_of(set: &[(i64, &Day)]) -> usize {
    set.iter().filter(|(_, d)| d.total > 0).count()
}

/// One line per week (Monday to Sunday) from `from` to `to`, as bars in a
/// text block. A week that crosses the edge counts only the days inside.
fn weeks(out: &mut String, set: &[(i64, &Day)], from: i64, to: i64) {
    let monday = |z: i64| z - (weekday_from_days(z) as i64 + 6) % 7;
    let mut rows: Vec<(String, u64)> = Vec::new();
    let mut start = monday(from);
    while start <= to {
        let (a, b) = (start.max(from), (start + 6).min(to));
        rows.push((format!("{} – {}", short_date(a), short_date(b)), secs(&between(set, a, b))));
        start += 7;
    }
    let top = rows.iter().map(|r| r.1).max().unwrap_or(0).max(1) as f64;
    out.push_str("```text\n");
    for (label, v) in rows {
        out.push_str(&format!("{label}  {}  {}\n", bar(v as f64 / top, BAR), fmt_hm(v as u32)));
    }
    out.push_str("```\n\n");
}

/// One line per day from `from` to `to`, as bars in a text block.
fn daily(out: &mut String, set: &[(i64, &Day)], from: i64, to: i64) {
    let top = (from..=to).map(|z| by_day(set, z)).max().unwrap_or(0).max(1) as f64;
    out.push_str("```text\n");
    for z in from..=to {
        out.push_str(&format!("{} {}  {}  {}\n", WEEKDAY_SHORT[weekday_from_days(z) as usize % 7], short_date(z), bar(by_day(set, z) as f64 / top, BAR), fmt_hm(by_day(set, z))));
    }
    out.push_str("```\n\n");
}

/// Projects, languages, the agent's part and editors of `set`. `h` is the
/// heading mark ("###").
fn lists(out: &mut String, h: &str, set: &[(i64, &Day)], ai_label: &str) {
    let (mut projects, mut languages, mut editors) = (Vec::new(), Vec::new(), Vec::new());
    for (_, d) in set {
        add(&mut projects, &d.projects);
        add(&mut languages, &d.languages);
        add(&mut editors, &d.editors);
    }
    table(out, h, "Projects", "Project", &projects);
    table(out, h, "Languages", "Language", &languages);
    let (all, ai) = (secs(set), set.iter().map(|(_, d)| d.ai as u64).sum::<u64>());
    if ai > 0 {
        out.push_str(&format!("{h} AI-assisted time\n\n| Period | You | AI | AI share |\n|---|---:|---:|---:|\n"));
        out.push_str(&format!("| {ai_label} | {} | {} | {}% |\n\n", fmt_hm(all.saturating_sub(ai) as u32), fmt_hm(ai as u32), pct(ai, all)));
    }
    table(out, h, "Editors", "Editor", &editors);
}

/// Days that are real dates up to `t`, oldest first.
fn dated(days: &[Day], t: i64) -> Set<'_> {
    let mut v: Set = days.iter().filter_map(|d| parse_date(&d.date).map(|z| (z, d))).filter(|(z, _)| *z <= t).collect();
    v.sort_by_key(|(z, _)| *z);
    v
}

/// The block for the README's front page: this month only, between the
/// markers (no newline after the closing one). Empty `days` (or a bad date)
/// still give a block that says nothing is tracked yet.
pub fn report(days: &[Day], today: &str) -> String {
    let none = format!("{START}\n## Coding report\n\n_Nothing tracked yet._\n{END}");
    let Some(t) = parse_date(today) else { return none };
    let all = dated(days, t);
    if all.is_empty() {
        return none;
    }
    let (y, m, _) = civil_from_days(t);
    let (from, to) = month_span(y, m);
    let set = between(&all, from, to);
    let mut out = format!("{START}\n## Coding report · {} {y}\n\n_Updated {today} · the whole history is in [{HISTORY_DIR}]({HISTORY_DIR}/README.md)_\n\n", MONTH_NAME[m as usize - 1]);
    if secs(&set) == 0 {
        out.push_str(&format!("_Nothing tracked this month yet._\n{END}"));
        return out;
    }
    let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
    let (pf, pt) = month_span(py, pm);
    let last = secs(&between(&all, pf, pt));
    out.push_str("| This month | Last month | Best day | Active days | Daily average |\n|---|---|---|---|---|\n");
    out.push_str(&format!("| {} | {} | {} | {} | {} |\n\n", fmt_hm(secs(&set) as u32), fmt_hm(last as u32), best_of(&set, false), active_of(&set), average_of(&set)));
    out.push_str(&format!("Streak: {}\n\n### Per week\n\n", days_word(streak(&all, t))));
    weeks(&mut out, &set, from, to.min(t));
    lists(&mut out, "###", &set, "This month");
    out.push_str(END);
    out
}

/// The files of the full history, as (path in the repository, text): an
/// index and one file per year. A year that is over never changes, so a sync
/// writes nothing for it. Empty when there are no days.
pub fn history(days: &[Day], today: &str) -> Vec<(String, String)> {
    let Some(t) = parse_date(today) else { return Vec::new() };
    let all = dated(days, t);
    if all.is_empty() {
        return Vec::new();
    }
    let note = "_Made by Motchy. Changes here are overwritten on the next sync._";
    let mut years: Vec<i32> = all.iter().map(|(z, _)| civil_from_days(*z).0).collect();
    years.dedup();
    let year_set = |y: i32| between(&all, days_from_civil(y, 1, 1), days_from_civil(y, 12, 31));

    let mut files = Vec::new();
    let mut index = format!("# Coding activity\n\n{note}\n\n");
    index.push_str("| Total | Active days | Best day | Streak | Daily average |\n|---|---|---|---|---|\n");
    index.push_str(&format!("| {} | {} | {} | {} | {} |\n\n", fmt_hm(secs(&all) as u32), active_of(&all), best_of(&all, true), days_word(streak(&all, t)), average_of(&all)));
    index.push_str("## Years\n\n| Year | Time | Active days |\n|---|---:|---:|\n");
    for &y in years.iter().rev() {
        let set = year_set(y);
        index.push_str(&format!("| [{y}]({y}.md) | {} | {} |\n", fmt_hm(secs(&set) as u32), active_of(&set)));
    }
    index.push('\n');
    lists(&mut index, "##", &all, "All time");
    files.push((format!("{HISTORY_DIR}/README.md"), index.trim_end().to_string() + "\n"));

    for &y in &years {
        let set = year_set(y);
        let mut out = format!("# {y}\n\n[All years](README.md)\n\n{note}\n\n");
        out.push_str("| Total | Active days | Best day | Daily average |\n|---|---|---|---|\n");
        out.push_str(&format!("| {} | {} | {} | {} |\n\n", fmt_hm(secs(&set) as u32), active_of(&set), best_of(&set, true), average_of(&set)));
        // Each month: a bar here, and its details folded below, newest first.
        let months: Vec<(u8, u64)> = (1..=12u8).map(|m| (m, secs(&between(&set, month_span(y, m).0, month_span(y, m).1)))).filter(|(_, v)| *v > 0).collect();
        let top = months.iter().map(|(_, v)| *v).max().unwrap_or(0).max(1) as f64;
        out.push_str("## Per month\n\n```text\n");
        for (m, v) in &months {
            out.push_str(&format!("{} {y}  {}  {}\n", MONTH_SHORT[*m as usize - 1], bar(*v as f64 / top, BAR), fmt_hm(*v as u32)));
        }
        out.push_str("```\n\n");
        for (m, v) in months.iter().rev() {
            let (a, b) = month_span(y, *m);
            let ms = between(&set, a, b);
            out.push_str(&format!("<details>\n<summary>{} {y} · {}</summary>\n\n#### Per week\n\n", MONTH_NAME[*m as usize - 1], fmt_hm(*v as u32)));
            weeks(&mut out, &ms, a, b.min(t));
            out.push_str("#### Per day\n\n");
            daily(&mut out, &ms, a, b.min(t));
            lists(&mut out, "####", &ms, "This month");
            out.push_str("</details>\n\n");
        }
        files.push((format!("{HISTORY_DIR}/{y}.md"), out.trim_end().to_string() + "\n"));
    }
    files
}

/// `existing` (the README as it is, or empty) with `block` in place of the
/// one between the markers, or added at the end when it has none. A new
/// file gets a heading of its own. Splicing the same block twice changes
/// nothing more.
pub fn splice(existing: &str, block: &str) -> String {
    if let (Some(a), Some(b)) = (existing.find(START), existing.find(END)) {
        if a < b {
            let end = b + END.len();
            return format!("{}{block}{}", &existing[..a], &existing[end..]);
        }
    }
    if existing.trim().is_empty() {
        return format!("# Motchy data\n\nYour settings and coding time, kept by Motchy.\n\n{block}\n");
    }
    format!("{}\n\n{block}\n", existing.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(date: &str, total: u32, project: &str, language: &str) -> Day {
        Day { date: date.into(), total, projects: vec![Share { name: project.into(), secs: total }], languages: vec![Share { name: language.into(), secs: total }], ..Day::default() }
    }

    fn sample() -> Vec<Day> {
        let mut d = vec![day("2026-10-31", 1800, "bms", "TypeScript"), day("2026-11-05", 3600, "motchy", "Rust"), day("2026-11-06", 7200, "motchy", "Rust"), day("2026-11-07", 900, "bms", "Go")];
        d[2].ai = 1800;
        d[2].editors = vec![Share { name: "Zed".into(), secs: 5400 }, Share { name: "Claude Code".into(), secs: 1800 }];
        d
    }

    #[test]
    fn bars_fill_to_the_share_and_never_vanish() {
        assert_eq!(bar(1.0, 4), "████");
        assert_eq!(bar(0.5, 4), "██░░");
        assert_eq!(bar(0.0, 4), "░░░░");
        assert_eq!(bar(0.01, 4), "█░░░", "a little shows");
        assert_eq!(bar(7.0, 4), "████");
        assert_eq!(bar(f64::NAN, 2), "░░");
    }

    #[test]
    fn the_front_page_is_this_month_in_weeks_not_dates() {
        let r = report(&sample(), "2026-11-07");
        assert!(r.starts_with(START) && r.ends_with(END));
        assert!(r.contains("## Coding report · November 2026") && r.contains("(coding-activity/README.md)"), "{r}");
        // Nov: 3600 + 7200 + 900 = 3h 15m; October's 30m is last month; 3 active days, 1h 05m each on average.
        assert!(r.contains("| 3h 15m | 30m | Nov 06 · 2h 00m | 3 | 1h 05m |"), "{r}");
        assert!(r.contains("Streak: 3 days"), "{r}");
        // Weeks, clipped to the month: Nov 01 is a Sunday, so the first week is one day.
        assert!(r.contains("Nov 01 – Nov 01  ░░░░░░░░░░░░░░░░░░░░  0m"), "{r}");
        assert!(r.contains("Nov 02 – Nov 07  ████████████████████  3h 15m"), "{r}");
        // No table of dates and nothing from other months.
        assert!(!r.contains("Per month") && !r.contains("Mon Nov") && !r.contains("Oct 31"), "{r}");
        // Where the month went: motchy 10800 of 11700 = 92%.
        assert!(r.contains("| motchy | 3h 00m | `█████████░` 92% |"), "{r}");
        assert!(r.contains("| Zed | 1h 30m |") && r.contains("| This month | 2h 45m | 30m | 15% |"), "{r}");
        assert_eq!(r, report(&sample(), "2026-11-07"));
    }

    #[test]
    fn a_month_with_nothing_yet_says_so_and_still_links_the_history() {
        let r = report(&sample(), "2026-12-02");
        assert!(r.contains("December 2026") && r.contains("Nothing tracked this month yet") && r.contains("coding-activity"), "{r}");
        // January looks back to December of the year before.
        let jan = report(&[day("2025-12-30", 600, "a", "b"), day("2026-01-02", 1200, "a", "b")], "2026-01-03");
        assert!(jan.contains("| 20m | 10m |"), "{jan}");
    }

    #[test]
    fn a_streak_counts_back_from_today_or_yesterday() {
        let streak = |today: &str| report(&sample(), today).lines().find(|l| l.starts_with("Streak")).unwrap().to_string();
        assert_eq!(streak("2026-11-07"), "Streak: 3 days");
        // Nothing yet today: yesterday's run still counts.
        assert_eq!(streak("2026-11-08"), "Streak: 3 days");
        // A day with nothing breaks it.
        assert_eq!(streak("2026-11-09"), "Streak: 0 days");
    }

    #[test]
    fn the_history_holds_every_year_in_its_own_file() {
        let mut d = sample();
        d.push(day("2025-03-10", 5400, "old", "C"));
        let files = history(&d, "2026-11-07");
        let names: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(names, ["coding-activity/README.md", "coding-activity/2025.md", "coding-activity/2026.md"]);
        let index = &files[0].1;
        // All time: 13500 + 5400 = 18900 s = 5h 15m; years newest first, linked.
        assert!(index.contains("| 5h 15m | 5 |"), "{index}");
        assert!(index.find("[2026](2026.md)").unwrap() < index.find("[2025](2025.md)").unwrap(), "{index}");
        assert!(index.contains("| old | 1h 30m |"), "{index}");
        let old = &files[1].1;
        assert!(old.contains("# 2025") && old.contains("Mar 2025") && !old.contains("Nov"), "{old}");
        let cur = &files[2].1;
        // Months newest first inside folds, with weeks and days.
        assert!(cur.find("<summary>November 2026").unwrap() < cur.find("<summary>October 2026").unwrap(), "{cur}");
        assert!(cur.contains("#### Per week") && cur.contains("Fri Nov 06  ") && cur.contains("</details>"), "{cur}");
        // A year that is over does not depend on today's date.
        assert_eq!(history(&d, "2026-11-07")[1], history(&d, "2026-12-30")[1]);
        assert!(history(&[], "2026-11-07").is_empty() && history(&d, "bad").is_empty());
    }

    #[test]
    fn without_days_or_with_a_bad_date_nothing_is_claimed() {
        assert!(report(&[], "2026-11-07").contains("Nothing tracked yet"));
        assert!(report(&sample(), "bad").contains("Nothing tracked yet"));
        // Days after the date are not in it.
        assert!(report(&[day("2026-12-01", 99, "x", "y")], "2026-11-07").contains("Nothing tracked yet"));
        // No agent and no editor: those sections are left out.
        let r = report(&[day("2026-11-07", 600, "a", "b")], "2026-11-07");
        assert!(!r.contains("AI-assisted") && !r.contains("### Editors"), "{r}");
    }

    #[test]
    fn names_cannot_break_the_page() {
        let r = report(&[day("2026-11-07", 600, "a|b <script>`x`\nz", "L")], "2026-11-07");
        assert!(r.contains("| a\\|b &lt;script>'x' z |"), "{r}");
        assert!(!r.contains("<script"), "{r}");
        // Nor can a name hold the markers.
        let r = report(&[day("2026-11-07", 600, "<!-- motchy:end -->", "L")], "2026-11-07");
        assert_eq!(r.matches(END).count(), 1);
    }

    #[test]
    fn only_the_block_changes_and_the_rest_of_the_readme_stays() {
        let block = report(&sample(), "2026-11-07");
        // A new file gets a heading.
        let new = splice("", &block);
        assert!(new.starts_with("# Motchy data") && new.contains(&block));
        // Splicing again changes nothing.
        assert_eq!(splice(&new, &block), new);
        // The person's own words above and below stay when the block is replaced.
        let mine = format!("# My data\n\nHello.\n\n{START}\nold\n{END}\n\nFooter.\n");
        let got = splice(&mine, &block);
        assert!(got.starts_with("# My data\n\nHello.\n\n") && got.ends_with("\n\nFooter.\n"), "{got}");
        assert!(got.contains(&block) && !got.contains("old"));
        // A README with no markers gets the block at its end.
        let plain = splice("# Notes\n\nText\n", &block);
        assert!(plain.starts_with("# Notes\n\nText\n\n") && plain.ends_with(&format!("{END}\n")), "{plain}");
        // A lone or reversed marker is not a block: it is left alone and the block added.
        let odd = splice(&format!("x {END} y {START}"), &block);
        assert!(odd.starts_with(&format!("x {END} y {START}")) && odd.ends_with(&format!("{END}\n")));
    }
}

//! The coding report in the sync repository's `README.md`: what the kept days
//! add up to, as Markdown a host shows on the repository's page. No I/O.
//!
//! The report fills one block between two marker comments and nothing else
//! of the file: whatever the person wrote above or below stays. It holds no
//! clock time, only the date it was made for, so the same days and the same
//! date give the same text and a sync with nothing new changes no file.

use crate::datetime::{civil_from_days, weekday_from_days, MONTH_SHORT, WEEKDAY_SHORT};
use crate::wakatime::{fmt_hm, parse_date, Day, Share};

pub const START: &str = "<!-- motchy:start -->";
pub const END: &str = "<!-- motchy:end -->";
/// How wide a bar is, in characters.
const BAR: usize = 20;
const SHARE_BAR: usize = 10;
/// Days drawn one by one, and rows in each list.
const DAYS_SHOWN: i64 = 14;
const MONTHS_SHOWN: usize = 12;
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
fn table(out: &mut String, heading: &str, first: &str, shares: &[Share]) {
    let whole: u64 = shares.iter().map(|s| s.secs as u64).sum();
    if whole == 0 {
        return;
    }
    out.push_str(&format!("### {heading}\n\n| {first} | Time | Share |\n|---|---:|---|\n"));
    for s in shares.iter().take(TOP) {
        out.push_str(&format!("| {} | {} | `{}` {}% |\n", cell(&s.name), fmt_hm(s.secs), bar(s.secs as f64 / whole as f64, SHARE_BAR), pct(s.secs as u64, whole)));
    }
    out.push('\n');
}

/// The report block for `days` as of `today` ("2026-11-07"), markers included,
/// ending with the closing marker (no newline after it). Empty `days` (or a
/// bad date) still give a block that says nothing is tracked yet.
pub fn report(days: &[Day], today: &str) -> String {
    let mut out = format!("{START}\n## Coding report\n\n");
    let Some(t) = parse_date(today) else {
        out.push_str(&format!("_Nothing tracked yet._\n{END}"));
        return out;
    };
    let mut days: Vec<(i64, &Day)> = days.iter().filter_map(|d| parse_date(&d.date).map(|z| (z, d))).filter(|(z, _)| *z <= t).collect();
    days.sort_by_key(|(z, _)| *z);
    if days.is_empty() {
        out.push_str(&format!("_Nothing tracked yet._\n{END}"));
        return out;
    }
    let total: u64 = days.iter().map(|(_, d)| d.total as u64).sum();
    let ai_total: u64 = days.iter().map(|(_, d)| d.ai as u64).sum();
    let last30: u64 = days.iter().filter(|(z, _)| t - z < 30).map(|(_, d)| d.total as u64).sum();
    let active: Vec<&(i64, &Day)> = days.iter().filter(|(_, d)| d.total > 0).collect();
    let by_day = |z: i64| days.iter().find(|(x, _)| *x == z).map_or(0, |(_, d)| d.total);

    // The streak counts back from today, or from yesterday while today has
    // nothing yet.
    let mut z = if by_day(t) > 0 { t } else { t - 1 };
    let mut streak = 0;
    while by_day(z) > 0 {
        streak += 1;
        z -= 1;
    }
    let best = active.iter().max_by(|a, b| a.1.total.cmp(&b.1.total).then(b.0.cmp(&a.0))).map(|(z, d)| format!("{} · {}", short_date(*z), fmt_hm(d.total))).unwrap_or_else(|| "–".into());
    let average = if active.is_empty() { 0 } else { (total / active.len() as u64) as u32 };
    let first = days[0].0;
    out.push_str(&format!("_Updated {today} · {} days tracked since {}_\n\n", days.len(), days_first(first)));
    out.push_str("| Total | Last 30 days | Best day | Streak | Daily average |\n|---|---|---|---|---|\n");
    out.push_str(&format!("| {} | {} | {} | {} | {} |\n\n", fmt_hm(total as u32), fmt_hm(last30 as u32), best, if streak == 1 { "1 day".to_string() } else { format!("{streak} days") }, fmt_hm(average)));

    // The last days, one line each, today last.
    let shown: Vec<i64> = ((t - DAYS_SHOWN + 1).max(first)..=t).collect();
    let max = shown.iter().map(|z| by_day(*z)).max().unwrap_or(0).max(1) as f64;
    out.push_str(&format!("### Last {} days\n\n```text\n", shown.len()));
    for z in shown {
        out.push_str(&format!("{} {}  {}  {}\n", WEEKDAY_SHORT[weekday_from_days(z) as usize % 7], short_date(z), bar(by_day(z) as f64 / max, BAR), fmt_hm(by_day(z))));
    }
    out.push_str("```\n\n");

    // Each month, newest first.
    let mut months: Vec<(String, u64)> = Vec::new();
    for (_, d) in &days {
        let m = d.date.get(..7).unwrap_or("").to_string();
        match months.iter_mut().find(|(k, _)| *k == m) {
            Some((_, v)) => *v += d.total as u64,
            None => months.push((m, d.total as u64)),
        }
    }
    months.reverse();
    months.truncate(MONTHS_SHOWN);
    let top = months.iter().map(|(_, v)| *v).max().unwrap_or(0).max(1) as f64;
    out.push_str("### Per month\n\n```text\n");
    for (m, v) in &months {
        out.push_str(&format!("{m}  {}  {}\n", bar(*v as f64 / top, BAR), fmt_hm(*v as u32)));
    }
    out.push_str("```\n\n");

    let (mut projects, mut languages, mut editors) = (Vec::new(), Vec::new(), Vec::new());
    for (_, d) in &days {
        add(&mut projects, &d.projects);
        add(&mut languages, &d.languages);
        add(&mut editors, &d.editors);
    }
    table(&mut out, "Projects", "Project", &projects);
    table(&mut out, "Languages", "Language", &languages);
    if ai_total > 0 {
        let ai30: u64 = days.iter().filter(|(z, _)| t - z < 30).map(|(_, d)| d.ai as u64).sum();
        out.push_str("### AI-assisted time\n\n| Period | You | AI | AI share |\n|---|---:|---:|---:|\n");
        for (label, all, ai) in [("All time", total, ai_total), ("Last 30 days", last30, ai30)] {
            out.push_str(&format!("| {label} | {} | {} | {}% |\n", fmt_hm(all.saturating_sub(ai) as u32), fmt_hm(ai as u32), pct(ai, all)));
        }
        out.push('\n');
    }
    table(&mut out, "Editors", "Editor", &editors);
    out.push_str(END);
    out
}

/// "2026-10-26" for a day number.
fn days_first(z: i64) -> String {
    let (y, m, d) = civil_from_days(z);
    format!("{y:04}-{m:02}-{d:02}")
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
    fn the_report_says_the_numbers_a_person_wants() {
        let r = report(&sample(), "2026-11-07");
        assert!(r.starts_with(START) && r.ends_with(END));
        // 1800 + 3600 + 7200 + 900 = 13500 s = 3h 45m; the last 30 days hold all four.
        assert!(r.contains("| 3h 45m | 3h 45m | Nov 06 · 2h 00m | 3 days | 56m |"), "{r}");
        assert!(r.contains("_Updated 2026-11-07 · 4 days tracked since 2026-10-31_"), "{r}");
        // Projects and languages as tables with their shares: motchy 10800 of 13500 = 80%.
        assert!(r.contains("| motchy | 3h 00m | `████████░░` 80% |"), "{r}");
        assert!(r.contains("| Rust | 3h 00m | `████████░░` 80% |"), "{r}");
        // Per month, newest first.
        assert!(r.find("2026-11").unwrap() < r.find("2026-10").unwrap());
        // The agent's part, and the editors.
        assert!(r.contains("| All time | 3h 15m | 30m | 13% |"), "{r}");
        assert!(r.contains("| Zed | 1h 30m |"), "{r}");
        // The same days and date give the same text.
        assert_eq!(r, report(&sample(), "2026-11-07"));
    }

    #[test]
    fn a_streak_counts_back_from_today_or_yesterday() {
        let streak = |today: &str| report(&sample(), today).lines().find(|l| l.starts_with("| 3h")).map(|l| l.split('|').nth(4).unwrap().trim().to_string()).unwrap();
        assert_eq!(streak("2026-11-07"), "3 days");
        // Nothing yet today: yesterday's run still counts.
        assert_eq!(streak("2026-11-08"), "3 days");
        // A day with nothing breaks it.
        assert_eq!(streak("2026-11-09"), "0 days");
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

//! Coding page: the time you code, from WakaTime's numbers kept by the app
//! (`poni_core::wakatime`).
//!
//! Left: today's time, the week and month, and the last seven days as bars.
//! Right: where the week went by project, the languages, and how many days
//! the app keeps (more than WakaTime's own window once it has run a while).
//! Nothing here asks for frames: the numbers change once a day, when the
//! sync reads them.

use crate::commands::Command;
use crate::gfx::{TextStyle, Weight};
use crate::state::AppState;
use crate::ui::{wid, PillStyle, Ui, PILL_SMALL};
use poni_core::datetime::{days_from_civil, weekday_from_days, WEEKDAY_SHORT};
use poni_core::geom::{palette, Color, Rect};
use poni_core::wakatime::{fmt_hm, Share, Summary};

const HEIGHT: f32 = 162.0;
const LEFT_W: f32 = 196.0;
const COL_GAP: f32 = 28.0;
const BAR_W: f32 = 20.0;
const BAR_GAP: f32 = 9.0;
const BARS_Y: f32 = 94.0;
const BARS_H: f32 = 40.0;
const ROW_H: f32 = 24.0;
const ROWS: usize = 3;

pub fn height(_st: &AppState) -> f32 {
    HEIGHT
}

pub fn draw(ui: &mut Ui, st: &mut AppState, area: Rect) {
    let accent = st.accent;
    // Three steps before there is anything to show: switch it on, point the
    // editors here, wait for them.
    if !st.settings.coding.track {
        return step(ui, st, area, "Track your coding time", "Motchy listens on this Mac for your editors' WakaTime plugins. Nothing leaves it.", Some(("Turn on", Command::CodingOn)));
    }
    if !st.coding.plugins_ready {
        return step(ui, st, area, "Point your editors at Motchy", "Adds one line (api_url) to ~/.wakatime.cfg and keeps a backup of the file.", Some(("Set up plugins", Command::CodingSetup)));
    }
    let s = st.coding.summary.clone();
    if s.kept == 0 {
        return step(ui, st, area, "Waiting for your editors", "Code for a couple of minutes. An editor that was already open may need a restart.", None);
    }
    left(ui, st, area, &s, accent);
    right(ui, st, area, &s, accent);
}

/// A centred message for a page with nothing to show yet, its one button,
/// and what went wrong if something did.
fn step(ui: &mut Ui, st: &mut AppState, area: Rect, title: &str, line: &str, action: Option<(&str, Command)>) {
    let top = area.y + 10.0;
    ui.c.text(title, Rect::new(area.x, top, area.w, 22.0), &TextStyle::headline().center());
    ui.c.text(line, Rect::new(area.x + 20.0, top + 26.0, area.w - 40.0, 34.0), &TextStyle::footnote().color(palette::TEXT_DIM).center());
    if let Some((label, cmd)) = action {
        let w = ui.pill_width(label, false, true).max(112.0);
        let r = Rect::new(area.x + (area.w - w) / 2.0, top + 66.0, w, PILL_SMALL);
        if ui.pill(wid("coding.step"), r, label, None, PillStyle::Primary(st.accent)).clicked {
            st.cmd(cmd);
        }
    }
    if !st.coding.error.is_empty() {
        ui.c.text(&st.coding.error, Rect::new(area.x + 20.0, top + 100.0, area.w - 40.0, 30.0), &TextStyle::footnote().color(palette::ORANGE).center());
    }
}

fn left(ui: &mut Ui, st: &AppState, area: Rect, s: &Summary, accent: Color) {
    let x = area.x;
    ui.c.text("TODAY", Rect::new(x, area.y, LEFT_W, 14.0), &TextStyle::caption().weight(Weight::Bold).color(accent));
    ui.c.text(&fmt_hm(s.today), Rect::new(x, area.y + 14.0, LEFT_W, 38.0), &TextStyle::new(30.0).weight(Weight::Semibold).tabular());
    let line = format!("Week {} · Month {}", fmt_hm(s.week_total), fmt_hm(s.month_total));
    ui.c.text(&line, Rect::new(x, area.y + 54.0, LEFT_W, 16.0), &TextStyle::footnote().color(palette::TEXT_DIM));
    // What a coding agent did is in those numbers; this says how much.
    if s.ai_week > 0 {
        let ai = format!("AI {} today · {} this week", fmt_hm(s.ai_today), fmt_hm(s.ai_week));
        ui.c.text(&ai, Rect::new(x, area.y + 70.0, LEFT_W, 16.0), &TextStyle::footnote().color(accent));
    }

    // Seven bars, today last, the weekday under each.
    let t = st.local;
    let today_wd = weekday_from_days(days_from_civil(t.year, t.month.clamp(1, 12), t.day.clamp(1, 31))) as usize;
    let max = s.week.iter().copied().max().unwrap_or(0).max(1) as f32;
    let initial = TextStyle::caption().weight(Weight::Semibold).color(palette::TEXT_FAINT).center();
    for (i, secs) in s.week.iter().copied().enumerate() {
        let bx = x + i as f32 * (BAR_W + BAR_GAP);
        let h = if secs == 0 { 2.0 } else { (BARS_H * secs as f32 / max).max(4.0) };
        let color = if i == 6 { accent } else { Color::white(0.28) };
        ui.c.fill_rrect(Rect::new(bx, area.y + BARS_Y + BARS_H - h, BAR_W, h), 3.0, if secs == 0 { Color::white(0.10) } else { color });
        let wd = WEEKDAY_SHORT[(today_wd + i + 1) % 7];
        ui.c.text(&wd[..1], Rect::new(bx - 3.0, area.y + BARS_Y + BARS_H + 3.0, BAR_W + 6.0, 14.0), &initial);
    }
}

fn right(ui: &mut Ui, st: &AppState, area: Rect, s: &Summary, accent: Color) {
    let x = area.x + LEFT_W + COL_GAP;
    let w = (area.right() - x).max(0.0);
    ui.c.text("PROJECTS · 7 DAYS", Rect::new(x, area.y, w, 14.0), &TextStyle::caption().weight(Weight::Bold).color(palette::TEXT_FAINT));

    let top = s.projects.first().map_or(1, |p| p.secs).max(1) as f32;
    if s.projects.is_empty() {
        ui.c.text("Nothing in the last seven days.", Rect::new(x, area.y + 24.0, w, 18.0), &TextStyle::footnote().color(palette::TEXT_DIM));
    }
    let time_w = 64.0;
    for (i, p) in s.projects.iter().take(ROWS).enumerate() {
        let y = area.y + 22.0 + i as f32 * ROW_H;
        ui.c.text(&p.name, Rect::new(x, y, (w - time_w).max(0.0), 16.0), &TextStyle::body());
        ui.c.text(&fmt_hm(p.secs), Rect::new(x + w - time_w, y, time_w, 16.0), &TextStyle::footnote().tabular().color(palette::TEXT_DIM).right());
        ui.c.fill_rrect(Rect::new(x, y + 17.0, w, 3.0), 1.5, Color::white(0.10));
        ui.c.fill_rrect(Rect::new(x, y + 17.0, (w * p.secs as f32 / top).max(3.0).min(w), 3.0), 1.5, if i == 0 { accent } else { accent.mul_alpha(0.55) });
    }

    let y = area.y + 22.0 + ROWS as f32 * ROW_H + 4.0;
    let mut next = y + 18.0;
    if let Some(line) = languages_line(&s.languages) {
        ui.c.text(&line, Rect::new(x, y, w, 16.0), &TextStyle::footnote().color(palette::TEXT_DIM));
    }
    if let Some(line) = languages_line(&s.editors) {
        ui.c.text(&format!("Editors  {line}"), Rect::new(x, next, w, 16.0), &TextStyle::footnote().color(palette::TEXT_DIM));
        next += 18.0;
    }
    // What went wrong last time, or how much history the app keeps.
    // A sync's trouble only counts where a repository is set.
    let sync_trouble = if st.settings.sync.repo_url.is_some() { &st.sync.error } else { &st.coding.error };
    let problem = if st.coding.error.is_empty() { sync_trouble } else { &st.coding.error };
    let (note, color) = if problem.is_empty() { (format!("{} days kept since {}", s.kept, s.since), palette::TEXT_FAINT) } else { (problem.clone(), palette::ORANGE) };
    ui.c.text(&note, Rect::new(x, next, w, 14.0), &TextStyle::caption().color(color));
}

/// "Rust 62% · Swift 20% · Go 9%": the top three languages' share of the week.
fn languages_line(langs: &[Share]) -> Option<String> {
    let total: u32 = langs.iter().map(|l| l.secs).sum();
    if total == 0 {
        return None;
    }
    Some(langs.iter().take(3).map(|l| format!("{} {}%", l.name, (l.secs as f32 * 100.0 / total as f32).round() as u32)).collect::<Vec<_>>().join(" · "))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn share(name: &str, secs: u32) -> Share {
        Share { name: name.into(), secs }
    }

    #[test]
    fn the_languages_line_gives_each_share_of_the_week() {
        assert_eq!(languages_line(&[]), None);
        assert_eq!(languages_line(&[share("Rust", 620), share("Swift", 200), share("Go", 90), share("Sh", 90)]).as_deref(), Some("Rust 62% · Swift 20% · Go 9%"));
        assert_eq!(languages_line(&[share("Rust", 5)]).as_deref(), Some("Rust 100%"));
    }
}

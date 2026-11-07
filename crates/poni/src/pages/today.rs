//! Today page: your day at a glance.
//!
//! On the left the date and five weeks from this Monday (today filled); on
//! the right four tiles: CPU, memory, disk and battery (or uptime on a
//! desktop). On a Mac the right side is the agenda instead: the day picked
//! in the weeks, its events from Calendar, and a form for a new one. On an
//! island narrower than [`COMPACT_BELOW`] the weeks make way.
//! The header leaves the date out on this page, so the page always shows it.
//!
//! Stats are sampled once a second by the app only while the tiles show
//! (this page is open). The page itself never asks for continuous frames.

use crate::commands::Command;
use crate::gfx::{TextStyle, Weight};
use crate::icons::Icon;
use crate::state::{AppState, PowerState, SysStats};
use crate::ui::{wid, wid_n, PillStyle, Ui, PILL_SMALL};
use poni_core::agenda::{self, Access};
use poni_core::datetime::{civil_from_days, days_from_civil, fmt_clock, weekday_from_days, LocalDateTime, MONTH_SHORT, WEEKDAY_SHORT};
use poni_core::geom::{palette, Color, Point, Rect};
use poni_core::sysmon::Module;

const HEIGHT: f32 = 200.0;

/// Below this content width the weeks make way for the tiles (the island can
/// be made as narrow as 480).
const COMPACT_BELOW: f32 = 480.0;
const CELL_W: f32 = 23.0;
const GRID_W: f32 = 7.0 * CELL_W;
/// A week's row: the date.
const CELL_H: f32 = 26.0;
/// Five weeks from this week's Monday.
const GRID_DAYS: i64 = 35;
/// The initials row, under the weekday and the date.
const GRID_Y: f32 = 48.0;
const INITIALS_H: f32 = 16.0;
const COL_GAP: f32 = 20.0;
const TILE_GAP: f32 = 8.0;
const TILE_RADIUS: f32 = 16.0;
const RING_R: f32 = 16.0;
const RING_W: f32 = 4.0;
/// Neutral track shared by every progress ring in the app.
const RING_TRACK: Color = Color::white(0.14);
const RING_ICON: f32 = 12.0;

const WEEKDAY_LONG: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const MONTH_LONG: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
/// Monday-first initials.
const INITIALS: [&str; 7] = ["M", "T", "W", "T", "F", "S", "S"];

pub fn height(_st: &AppState) -> f32 {
    HEIGHT
}

/// The stat tiles show (the app samples the stats only meanwhile). With the
/// agenda there are none.
pub fn shows_tiles(st: &AppState) -> bool {
    !st.ui.agenda.on
}

/// Nothing on this page moves by itself.
pub fn next_wake(_st: &AppState) -> Option<f64> {
    None
}

pub fn draw(ui: &mut Ui, st: &mut AppState, area: Rect) {
    // Fixed layout height: the island's spring may still be opening.
    let area = Rect::new(area.x, area.y, area.w, HEIGHT);
    let compact = area.w < COMPACT_BELOW;
    let x = if compact { area.x } else { area.x + GRID_W + COL_GAP };
    let right = Rect::new(x, area.y, (area.right() - x).max(0.0), area.h);
    let left = Rect::new(area.x, area.y, GRID_W, area.h);
    if !compact {
        draw_weeks_grid(ui, st, left);
    }
    // The bottom-right tile shares the Pill card's corner.
    let card = super::card::pill_card(st, area);
    if st.ui.agenda.on {
        draw_agenda(ui, st, right, card);
    } else {
        draw_tiles(ui, st, right, card);
    }
}

// ------------------------------------------------------------------ the weeks

/// The date and five weeks from this Monday: today filled.
fn draw_weeks_grid(ui: &mut Ui, st: &mut AppState, r: Rect) {
    let t = st.local;
    if !valid_date(&t) {
        return;
    }
    let today = days_from_civil(t.year, t.month, t.day);
    let weekday = WEEKDAY_LONG[weekday_from_days(today) as usize % 7].to_uppercase();
    ui.c.text(&weekday, Rect::new(r.x, r.y, r.w, 16.0), &TextStyle::caption().weight(Weight::Bold).color(st.accent));
    let date = format!("{} {}", t.day, MONTH_LONG[(t.month.clamp(1, 12) - 1) as usize]);
    ui.c.text(&date, Rect::new(r.x, r.y + 16.0, r.w, 24.0), &TextStyle::value());

    // Every initial in one tone; weekend days are told apart by the dimmer numbers.
    let initials_st = TextStyle::caption().weight(Weight::Semibold).color(palette::TEXT_FAINT).center();
    for (col, s) in INITIALS.iter().enumerate() {
        ui.c.text(s, Rect::new(r.x + col as f32 * CELL_W, r.y + GRID_Y, CELL_W, INITIALS_H), &initials_st);
    }
    let top = r.y + GRID_Y + INITIALS_H;
    let start = today - (i64::from(weekday_from_days(today)) + 6) % 7;
    let num_st = TextStyle::footnote().tabular().center();
    let step = Color::white(0.2);
    let picked = if st.ui.agenda.on && st.ui.agenda.day != 0 { st.ui.agenda.day } else { today };
    for i in 0..GRID_DAYS {
        let day = start + i;
        let (row, col) = ((i / 7) as f32, (i % 7) as usize);
        let cell = Rect::new(r.x + col as f32 * CELL_W, top + row * CELL_H, CELL_W, CELL_H);
        let num = Rect::new(cell.x, cell.y, cell.w, 20.0);
        let c = Point::new(num.center().x, num.center().y);
        // With the agenda, a day is a button: it picks the day the agenda shows.
        if st.ui.agenda.on && ui.button(wid_n("today-day", i as u64), cell).clicked {
            st.cmd(Command::CalendarDay(if day == today { 0 } else { day }));
        }
        if day == picked && day != today {
            ui.c.stroke_circle(c, 9.5, st.accent, 1.5);
        }
        let (_, _, d) = civil_from_days(day);
        let label = d.to_string();
        if day == today {
            ui.c.fill_circle(c, 9.5, st.accent);
            ui.c.text(&label, num, &num_st.weight(Weight::Bold).color(ink_on(st.accent)));
        } else {
            let ink = if day < today {
                Color::white(0.32)
            } else if col >= 5 {
                palette::TEXT_DIM
            } else {
                Color::white(0.9)
            };
            let st_num = if d == 1 { num_st.weight(Weight::Bold) } else { num_st };
            ui.c.text(&label, num, &st_num.color(ink));
        }
        // Where a month starts: a step line between its 1st and the days before it.
        if d == 1 && i > 0 {
            let y = cell.y - 1.5;
            if row > 0.0 {
                ui.c.line(Point::new(cell.x, y), Point::new(r.x + GRID_W, y), step, 1.0);
            }
            if col > 0 {
                ui.c.line(Point::new(cell.x, y), Point::new(cell.x, y + CELL_H), step, 1.0);
                if row < 4.0 {
                    ui.c.line(Point::new(r.x, y + CELL_H), Point::new(cell.x, y + CELL_H), step, 1.0);
                }
            }
        }
    }
}

// ------------------------------------------------------------------ agenda

const HEAD_H: f32 = 26.0;
const FORM_ROW_H: f32 = 34.0;

/// The Mac's UTC offset in seconds, from its clock.
fn offset_s(st: &AppState) -> i64 {
    offset_s_of(st.local, st.unix)
}

/// The day the agenda shows, as days since 1970-01-01.
fn agenda_day(st: &AppState) -> i64 {
    let a = &st.ui.agenda;
    if a.day != 0 {
        a.day
    } else {
        days_from_civil(st.local.year, st.local.month, st.local.day)
    }
}

fn draw_agenda(ui: &mut Ui, st: &mut AppState, r: Rect, card: Option<(Rect, f32)>) {
    if r.w < 120.0 {
        return;
    }
    let day = agenda_day(st);
    let today = days_from_civil(st.local.year, st.local.month, st.local.day);
    let label = if day == today {
        "Today".to_string()
    } else {
        let (_, m, d) = civil_from_days(day);
        format!("{} {} {}", WEEKDAY_SHORT[weekday_from_days(day) as usize % 7], d, MONTH_SHORT[(m.clamp(1, 12) - 1) as usize])
    };
    ui.c.text(&label, Rect::new(r.x, r.y, (r.w - 30.0).max(0.0), HEAD_H), &TextStyle::headline());
    let body = Rect::new(r.x, r.y + HEAD_H + 6.0, r.w, (r.h - HEAD_H - 6.0).max(0.0));

    match st.ui.agenda.access {
        Access::Ask => {
            super::card::fill_box(ui, card, body, TILE_RADIUS, palette::SURFACE);
            let c = body.center();
            ui.c.text("Show your Mac's calendar here", Rect::new(body.x, c.y - 30.0, body.w, 20.0), &TextStyle::footnote().color(palette::TEXT_DIM).center());
            let btn = Rect::from_center(Point::new(c.x, c.y + 10.0), 150.0, 30.0);
            let resp = ui.button(wid("today-allow"), btn);
            ui.c.fill_rrect(btn, 15.0, if resp.hovered { st.accent.with_alpha(0.85) } else { st.accent });
            ui.c.text("Allow Calendars", btn, &TextStyle::caption().weight(Weight::Bold).color(ink_on(st.accent)).center());
            if resp.clicked {
                st.cmd(Command::CalendarAllow);
            }
        }
        Access::Denied => {
            super::card::fill_box(ui, card, body, TILE_RADIUS, palette::SURFACE);
            let c = body.center();
            let dim = TextStyle::footnote().color(palette::TEXT_DIM).center();
            ui.c.text("Calendars is off for Motchy", Rect::new(body.x, c.y - 20.0, body.w, 20.0), &dim);
            ui.c.text("System Settings › Privacy & Security", Rect::new(body.x, c.y, body.w, 20.0), &dim);
        }
        Access::Granted => {
            let add = Rect::new(r.right() - 24.0, r.y + 1.0, 24.0, 24.0);
            let icon = if st.ui.agenda.adding { Icon::Close } else { Icon::Plus };
            if ui.icon_button(wid("today-add"), add, icon, Color::white(0.8), 14.0).clicked {
                let open = !st.ui.agenda.adding;
                st.ui.agenda.adding = open;
                st.ui.agenda.error = None;
                if open && st.ui.agenda.time.is_empty() {
                    let minutes = (st.unix + offset_s(st)).rem_euclid(86_400) / 60;
                    st.ui.agenda.time = agenda::next_hour(minutes);
                }
            }
            if st.ui.agenda.adding {
                draw_form(ui, st, body, day);
            } else {
                draw_events(ui, st, body);
            }
        }
    }
}

/// A card's height and the gap between cards.
const CARD_H: f32 = 44.0;
const CARD_GAP: f32 = 6.0;

/// The day's events as cards: the calendar's colour at the left, the time
/// and what is left of it under the title. The one on now is filled with the
/// accent and shows how far through it is; one that ended is dim; one with a
/// video call has Join.
fn draw_events(ui: &mut Ui, st: &mut AppState, body: Rect) {
    if st.ui.agenda.events.is_empty() {
        super::empty_state(ui, body, Icon::Calendar, "No events", "Nothing on this day");
        return;
    }
    let (off, now, accent, h24) = (offset_s(st), st.unix, st.accent, st.settings.use_24h);
    let n = st.ui.agenda.events.len();
    let max_scroll = (n as f32 * (CARD_H + CARD_GAP) - CARD_GAP - body.h).max(0.0);
    if max_scroll > 0.0 {
        st.ui.agenda.scroll -= ui.wheel_in(body) * (CARD_H + CARD_GAP);
    }
    st.ui.agenda.scroll = st.ui.agenda.scroll.clamp(0.0, max_scroll);
    let scroll = st.ui.agenda.scroll;
    let mut join = None;
    ui.c.clip(body);
    for (i, e) in st.ui.agenda.events.iter().enumerate() {
        let card = Rect::new(body.x, body.y + i as f32 * (CARD_H + CARD_GAP) - scroll, body.w, CARD_H);
        if card.bottom() < body.y || card.y > body.bottom() {
            continue;
        }
        let phase = agenda::phase(e, now);
        let running = matches!(phase, agenda::Phase::Now { .. });
        let ended = phase == agenda::Phase::Ended;
        let fill = if running { accent } else { palette::SURFACE };
        super::card::fill_box(ui, None, card, 12.0, fill);
        let (ink, sub) = if running {
            (ink_on(accent), ink_on(accent).with_alpha(0.75))
        } else if ended {
            (palette::TEXT_FAINT, palette::TEXT_FAINT)
        } else {
            (palette::TEXT, palette::TEXT_DIM)
        };
        // The calendar's colour (on the accent card it would vanish: a light bar then).
        let bar = if running { ink.with_alpha(0.55) } else { Color::rgba(e.rgb[0], e.rgb[1], e.rgb[2], if ended { 0.45 } else { 1.0 }) };
        ui.c.fill_rrect(Rect::new(card.x + 8.0, card.y + 8.0, 3.0, card.h - 16.0), 1.5, bar);

        // Join, at the right, unless it is over.
        let mut text_right = card.right() - 10.0;
        if let Some(url) = e.link.as_ref().filter(|_| !ended) {
            let w = ui.pill_width("Join", false, true);
            let pr = Rect::new(card.right() - 8.0 - w, card.center().y - PILL_SMALL / 2.0, w, PILL_SMALL);
            let style = if running { PillStyle::Primary(ink_on(accent)) } else { PillStyle::UtilityAccent(accent) };
            if ui.pill(wid_n("today-join", i as u64), pr, "Join", None, style).clicked {
                join = Some(url.clone());
            }
            text_right = pr.x - 8.0;
        }
        let tx = card.x + 19.0;
        let tw = (text_right - tx).max(0.0);
        let title = if e.title.is_empty() { "(no title)" } else { e.title.as_str() };
        ui.c.text(title, Rect::new(tx, card.y + 5.0, tw, 18.0), &TextStyle::footnote().weight(Weight::Semibold).color(ink));
        let clock = |t: i64| agenda::clock((t + off).rem_euclid(86_400) / 60, h24);
        let when = if e.all_day { String::new() } else { format!("{}–{}", clock(e.start), clock(e.end)) };
        let status = agenda::status(e, now);
        let line = match (when.is_empty(), status.is_empty() || e.all_day) {
            (true, _) => status,
            (false, true) => when,
            (false, false) => format!("{when} · {status}"),
        };
        ui.c.text(&line, Rect::new(tx, card.y + 23.0, tw, 16.0), &TextStyle::caption().tabular().color(sub));
        if let agenda::Phase::Now { progress, .. } = phase {
            let track = Rect::new(card.x + 19.0, card.bottom() - 4.0, card.w - 30.0, 2.0);
            ui.c.fill_rrect(track, 1.0, ink.with_alpha(0.25));
            ui.c.fill_rrect(Rect::new(track.x, track.y, track.w * progress, track.h), 1.0, ink);
        }
    }
    ui.c.unclip();
    if let Some(url) = join {
        st.cmd(Command::OpenUrl(url));
    }
}

/// The new-event form: a title (Enter saves), a start time and the calendar.
fn draw_form(ui: &mut Ui, st: &mut AppState, body: Rect, day: i64) {
    let accent = st.accent;
    let title_r = Rect::new(body.x, body.y, body.w, FORM_ROW_H);
    let resp = ui.text_input_row(wid("today-title"), title_r, &mut st.ui.agenda.title, "New event, Enter to save", 120, accent, Some(Icon::Plus));
    let time_r = Rect::new(body.x, body.y + FORM_ROW_H + 6.0, 96.0, FORM_ROW_H);
    let time = ui.text_input_row(wid("today-time"), time_r, &mut st.ui.agenda.time, "14:00", 5, accent, Some(Icon::Clock));
    // The calendar: one click moves to the next.
    let cal_r = Rect::new(time_r.right() + 8.0, time_r.y, (body.right() - time_r.right() - 8.0).max(0.0), FORM_ROW_H);
    let n = st.ui.agenda.calendars.len();
    let name = st.ui.agenda.calendars.get(st.ui.agenda.calendar).map_or("No calendar", |c| c.title.as_str()).to_string();
    let cal = ui.button(wid("today-cal"), cal_r);
    ui.c.fill_rrect(cal_r, 12.0, if cal.hovered { palette::SURFACE_HOVER } else { palette::SURFACE });
    ui.c.icon(Icon::Calendar, Rect::from_center(Point::new(cal_r.x + 16.0, cal_r.center().y), 12.0, 12.0), Color::white(0.72));
    ui.c.text(&name, Rect::new(cal_r.x + 30.0, cal_r.y, (cal_r.w - 38.0).max(0.0), cal_r.h), &TextStyle::footnote().color(palette::TEXT));
    if cal.clicked && n > 1 {
        st.ui.agenda.calendar = (st.ui.agenda.calendar + 1) % n;
    }
    if let Some(e) = st.ui.agenda.error.clone() {
        ui.c.text(&e, Rect::new(body.x, body.bottom() - 18.0, body.w, 16.0), &TextStyle::caption().color(palette::ORANGE));
    }
    if resp.cancelled || time.cancelled {
        st.ui.agenda.adding = false;
    } else if resp.submitted || time.submitted {
        submit_event(st, day);
    }
}

/// Send the form as an event of one hour on `day`.
fn submit_event(st: &mut AppState, day: i64) {
    let a = &mut st.ui.agenda;
    let title = a.title.split_whitespace().collect::<Vec<_>>().join(" ");
    if title.is_empty() {
        return;
    }
    let Some(minutes) = agenda::parse_time(&a.time) else {
        a.error = Some("Start time like 14:00".into());
        return;
    };
    a.error = None;
    let start = day * 86_400 + minutes * 60 - offset_s_of(st.local, st.unix);
    let end = start + 3600;
    let calendar = st.ui.agenda.calendar;
    st.cmd(Command::CalendarAdd { title, start, end, calendar });
}

fn offset_s_of(local: LocalDateTime, unix: i64) -> i64 {
    let secs = days_from_civil(local.year, local.month, local.day) * 86_400 + i64::from(local.hour) * 3600 + i64::from(local.minute) * 60 + i64::from(local.second);
    ((secs - unix) as f64 / 900.0).round() as i64 * 900
}

// ------------------------------------------------------------------ tiles

struct Tile {
    icon: Icon,
    label: &'static str,
    /// Ring fill fraction; `None` = track only (no meaningful fraction).
    frac: Option<f32>,
    color: Color,
    value: String,
    /// Preferred detail line, and a shorter fallback when it doesn't fit.
    detail: String,
    detail_short: String,
}

fn draw_tiles(ui: &mut Ui, st: &mut AppState, r: Rect, card: Option<(Rect, f32)>) {
    if r.w < 120.0 {
        return;
    }
    let s = &st.stats;
    let battery = st.power.has_battery;
    let tiles = [cpu_tile(s), memory_tile(s), disk_tile(s), if battery { battery_tile(&st.power) } else { uptime_tile(st) }];
    // With System on, each ring opens its module on the System page (uptime
    // lives under CPU there); without it the tiles are only to read.
    let modules = [Module::Cpu, Module::Memory, Module::Drives, if battery { Module::Battery } else { Module::Cpu }];
    let system = st.system_on();
    let rows = r.rows(2, TILE_GAP);
    for (i, tile) in tiles.iter().enumerate() {
        let row = rows[i / 2];
        let cell = row.columns(2, TILE_GAP)[i % 2];
        let resp = system.then(|| ui.button(wid_n("today-tile", i as u64), cell));
        draw_tile(ui, cell, tile, resp.is_some_and(|r| r.hovered), card);
        if resp.is_some_and(|r| r.clicked) {
            st.cmd(Command::OpenSystem(modules[i]));
        }
    }
}

fn draw_tile(ui: &mut Ui, r: Rect, t: &Tile, hovered: bool, card: Option<(Rect, f32)>) {
    super::card::fill_box(ui, card, r, TILE_RADIUS, if hovered { palette::SURFACE_HOVER } else { palette::SURFACE });
    let cy = r.center().y;
    let c = Point::new(r.x + 12.0 + RING_R, cy);
    match t.frac {
        Some(f) => ui.c.ring(c, RING_R, RING_W, f.clamp(0.0, 1.0), RING_TRACK, t.color),
        None => ui.c.stroke_circle(c, RING_R, RING_TRACK, RING_W),
    }
    ui.c.icon(t.icon, Rect::from_center(c, RING_ICON, RING_ICON), Color::white(0.72));

    // Text sits on the tile's fill, so the faintest tone here is TEXT_DIM.
    let tx = c.x + RING_R + 11.0;
    let tw = (r.right() - 10.0 - tx).max(0.0);
    ui.c.text(t.label, Rect::new(tx, cy - 26.0, tw, 15.0), &TextStyle::caption().weight(Weight::Semibold).color(palette::TEXT_DIM));
    let value_col = if t.value == PLACEHOLDER { palette::TEXT_DIM } else { palette::TEXT };
    ui.c.text(&t.value, Rect::new(tx, cy - 12.0, tw, 24.0), &TextStyle::value().tabular().color(value_col));
    if !t.detail.is_empty() {
        let st = TextStyle::caption().color(palette::TEXT_DIM);
        let fits = ui.c.measure(&t.detail, &st, 10_000.0).0 <= tw;
        let detail = if fits || t.detail_short.is_empty() { &t.detail } else { &t.detail_short };
        ui.c.text(detail, Rect::new(tx, cy + 11.0, tw, 15.0), &st);
    }
}

const PLACEHOLDER: &str = "–";

/// Ring colour for a "how full" fraction: white, then orange, then red.
fn load_color(f: f32) -> Color {
    if f >= 0.9 {
        palette::RED
    } else if f >= 0.75 {
        palette::ORANGE
    } else {
        Color::white(0.92)
    }
}

fn placeholder(icon: Icon, label: &'static str) -> Tile {
    Tile { icon, label, frac: None, color: Color::white(0.92), value: PLACEHOLDER.into(), detail: String::new(), detail_short: String::new() }
}

fn cpu_tile(s: &SysStats) -> Tile {
    if !s.valid || !s.cpu.is_finite() {
        return placeholder(Icon::Cpu, "CPU");
    }
    let f = s.cpu.clamp(0.0, 1.0);
    let detail = match f {
        f if f < 0.10 => "Idle",
        f if f < 0.35 => "Light load",
        f if f < 0.70 => "Moderate",
        f if f < 0.90 => "Busy",
        _ => "Very busy",
    };
    Tile { icon: Icon::Cpu, label: "CPU", frac: Some(f), color: load_color(f), value: pct(f), detail: detail.into(), detail_short: String::new() }
}

fn memory_tile(s: &SysStats) -> Tile {
    if !s.valid || s.mem_total == 0 {
        return placeholder(Icon::Memory, "Memory");
    }
    let f = (s.mem_used as f64 / s.mem_total as f64).clamp(0.0, 1.0) as f32;
    let detail = format!("{} / {}", gb_one_decimal(s.mem_used), fmt_bytes(s.mem_total));
    let short = gb_one_decimal(s.mem_used) + " GB";
    Tile { icon: Icon::Memory, label: "Memory", frac: Some(f), color: load_color(f), value: pct(f), detail, detail_short: short }
}

fn disk_tile(s: &SysStats) -> Tile {
    if !s.valid || s.disk_total == 0 {
        return placeholder(Icon::Disk, "Disk");
    }
    let free = s.disk_free.min(s.disk_total);
    let used = 1.0 - (free as f64 / s.disk_total as f64) as f32;
    let detail = format!("free of {}", fmt_bytes(s.disk_total));
    Tile { icon: Icon::Disk, label: "Disk", frac: Some(used), color: load_color(used), value: fmt_bytes(free), detail, detail_short: "free".into() }
}

fn battery_tile(p: &PowerState) -> Tile {
    let percent = p.percent.min(100);
    let f = percent as f32 / 100.0;
    let (detail, color) = if p.charging {
        ("Charging", palette::GREEN)
    } else if p.on_ac {
        (if percent >= 99 { "Fully charged" } else { "Plugged in" }, palette::GREEN)
    } else if percent <= 20 {
        ("Low battery", palette::RED)
    } else if p.saver {
        ("Battery saver", palette::YELLOW)
    } else {
        ("On battery", Color::white(0.92))
    };
    let short = if p.charging || p.on_ac { "Plugged in" } else { "On battery" };
    Tile {
        icon: if p.charging { Icon::Bolt } else { Icon::Battery },
        label: "Battery",
        frac: Some(f),
        color,
        value: format!("{percent}%"),
        detail: detail.into(),
        detail_short: short.into(),
    }
}

fn uptime_tile(st: &AppState) -> Tile {
    let s = &st.stats;
    if !s.valid {
        return placeholder(Icon::Clock, "Uptime");
    }
    let (detail, detail_short) = boot_label(&st.local, s.uptime_s, st.settings.use_24h).unwrap_or_default();
    Tile { icon: Icon::Clock, label: "Uptime", frac: None, color: Color::white(0.92), value: fmt_uptime(s.uptime_s), detail, detail_short }
}

// ------------------------------------------------------------------ formatting

fn pct(f: f32) -> String {
    format!("{}%", (f * 100.0).round().clamp(0.0, 100.0) as i32)
}

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// "6.1" (GiB, one decimal).
fn gb_one_decimal(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / GIB)
}

/// Compact size with unit: "640 MB", "8.4 GB", "212 GB", "1.8 TB"
/// (binary units, as Explorer shows them).
fn fmt_bytes(bytes: u64) -> String {
    let gb = bytes as f64 / GIB;
    if gb >= 1000.0 {
        let tb = gb / 1024.0;
        if tb >= 10.0 {
            format!("{tb:.0} TB")
        } else {
            format!("{tb:.1} TB")
        }
    } else if gb >= 10.0 {
        format!("{gb:.0} GB")
    } else if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

/// "3d 4h", "5h 12m", "42m".
fn fmt_uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, (secs / 3600) % 24, (secs / 60) % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// When the PC booted: ("Since 09:14", "Since today"), ("Since Mon 09:14",
/// "Since Mon") or ("Since 3 Oct", same) — full text plus a narrow fallback.
fn boot_label(now: &LocalDateTime, uptime_s: u64, use_24h: bool) -> Option<(String, String)> {
    if !valid_date(now) {
        return None;
    }
    let today = days_from_civil(now.year, now.month, now.day);
    let now_s = today * 86_400 + now.hour as i64 * 3600 + now.minute as i64 * 60 + now.second as i64;
    let boot_s = now_s - i64::try_from(uptime_s).ok()?;
    let boot_day = boot_s.div_euclid(86_400);
    let sod = boot_s.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(boot_day);
    let boot = LocalDateTime { year, month, day, hour: (sod / 3600) as u8, minute: ((sod / 60) % 60) as u8, second: (sod % 60) as u8 };
    let clock = fmt_clock(&boot, use_24h);
    let ago = today - boot_day;
    Some(if ago <= 0 {
        (format!("Since {clock}"), "Since today".to_string())
    } else if ago < 7 {
        let wd = WEEKDAY_SHORT[weekday_from_days(boot_day) as usize % 7];
        (format!("Since {wd} {clock}"), format!("Since {wd}"))
    } else {
        let s = format!("Since {} {}", day, MONTH_SHORT[(month.clamp(1, 12) - 1) as usize]);
        (s.clone(), s)
    })
}

/// The app fills `local` from GetLocalTime; guard against the zeroed default.
fn valid_date(t: &LocalDateTime) -> bool {
    (1..=12).contains(&t.month) && t.day >= 1
}

/// Text colour that stays legible on an accent fill.
fn ink_on(fill: Color) -> Color {
    if fill.luminance() > 0.6 {
        palette::ON_ACCENT_DARK
    } else {
        palette::ON_ACCENT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(year: i32, month: u8, day: u8, hour: u8, minute: u8) -> LocalDateTime {
        LocalDateTime { year, month, day, hour, minute, second: 0 }
    }

    #[test]
    fn sizes_and_uptime() {
        assert_eq!(fmt_bytes(212 * 1024 * 1024 * 1024), "212 GB");
        assert_eq!(fmt_bytes((8.4 * GIB) as u64), "8.4 GB");
        assert_eq!(fmt_bytes(640 * 1024 * 1024), "640 MB");
        assert_eq!(fmt_bytes((1.8 * 1024.0 * GIB) as u64), "1.8 TB");
        assert_eq!(gb_one_decimal((6.1 * GIB) as u64), "6.1");
        assert_eq!(fmt_bytes((15.8 * GIB) as u64), "16 GB");
        assert_eq!(fmt_uptime(3 * 86_400 + 4 * 3600 + 59), "3d 4h");
        assert_eq!(fmt_uptime(5 * 3600 + 12 * 60), "5h 12m");
        assert_eq!(fmt_uptime(42 * 60 + 5), "42m");
        assert_eq!(pct(0.234), "23%");
    }

    #[test]
    fn boot_labels() {
        let now = dt(2026, 10, 24, 10, 30); // Saturday
        let (full, short) = boot_label(&now, 3600, true).unwrap();
        assert_eq!((full.as_str(), short.as_str()), ("Since 09:30", "Since today"));
        let (full, short) = boot_label(&now, 5 * 86_400, true).unwrap();
        assert_eq!((full.as_str(), short.as_str()), ("Since Mon 10:30", "Since Mon"));
        let (full, _) = boot_label(&now, 30 * 86_400, true).unwrap();
        assert_eq!(full, "Since 24 Sep");
        assert!(boot_label(&LocalDateTime::default(), 10, true).is_none());
    }
}

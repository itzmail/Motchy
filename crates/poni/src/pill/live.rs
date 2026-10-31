//! The other live activities, collapsed: a screen recording, the focus
//! timer, music with the timer, the minutes before a prayer. Each is a
//! 32-DIP capsule sized to what it shows (spec 6.1 and 6.8).
//!
//! The Pill way: content keeps clear of the round ends (16 DIPs for text, 12
//! for a glyph and 6 for album art sitting in the end cap), primary text is
//! white and semibold, labels grey, every number tabular, and colour only
//! where it means something: the phase's colour on the timer, the Windows
//! accent on the prayer countdown. Numbers sit in slots sized for their
//! widest value, so a pill keeps its width while they change.
//!
//! Nothing here moves by itself: the countdowns repaint on the app's
//! existing one-shot wakes (the timer's second, the minute), the readout
//! when a reading arrives.

use crate::activities::{draw_prayer_ok, prayer_countdown, Activity, PRAYER_OK_ROOM};
use crate::gfx::{TextStyle, Weight};
use crate::icons::Icon;
use crate::island::Geom;
use crate::state::AppState;
use crate::ui::Ui;
use poni_core::datetime::fmt_countdown;
use poni_core::geom::{palette, Color, Point, Rect};
use poni_core::pomodoro::Phase;
use poni_core::prayer::Prayer;
use poni_core::svgpath::Seg;
use std::cell::Cell;

/// Every collapsed pill is this tall, a capsule (radius = height / 2).
const H: f32 = 32.0;
/// Content insets from the round ends: a glyph sitting in the end cap,
/// album art sitting in it.
const GLYPH_INSET: f32 = 12.0;
const ART_INSET: f32 = 6.0;
/// Primary content: white at full strength.
const INK: Color = Color::white(1.0);

/// The focus timer (spec 6.8): the ring (14 across) 12 in from the left
/// end, the countdown right-aligned 14 from the right one. Classic's
/// widths, so the morphs between them match; wider once the countdown
/// needs a third minute digit.
const TIMER_W: f32 = 96.0;
const TIMER_LONG_W: f32 = 112.0;
const RING_D: f32 = 14.0;
const RING_STROKE: f32 = 2.5;
const RING_TRACK: Color = Color::white(0.22);
const DIGITS_RIGHT: f32 = 14.0;
/// Music with the timer: the music pill's width and art (spec 6.4), so a
/// timer starting during a song keeps the pill's size and only the right
/// end changes; the ring sits just before its countdown.
const MEDIA_TIMER_W: f32 = 120.0;
const ART: f32 = 20.0;
const ART_R: f32 = 6.0;
const MARK_GAP: f32 = 7.0;
/// The countdown before a prayer: a crescent in the end cap, then "Maghrib
/// 10 min" with the minutes in a two-digit slot (60 at most), then its OK
/// in the other end (`activities::draw_prayer_ok`).
const MOON: f32 = 14.0;
const MOON_GAP: f32 = 8.0;

fn capsule(w: f32) -> Geom {
    Geom::new(w, H, H / 2.0)
}

pub fn geom(a: Activity, st: &AppState) -> Option<Geom> {
    let w = match a {
        // Oyen's head, listening or writing, is the same pill in every theme.
        Activity::Media | Activity::Notes => return None,
        Activity::Timer if long_countdown(st) => TIMER_LONG_W,
        Activity::Timer => TIMER_W,
        Activity::MediaTimer => MEDIA_TIMER_W,
        Activity::Prayer => prayer_w(st),
        Activity::Recording if st.rec_elapsed() >= 3600.0 => TIMER_LONG_W,
        Activity::Recording => TIMER_W,
    };
    Some(capsule(w))
}

pub fn draw(ui: &mut Ui, st: &mut AppState, a: Activity, body: Rect) -> bool {
    match a {
        Activity::Media | Activity::Notes => return false,
        Activity::Timer => draw_timer(ui, st, body),
        Activity::MediaTimer => draw_media_timer(ui, st, body),
        Activity::Prayer => draw_prayer(ui, st, body),
        Activity::Recording => draw_recording(ui, st, body),
    }
    true
}

/// Whether the collapsed `a` needs ambient frames (`None` = as in Classic).
/// Nothing in these pills moves on its own; Classic keeps the frame clock
/// running for music with the timer while the song plays (a leftover from
/// when that pill had an equalizer), which the Pill one doesn't need.
pub fn is_live(a: Activity, _st: &AppState) -> Option<bool> {
    match a {
        Activity::MediaTimer => Some(false),
        _ => None,
    }
}

// ------------------------------------------------------------ fitted widths

// The prayer pill fits what it shows, but `geom` has no canvas
// to measure text with. So each keeps the width it measured when last
// drawn, with what it showed then: its first frame uses an estimate, and
// every later one fits (the island just retargets on the second frame of
// its morph). Only the drawing thread touches these.
thread_local! {
    static PRAYER_FIT: Cell<Option<(Prayer, f32)>> = const { Cell::new(None) };
}

/// Snap a DIP coordinate to the device-pixel grid (crisp 1-DIP dividers
/// and bars at 100 % and 150 %).
fn snap(ui: &Ui, v: f32) -> f32 {
    let s = ui.c.scale().max(0.1);
    (v * s).round() / s
}

// -------------------------------------------------------------------- timer

fn long_countdown(st: &AppState) -> bool {
    // "MM:SS" until the countdown needs another digit.
    fmt_countdown(st.pomodoro.remaining(st.now)).len() > 5
}

fn phase_color(p: Phase) -> Color {
    match p {
        Phase::Focus => palette::ORANGE,
        Phase::ShortBreak | Phase::LongBreak => palette::GREEN,
    }
}

/// The countdown: 15 semibold tabular, white while it runs, grey paused.
fn digits(paused: bool) -> TextStyle {
    TextStyle::headline().tabular().color(if paused { palette::TEXT_DIM } else { INK })
}

/// The timer's mark, centred on `c`: a ring filling in the phase's colour
/// while it runs; paused, two bars in the phase's colour in its place (a
/// 14-DIP ring has no room for a glyph inside, as Classic's 20-DIP one has).
fn timer_mark(ui: &mut Ui, c: Point, progress: f32, col: Color, paused: bool) {
    if paused {
        // Both edges of each bar on whole device pixels (3 DIPs is 4.5 at 150 %).
        let (bw, bh, gap) = (3.0, 10.0, 3.0);
        let (y0, y1) = (snap(ui, c.y - bh / 2.0), snap(ui, c.y + bh / 2.0));
        for x in [c.x - gap / 2.0 - bw, c.x + gap / 2.0] {
            let (x0, x1) = (snap(ui, x), snap(ui, x + bw));
            ui.c.fill_rrect(Rect::new(x0, y0, x1 - x0, y1 - y0), 1.25, col);
        }
    } else {
        ui.c.ring(c, (RING_D - RING_STROKE) / 2.0, RING_STROKE, progress, RING_TRACK, col);
    }
}

fn draw_timer(ui: &mut Ui, st: &AppState, body: Rect) {
    let p = &st.pomodoro;
    let paused = !p.is_running();
    let c = Point::new(body.x + GLYPH_INSET + RING_D / 2.0, body.center().y);
    timer_mark(ui, c, p.progress(st.now), phase_color(p.phase), paused);
    let text = fmt_countdown(p.remaining(st.now));
    let right = body.right() - DIGITS_RIGHT;
    ui.c.text(&text, Rect::new(right - 80.0, body.y, 80.0, body.h), &digits(paused).right());
}

/// A recording, in the timer's place: the red dot in the left end cap (a
/// pause mark while paused, a ring while it starts or saves), the time
/// where the countdown sits.
fn draw_recording(ui: &mut Ui, st: &AppState, body: Rect) {
    use poni_core::recorder::Phase;
    let c = Point::new(body.x + GLYPH_INSET + RING_D / 2.0, body.center().y);
    let phase = st.rec.phase;
    match phase {
        Some(Phase::Recording) => ui.c.fill_circle(c, 5.0, palette::RED),
        Some(Phase::Paused) => ui.c.icon(Icon::Pause, Rect::from_center(c, 11.0, 11.0), palette::TEXT),
        _ => ui.c.stroke_circle(c, 4.4, palette::TEXT_DIM, 1.4),
    }
    let text = poni_core::recorder::fmt_length(st.rec_elapsed());
    let right = body.right() - DIGITS_RIGHT;
    ui.c.text(&text, Rect::new(right - 80.0, body.y, 80.0, body.h), &digits(phase != Some(Phase::Recording)).right());
}

/// Album art in the left end cap (or a note on a grey tile), dimmed while
/// the player is paused, as in the music pill.
fn art(ui: &mut Ui, st: &AppState, r: Rect, playing: bool) {
    let art = st.media.as_ref().and_then(|m| m.art.as_ref());
    ui.c.with_alpha(if playing { 1.0 } else { 0.55 }, |c| match art {
        Some(art) => c.image(art, r, ART_R),
        None => {
            c.fill_rrect(r, ART_R, palette::SURFACE_HOVER);
            c.icon(Icon::Music, r.inset(4.0, 4.0), palette::TEXT_DIM);
        }
    });
}

fn draw_media_timer(ui: &mut Ui, st: &AppState, body: Rect) {
    let cy = body.center().y;
    let playing = st.media.as_ref().is_some_and(|m| m.is_playing());
    art(ui, st, Rect::new(body.x + ART_INSET, cy - ART / 2.0, ART, ART), playing);
    let p = &st.pomodoro;
    let paused = !p.is_running();
    let text = fmt_countdown(p.remaining(st.now));
    let ts = digits(paused);
    // Tabular digits: the same width for every value of the same length,
    // so the ring stays put as the seconds go.
    let (tw, _) = ui.c.measure(&text, &ts, 200.0);
    let right = body.right() - DIGITS_RIGHT;
    ui.c.text(&text, Rect::new(right - 80.0, body.y, 80.0, body.h), &ts.right());
    let c = Point::new(right - tw - MARK_GAP - RING_D / 2.0, cy);
    timer_mark(ui, c, p.progress(st.now), phase_color(p.phase), paused);
}

// ------------------------------------------------------------------- prayer

fn prayer_style(col: Color) -> TextStyle {
    TextStyle::body().weight(Weight::Semibold).tabular().color(col)
}

/// The line's slot: the prayer's name and two digits of minutes.
fn prayer_slot_text(p: Prayer) -> String {
    format!("{} 00 min", p.name())
}

fn prayer_w(st: &AppState) -> f32 {
    let Some((prayer, _)) = prayer_countdown(st) else { return 150.0 };
    if let Some((p, w)) = PRAYER_FIT.with(Cell::get) {
        if p == prayer {
            return w;
        }
    }
    // Before the first frame: about 7 DIPs a character at 13 semibold.
    (GLYPH_INSET + MOON + MOON_GAP + 7.0 * prayer_slot_text(prayer).chars().count() as f32 + PRAYER_OK_ROOM).ceil()
}

/// A filled crescent `d` across centred on `c`: Poni's moon icon, filled
/// (also the Mac's, in the right ear around a notch).
pub(crate) fn moon(ui: &mut Ui, c: Point, d: f32, col: Color) {
    // The icon's outline on its 24-unit grid: a circle of radius 8 round
    // (12, 12) with a radius-7 bite out of its top right.
    let s = d / 16.0;
    let p = |x: f32, y: f32| (c.x + (x - 12.0) * s, c.y + (y - 12.0) * s);
    let (a, b) = (p(19.77, 13.9), p(10.1, 4.23));
    let segs = [
        Seg::MoveTo(a.0, a.1),
        Seg::ArcTo(8.0 * s, 8.0 * s, 0.0, true, true, b.0, b.1),
        Seg::ArcTo(7.0 * s, 7.0 * s, 0.0, false, false, a.0, a.1),
        Seg::Close,
    ];
    ui.c.path(&segs, Some(col), None);
}

fn draw_prayer(ui: &mut Ui, st: &AppState, body: Rect) {
    let Some((prayer, left)) = prayer_countdown(st) else { return };
    let col = st.accent;
    let ts = prayer_style(col);
    let (slot, _) = ui.c.measure(&prayer_slot_text(prayer), &ts, 400.0);
    let w = (GLYPH_INSET + MOON + MOON_GAP + slot + PRAYER_OK_ROOM).ceil();
    PRAYER_FIT.with(|f| f.set(Some((prayer, w))));
    // The pill keeps the two-digit width; the moon and the line centre
    // left of the OK as one group, so "8 min" doesn't leave a gap before
    // it. They shift half a digit once, at "10 min" to "9 min", as the
    // text changes.
    let text = format!("{} {left} min", prayer.name());
    let (tw, _) = ui.c.measure(&text, &ts, 400.0);
    let end = draw_prayer_ok(ui, body);
    let content = MOON + MOON_GAP + tw;
    let x0 = ((body.x + GLYPH_INSET + end) / 2.0 - content / 2.0).max(body.x + GLYPH_INSET);
    moon(ui, Point::new(x0 + MOON / 2.0, body.center().y), MOON, col);
    ui.c.text(&text, Rect::new(x0 + MOON + MOON_GAP, body.y, tw + 2.0, body.h), &ts);
}

#[cfg(test)]
mod tests {
    use super::*;
    use poni_core::pomodoro::RunState;
    use poni_core::settings::{Settings, Theme};
    use poni_core::store::Store;

    fn state() -> AppState {
        let mut st = AppState::new(Settings::default(), Store::default());
        st.settings.theme = Theme::Pill;
        st.now = 1000.0;
        st
    }

    #[test]
    fn every_live_pill_is_a_32_dip_capsule() {
        let st = state();
        for a in [Activity::Timer, Activity::MediaTimer] {
            let g = geom(a, &st).expect("a Pill size");
            assert_eq!((g.h, g.r), (32.0, 16.0), "{a:?}");
        }
        assert!(geom(Activity::Media, &st).is_none(), "the music pill is pill::media's");
    }

    #[test]
    fn the_timer_widens_only_for_a_third_minute_digit() {
        let mut st = state();
        st.pomodoro.run = RunState::Running { ends_at: st.now + 99.0 * 60.0 + 59.0 };
        assert_eq!(geom(Activity::Timer, &st).unwrap().w, TIMER_W, "99:59");
        st.pomodoro.run = RunState::Running { ends_at: st.now + 100.0 * 60.0 };
        assert_eq!(geom(Activity::Timer, &st).unwrap().w, TIMER_LONG_W, "100:00");
        // Music with the timer keeps the music pill's width either way.
        assert_eq!(geom(Activity::MediaTimer, &st).unwrap().w, MEDIA_TIMER_W);
    }

    #[test]
    fn nothing_in_the_live_pills_moves_by_itself() {
        let st = state();
        assert_eq!(is_live(Activity::MediaTimer, &st), Some(false));
        for a in [Activity::Timer, Activity::Prayer] {
            assert_eq!(is_live(a, &st), None, "{a:?}: Classic's answer (no frames) stands");
        }
    }
}

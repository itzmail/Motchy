//! Notes page: quick notes, one line each, kept on this Mac and the same on
//! every Mac that syncs to the person's repository (`poni_core::textnotes`).
//!
//! The first row is the field (Enter adds; with a note picked, Enter saves it
//! and Esc lets go). Click a note to change it, hover for the bin. The
//! island edits one line: a note with more lines (written in the repository's
//! `notes/` folder) shows its first line and can only be deleted here.
//! The footer's Sync now sends and fetches them at once; otherwise they go
//! with the day's sync.

use crate::commands::Command;
use crate::gfx::TextStyle;
use crate::icons::Icon;
use crate::state::AppState;
use crate::ui::{wid, wid_n, PillStyle, Ui, PILL_SMALL};
use poni_core::geom::{palette, Rect};
use poni_core::textnotes::MAX_NOTES;

const HEIGHT: f32 = 200.0;
const INPUT_H: f32 = 34.0;
const ROW_H: f32 = 31.0;
/// List viewport, relative to the page top: exactly four rows.
const LIST_TOP: f32 = 44.0;
const LIST_H: f32 = 4.0 * ROW_H;
const FOOTER_H: f32 = PILL_SMALL;
/// Icon column centre and the text column, from the row's left edge (as Tasks).
const MARK_X: f32 = 18.0;
const TEXT_X: f32 = 36.0;
const WHEEL_STEP: f32 = ROW_H * 1.5;
/// Longest line typed here, in characters.
const MAX_LINE: usize = 500;

pub fn height(_st: &AppState) -> f32 {
    HEIGHT
}

/// Nothing on this page moves by itself.
pub fn next_wake(_st: &AppState) -> Option<f64> {
    None
}

enum RowAction {
    None,
    Edit,
    Delete,
}

pub fn draw(ui: &mut Ui, st: &mut AppState, area: Rect) {
    let area = Rect::new(area.x, area.y, area.w, HEIGHT);
    let accent = st.accent;
    let editing = st.ui.note_edit.clone();

    let input_r = Rect::new(area.x, area.y, area.w, INPUT_H);
    let placeholder = if editing.is_some() { "Editing, Enter to save, Esc to cancel" } else { "Add a note, Enter to save" };
    let icon = if editing.is_some() { Icon::Check } else { Icon::Plus };
    let resp = ui.text_input_row(wid("notes.input"), input_r, &mut st.ui.note_input, placeholder, MAX_LINE, accent, Some(icon));
    if resp.cancelled {
        st.ui.note_edit = None;
        st.ui.note_input.clear();
        ui.input.caret = 0;
    } else if resp.submitted {
        submit(ui, st);
    }

    let view = Rect::new(area.x, area.y + LIST_TOP, area.w, LIST_H);
    let notes: Vec<(String, String, bool)> = st.store.textnotes.live().iter().map(|n| (n.id.clone(), n.title().to_string(), n.one_line())).collect();
    if notes.is_empty() {
        super::empty_state(ui, Rect::new(area.x, view.y, area.w, LIST_H), Icon::File, "No notes yet", "Type one above");
        st.ui.note_scroll = 0.0;
    } else {
        let max_scroll = (notes.len() as f32 * ROW_H - LIST_H).max(0.0);
        if max_scroll > 0.0 {
            st.ui.note_scroll -= ui.wheel_in(view) * WHEEL_STEP;
        }
        st.ui.note_scroll = st.ui.note_scroll.clamp(0.0, max_scroll);
        let mut action = None;
        ui.c.clip(view);
        for (i, (id, title, one_line)) in notes.iter().enumerate() {
            let row = Rect::new(area.x, view.y + i as f32 * ROW_H - st.ui.note_scroll, area.w, ROW_H);
            if row.bottom() <= view.y || row.y >= view.bottom() {
                continue;
            }
            match draw_row(ui, row, view, id, title, *one_line, editing.as_deref() == Some(id.as_str()), accent) {
                RowAction::None => {}
                a => action = Some((id.clone(), a)),
            }
        }
        if st.ui.note_scroll > 0.5 {
            super::tasks::fade(ui, Rect::new(view.x, view.y, view.w, 14.0), true);
        }
        if st.ui.note_scroll < max_scroll - 0.5 {
            super::tasks::fade(ui, Rect::new(view.x, view.bottom() - 14.0, view.w, 14.0), false);
        }
        ui.c.unclip();
        match action {
            Some((id, RowAction::Edit)) => {
                if let Some(n) = st.store.textnotes.notes.iter().find(|n| n.id == id) {
                    st.ui.note_input = n.text.clone();
                    ui.input.caret = n.text.chars().count();
                    ui.input.focused = Some(wid("notes.input"));
                    st.ui.note_edit = Some(id);
                }
            }
            Some((id, RowAction::Delete)) => {
                if st.store.textnotes.delete(&id, st.unix) {
                    if st.ui.note_edit.as_deref() == Some(id.as_str()) {
                        st.ui.note_edit = None;
                        st.ui.note_input.clear();
                        ui.input.caret = 0;
                    }
                    st.save();
                }
            }
            _ => {}
        }
    }
    draw_footer(ui, st, Rect::new(area.x, area.y + HEIGHT - FOOTER_H, area.w, FOOTER_H), notes.len());
}

/// Enter in the field: add the note, or save the one picked.
fn submit(ui: &mut Ui, st: &mut AppState) {
    let text = std::mem::take(&mut st.ui.note_input);
    ui.input.caret = 0;
    match st.ui.note_edit.take() {
        Some(id) => {
            if st.store.textnotes.set_text(&id, &text, st.unix) {
                st.save();
            }
        }
        None => {
            let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
            if st.store.textnotes.add(&text, st.unix, nanos).is_some() {
                st.ui.note_scroll = 0.0;
                st.save();
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_row(ui: &mut Ui, row: Rect, view: Rect, id: &str, title: &str, one_line: bool, picked: bool, accent: poni_core::geom::Color) -> RowAction {
    let mut action = RowAction::None;
    let key = id.bytes().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
    let hit = Rect::new(row.x, row.y.max(view.y), row.w, row.bottom().min(view.bottom()) - row.y.max(view.y));
    let resp = ui.button(wid_n("notes.row", key), hit);
    if picked || resp.pressed {
        ui.c.fill_rrect(row, 9.0, palette::SURFACE_HOVER);
    } else if resp.hovered {
        ui.c.fill_rrect(row, 9.0, palette::SURFACE);
    }
    if resp.clicked && one_line {
        action = RowAction::Edit;
    }
    let cy = row.center().y;
    let mark = if picked { accent } else if resp.hovered { palette::TEXT_DIM } else { palette::TEXT_FAINT };
    ui.c.icon(Icon::File, Rect::from_center(poni_core::geom::Point::new(row.x + MARK_X, cy), 16.0, 16.0), mark);

    let tw = (row.w - TEXT_X - 40.0).max(0.0);
    let shown = if one_line { title.to_string() } else { format!("{title} …") };
    ui.c.text(&shown, Rect::new(row.x + TEXT_X, row.y, tw, row.h), &TextStyle::body().color(palette::TEXT));

    // Delete, only while hovering a fully visible row.
    let del = Rect::new(row.right() - 32.0, cy - 13.0, 26.0, 26.0);
    if resp.hovered && del.y >= view.y && del.bottom() <= view.bottom() {
        let color = if ui.hovered(del) { palette::RED } else { palette::TEXT_DIM };
        if ui.icon_button(wid_n("notes.del", key), del, Icon::Trash, color, 14.0).clicked {
            action = RowAction::Delete;
        }
    }
    action
}

/// How many notes there are and where syncing stands, with Sync now.
fn draw_footer(ui: &mut Ui, st: &mut AppState, r: Rect, count: usize) {
    let linked = st.settings.sync.repo_url.is_some();
    let mut right = r.right();
    if linked && !st.sync.running {
        let w = ui.pill_width("Sync now", false, true);
        let pr = Rect::new(r.right() - w, r.y, w, r.h);
        right = pr.x - 12.0;
        if ui.pill(wid("notes.sync"), pr, "Sync now", None, PillStyle::Utility).clicked {
            st.cmd(Command::SyncNow);
        }
    }
    let (text, color) = if st.sync.running {
        ("Syncing…".to_string(), palette::TEXT_DIM)
    } else if !st.sync.error.is_empty() {
        (st.sync.error.clone(), palette::ORANGE)
    } else if count >= MAX_NOTES {
        (format!("Notes are full ({MAX_NOTES})"), palette::ORANGE)
    } else if !linked {
        ("Kept on this Mac. Link a repository in Settings to sync".to_string(), palette::TEXT_DIM)
    } else {
        (format!("{count} {}", if count == 1 { "note" } else { "notes" }), palette::TEXT_DIM)
    };
    ui.c.text(&text, Rect::new(r.x + TEXT_X, r.y, (right - r.x - TEXT_X).max(0.0), r.h), &TextStyle::footnote().color(color));
}

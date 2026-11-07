//! Notes page: notes kept on this Mac and the same on every Mac that syncs to
//! the person's repository (`poni_core::textnotes`), laid out like Notes.app:
//! the list on the left (first line as the title, the next as a hint), the
//! note open on the right.
//!
//! The editor is the Mac's own text view, laid over the island by the app
//! (`mac/notes_editor.rs`) where this page leaves a box for it
//! (`st.ui.note_editor`); the page draws the box and everything around it.
//! Without a native editor (the pictures `--render` takes) the page draws
//! the open note's text instead. Typing goes to the note as it happens; a
//! note left blank is deleted when another is opened or the page is left.
//! The footer's Sync now sends and fetches at once; otherwise notes go with
//! the day's sync.

use crate::commands::Command;
use crate::gfx::TextStyle;
use crate::icons::Icon;
use crate::state::AppState;
use crate::ui::{wid, wid_n, PillStyle, Ui, PILL_SMALL};
use poni_core::geom::{palette, Rect};
use poni_core::textnotes::MAX_NOTES;

/// Taller than the other pages: a note needs room to be written in.
const HEIGHT: f32 = 290.0;
const LIST_W: f32 = 176.0;
const GAP: f32 = 10.0;
const TOP_H: f32 = 30.0;
const ROW_H: f32 = 46.0;
const FOOTER_H: f32 = PILL_SMALL;
const WHEEL_STEP: f32 = ROW_H;
/// Space round the editor's text inside its box.
const EDITOR_INSET: f32 = 3.0;

pub fn height(_st: &AppState) -> f32 {
    HEIGHT
}

/// Nothing on this page moves by itself.
pub fn next_wake(_st: &AppState) -> Option<f64> {
    None
}

pub fn draw(ui: &mut Ui, st: &mut AppState, area: Rect) {
    let area = Rect::new(area.x, area.y, area.w, HEIGHT);
    let accent = st.accent;
    st.ui.note_editor = None;

    // The open note must be one that exists: opening the page, or after a
    // delete, blank notes left over go and the newest opens.
    if !st.ui.note_open.as_ref().is_some_and(|id| st.store.textnotes.live().iter().any(|n| &n.id == id)) {
        if st.store.textnotes.prune_empty(None, st.unix) {
            st.save();
        }
        st.ui.note_open = st.store.textnotes.live().first().map(|n| n.id.clone());
    }
    // Rows to show, newest change first.
    let rows: Vec<(String, String, String)> = st.store.textnotes.live().iter().map(|n| (n.id.clone(), n.title().to_string(), n.snippet().to_string())).collect();

    let left = Rect::new(area.x, area.y, LIST_W, HEIGHT);
    let right = Rect::new(area.x + LIST_W + GAP, area.y, (area.w - LIST_W - GAP).max(0.0), HEIGHT - FOOTER_H - 8.0);

    // Header of the list: its name and the button that starts a note.
    ui.c.text("Notes", Rect::new(left.x + 6.0, left.y, left.w - 40.0, TOP_H), &TextStyle::headline().color(palette::TEXT));
    let full = st.store.textnotes.live().len() >= MAX_NOTES;
    let plus = Rect::new(left.right() - 28.0, left.y + 2.0, 26.0, 26.0);
    if !full && ui.icon_button(wid("notes.new"), plus, Icon::Plus, accent, 16.0).clicked {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos());
        if let Some(id) = st.store.textnotes.create(st.unix, nanos) {
            st.store.textnotes.prune_empty(Some(&id), st.unix);
            st.ui.note_open = Some(id);
            st.ui.note_scroll = 0.0;
            st.ui.note_focus = true;
            st.save();
        }
    }

    let view = Rect::new(left.x, left.y + TOP_H + 4.0, left.w, HEIGHT - TOP_H - 4.0 - FOOTER_H - 8.0);
    let mut pick = None;
    if rows.is_empty() {
        st.ui.note_scroll = 0.0;
        super::empty_state(ui, view, Icon::File, "No notes yet", "Tap + to write one");
    } else {
        let max_scroll = (rows.len() as f32 * ROW_H - view.h).max(0.0);
        if max_scroll > 0.0 {
            st.ui.note_scroll -= ui.wheel_in(view) * WHEEL_STEP;
        }
        st.ui.note_scroll = st.ui.note_scroll.clamp(0.0, max_scroll);
        ui.c.clip(view);
        for (i, (id, title, snippet)) in rows.iter().enumerate() {
            let row = Rect::new(view.x, view.y + i as f32 * ROW_H - st.ui.note_scroll, view.w, ROW_H - 2.0);
            if row.bottom() <= view.y || row.y >= view.bottom() {
                continue;
            }
            let picked = st.ui.note_open.as_deref() == Some(id.as_str());
            if draw_row(ui, row, view, id, title, snippet, picked) {
                pick = Some(id.clone());
            }
        }
        if st.ui.note_scroll > 0.5 {
            super::tasks::fade(ui, Rect::new(view.x, view.y, view.w, 14.0), true);
        }
        if st.ui.note_scroll < max_scroll - 0.5 {
            super::tasks::fade(ui, Rect::new(view.x, view.bottom() - 14.0, view.w, 14.0), false);
        }
        ui.c.unclip();
    }
    if let Some(id) = pick {
        if st.ui.note_open.as_deref() != Some(id.as_str()) {
            if st.store.textnotes.prune_empty(Some(&id), st.unix) {
                st.save();
            }
            st.ui.note_open = Some(id);
        }
    }

    // The note open: a box for the editor.
    ui.c.fill_rrect(right, 10.0, palette::SURFACE);
    if let Some(note) = st.ui.note_open.as_ref().and_then(|id| st.store.textnotes.notes.iter().find(|n| &n.id == id && !n.deleted)) {
        let inner = right.inset(EDITOR_INSET, EDITOR_INSET);
        if st.ui.note_native {
            st.ui.note_editor = Some(inner);
        } else {
            let style = TextStyle::body().color(palette::TEXT).wrap();
            ui.c.text(&note.text, inner.inset(8.0, 8.0), &style);
        }
    } else if !rows.is_empty() {
        super::empty_state(ui, right, Icon::File, "Pick a note", "");
    }
    draw_footer(ui, st, Rect::new(area.x, area.y + HEIGHT - FOOTER_H, area.w, FOOTER_H), rows.len());
}

/// One note in the list: the first line, and the next as a hint. True when clicked.
fn draw_row(ui: &mut Ui, row: Rect, view: Rect, id: &str, title: &str, snippet: &str, picked: bool) -> bool {
    let key = id.bytes().fold(0u64, |h, b| h.wrapping_mul(31).wrapping_add(u64::from(b)));
    let hit = Rect::new(row.x, row.y.max(view.y), row.w, row.bottom().min(view.bottom()) - row.y.max(view.y));
    let resp = ui.button(wid_n("notes.row", key), hit);
    if picked || resp.pressed {
        ui.c.fill_rrect(row, 9.0, palette::SURFACE_HOVER);
    } else if resp.hovered {
        ui.c.fill_rrect(row, 9.0, palette::SURFACE);
    }
    let (shown, color) = if title.is_empty() { ("New note", palette::TEXT_FAINT) } else { (title, palette::TEXT) };
    let tw = (row.w - 20.0).max(0.0);
    let top = if snippet.is_empty() { 12.0 } else { 5.0 };
    ui.c.text(shown, Rect::new(row.x + 10.0, row.y + top, tw, 20.0), &TextStyle::body().color(color));
    if !snippet.is_empty() {
        ui.c.text(snippet, Rect::new(row.x + 10.0, row.y + 24.0, tw, 16.0), &TextStyle::footnote().color(palette::TEXT_DIM));
    }
    resp.clicked
}

/// How many notes there are and where syncing stands; the bin for the open
/// note and Sync now.
fn draw_footer(ui: &mut Ui, st: &mut AppState, r: Rect, count: usize) {
    let linked = st.settings.sync.repo_url.is_some();
    let mut right = r.right();
    if linked && !st.sync.running {
        let w = ui.pill_width("Sync now", false, true);
        let pr = Rect::new(r.right() - w, r.y, w, r.h);
        right = pr.x - 8.0;
        if ui.pill(wid("notes.sync"), pr, "Sync now", None, PillStyle::Utility).clicked {
            st.cmd(Command::SyncNow);
        }
    }
    if let Some(id) = st.ui.note_open.clone() {
        let del = Rect::new(right - 26.0, r.center().y - 13.0, 26.0, 26.0);
        right = del.x - 4.0;
        let color = if ui.hovered(del) { palette::RED } else { palette::TEXT_DIM };
        if ui.icon_button(wid("notes.del"), del, Icon::Trash, color, 14.0).clicked && st.store.textnotes.delete(&id, st.unix) {
            st.ui.note_open = None;
            st.save();
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
    ui.c.text(&text, Rect::new(r.x + 6.0, r.y, (right - r.x - 6.0).max(0.0), r.h), &TextStyle::footnote().color(color));
}

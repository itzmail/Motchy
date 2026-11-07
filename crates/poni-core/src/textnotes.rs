//! The Notes page's quick notes: text kept on this Mac and the same on every
//! Mac that syncs to the person's repository (`crate::gitsync`), one
//! Markdown file per note in its `notes/` folder.
//!
//! Not the meeting notes (`crate::notes`), which are written by the Oyenza
//! account from a recording.
//!
//! A deleted note stays as a tombstone (a file with `deleted: true` and no
//! text), so another Mac that still has it learns it is gone rather than
//! sending it back.

use serde::{Deserialize, Serialize};

pub const MAX_NOTES: usize = 500;
/// Longest text of one note, in characters.
pub const MAX_LEN: usize = 20_000;
/// Longest id a file name may carry.
const MAX_ID: usize = 40;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    pub text: String,
    /// Unix seconds of the last change; the newer side wins.
    pub modified: i64,
    /// Deleted: only the tombstone is kept.
    #[serde(default)]
    pub deleted: bool,
}

impl Note {
    /// The first line that has anything on it.
    pub fn title(&self) -> &str {
        self.text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("")
    }

    /// The second line that has anything on it (the list's second row).
    pub fn snippet(&self) -> &str {
        self.text.lines().map(str::trim).filter(|l| !l.is_empty()).nth(1).unwrap_or("")
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NoteList {
    pub notes: Vec<Note>,
}

impl NoteList {
    /// The notes to show, the newest change first.
    pub fn live(&self) -> Vec<&Note> {
        let mut v: Vec<&Note> = self.notes.iter().filter(|n| !n.deleted).collect();
        v.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.id.cmp(&b.id)));
        v
    }

    /// Add a note; returns its id, or `None` for blank text or a full list.
    pub fn add(&mut self, text: &str, now: i64, nanos: u128) -> Option<String> {
        let text = clean(text);
        if text.is_empty() || self.notes.iter().filter(|n| !n.deleted).count() >= MAX_NOTES {
            return None;
        }
        let id = new_id(nanos);
        self.notes.push(Note { id: id.clone(), text, modified: now, deleted: false });
        Some(id)
    }

    /// Change a note's text. False when nothing changed (no such note, the
    /// same text, or blank text, which deleting is for).
    pub fn set_text(&mut self, id: &str, text: &str, now: i64) -> bool {
        let text = clean(text);
        match self.notes.iter_mut().find(|n| n.id == id && !n.deleted) {
            Some(n) if !text.is_empty() && n.text != text => {
                n.text = text;
                n.modified = now;
                true
            }
            _ => false,
        }
    }

    /// Start an empty note (the editor fills it); `None` when the list is full.
    pub fn create(&mut self, now: i64, nanos: u128) -> Option<String> {
        if self.notes.iter().filter(|n| !n.deleted).count() >= MAX_NOTES {
            return None;
        }
        let id = new_id(nanos);
        self.notes.push(Note { id: id.clone(), text: String::new(), modified: now, deleted: false });
        Some(id)
    }

    /// Keep what the editor holds, blank included (a note being written).
    /// False when nothing changed.
    pub fn write(&mut self, id: &str, text: &str, now: i64) -> bool {
        let text: String = text.chars().take(MAX_LEN).collect();
        match self.notes.iter_mut().find(|n| n.id == id && !n.deleted) {
            Some(n) if n.text != text => {
                n.text = text;
                n.modified = now;
                true
            }
            _ => false,
        }
    }

    /// Delete the notes that stayed blank, except `keep`. True when any went.
    pub fn prune_empty(&mut self, keep: Option<&str>, now: i64) -> bool {
        let blank: Vec<String> = self.notes.iter().filter(|n| !n.deleted && n.text.trim().is_empty() && Some(n.id.as_str()) != keep).map(|n| n.id.clone()).collect();
        for id in &blank {
            self.delete(id, now);
        }
        !blank.is_empty()
    }

    pub fn delete(&mut self, id: &str, now: i64) -> bool {
        match self.notes.iter_mut().find(|n| n.id == id && !n.deleted) {
            Some(n) => {
                n.text.clear();
                n.deleted = true;
                n.modified = now;
                true
            }
            None => false,
        }
    }

    /// Take on what a sync found: the newer side of each note wins.
    pub fn absorb(&mut self, other: &[Note]) {
        self.notes = merge(&self.notes, other);
    }
}

/// A note's text as kept: trailing space and blank edges off, cut to [`MAX_LEN`].
pub fn clean(text: &str) -> String {
    text.trim().chars().take(MAX_LEN).collect()
}

/// The notes of two sides as one set: each id once, with the newer change
/// (a deletion wins a tie). Sorted by id, so equal sets come out equal.
pub fn merge(a: &[Note], b: &[Note]) -> Vec<Note> {
    let mut out: Vec<Note> = Vec::with_capacity(a.len().max(b.len()));
    for n in a.iter().chain(b) {
        match out.iter_mut().find(|o| o.id == n.id) {
            Some(o) => {
                if n.modified > o.modified || (n.modified == o.modified && n.deleted && !o.deleted) {
                    *o = n.clone();
                }
            }
            None => out.push(n.clone()),
        }
    }
    out.sort_by(|x, y| x.id.cmp(&y.id));
    out
}

/// An id a file name may carry: letters, digits and dashes, not too long.
/// (Names come from the repository, so they are never trusted as paths.)
pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// A new id from the clock in nanoseconds: Macs do not make one in the same
/// instant.
pub fn new_id(nanos: u128) -> String {
    format!("{nanos:x}")
}

/// The note as its file: a small header, then the text.
pub fn to_file(n: &Note) -> String {
    let mut s = format!("---\nmodified: {}\n", n.modified);
    if n.deleted {
        s.push_str("deleted: true\n");
    }
    s.push_str("---\n");
    s.push_str(&n.text);
    s.push('\n');
    s
}

/// A note from its file. A file without the header (written by hand in the
/// repository) is a note changed at `fallback`, the file's own time.
pub fn from_file(id: &str, file: &str, fallback: i64) -> Option<Note> {
    if !valid_id(id) {
        return None;
    }
    let file = file.replace("\r\n", "\n");
    let (mut modified, mut deleted, mut text) = (fallback, false, file.as_str());
    if let Some(rest) = file.strip_prefix("---\n") {
        if let Some((head, body)) = rest.split_once("\n---\n").or_else(|| rest.strip_suffix("\n---").map(|h| (h, ""))) {
            for line in head.lines() {
                match line.split_once(':').map(|(k, v)| (k.trim(), v.trim())) {
                    Some(("modified", v)) => modified = v.parse().unwrap_or(fallback),
                    Some(("deleted", v)) => deleted = v == "true",
                    _ => {}
                }
            }
            text = body;
        }
    }
    let text = if deleted { String::new() } else { clean(text) };
    Some(Note { id: id.into(), text, modified, deleted })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: &str, text: &str, modified: i64) -> Note {
        Note { id: id.into(), text: text.into(), modified, deleted: false }
    }

    #[test]
    fn add_edit_delete() {
        let mut l = NoteList::default();
        assert_eq!(l.add("   ", 1, 1), None);
        let id = l.add("  call the bank ", 10, 0xabc).unwrap();
        assert_eq!(l.notes[0].text, "call the bank");
        assert!(!l.set_text(&id, "call the bank", 20), "same text changes nothing");
        assert!(!l.set_text(&id, "  ", 20), "blank is for deleting");
        assert!(l.set_text(&id, "call the bank at 3", 30));
        assert_eq!(l.notes[0].modified, 30);
        assert!(l.delete(&id, 40));
        assert!(!l.delete(&id, 50));
        assert!(l.live().is_empty());
        assert_eq!(l.notes.len(), 1, "the tombstone stays");
    }

    #[test]
    fn a_note_is_written_while_it_is_typed_and_blank_ones_go() {
        let mut l = NoteList::default();
        let a = l.create(1, 0xa).unwrap();
        let b = l.create(2, 0xb).unwrap();
        assert!(l.write(&a, "Shopping\n- milk ", 5), "trailing space is kept while typing");
        assert_eq!(l.notes[0].text, "Shopping\n- milk ");
        assert!(!l.write(&a, "Shopping\n- milk ", 6));
        assert_eq!(l.live().len(), 2);
        // The open note stays; any other blank one is deleted.
        assert!(!l.prune_empty(Some(&b), 7));
        assert!(l.prune_empty(None, 8));
        assert_eq!(l.live().iter().map(|n| n.id.as_str()).collect::<Vec<_>>(), [a.as_str()]);
        assert!(l.write(&a, "   ", 9), "text can be wiped to blank");
        assert!(l.prune_empty(None, 10));
        assert!(l.live().is_empty());
    }

    #[test]
    fn newest_first() {
        let l = NoteList { notes: vec![note("a", "old", 1), note("b", "new", 9), note("c", "mid", 5)] };
        let ids: Vec<&str> = l.live().iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, ["b", "c", "a"]);
    }

    #[test]
    fn merge_takes_the_newer_and_keeps_both_sides_notes() {
        let a = vec![note("1", "mine", 10), note("2", "only here", 5)];
        let b = vec![note("1", "theirs", 20), note("3", "only there", 7)];
        let m = merge(&a, &b);
        assert_eq!(m.iter().map(|n| (n.id.as_str(), n.text.as_str())).collect::<Vec<_>>(), [("1", "theirs"), ("2", "only here"), ("3", "only there")]);
        assert_eq!(merge(&b, &a), m, "the order of the sides doesn't matter");
    }

    #[test]
    fn a_deletion_wins_a_tie_and_a_newer_edit_beats_a_deletion() {
        let gone = Note { deleted: true, text: String::new(), ..note("1", "", 10) };
        assert!(merge(&[note("1", "x", 10)], &[gone.clone()])[0].deleted);
        assert!(merge(&[gone.clone()], &[note("1", "x", 10)])[0].deleted);
        assert!(!merge(&[gone], &[note("1", "edited later", 11)])[0].deleted);
    }

    #[test]
    fn files_round_trip() {
        let n = note("abc-1", "first line\nsecond line", 1700000000);
        assert_eq!(from_file("abc-1", &to_file(&n), 0), Some(n));
        let gone = Note { deleted: true, text: String::new(), ..note("abc-2", "", 5) };
        assert_eq!(from_file("abc-2", &to_file(&gone), 0), Some(gone));
    }

    #[test]
    fn a_hand_written_file_takes_the_files_time() {
        let n = from_file("todo", "# Shopping\n\n- milk\n", 777).unwrap();
        assert_eq!((n.modified, n.deleted), (777, false));
        assert_eq!(n.text, "# Shopping\n\n- milk");
        assert_eq!(n.title(), "# Shopping");
        assert_eq!(n.snippet(), "- milk");
        // CRLF files from another editor read the same.
        assert_eq!(from_file("w", "---\r\nmodified: 5\r\n---\r\nhi\r\n", 0).unwrap(), note("w", "hi", 5));
    }

    #[test]
    fn ids_from_the_repository_are_never_paths() {
        assert!(valid_id("17a3-bc"));
        assert!(!valid_id(""));
        assert!(!valid_id("../x"));
        assert!(!valid_id("a/b"));
        assert!(!valid_id(".hidden"));
        assert!(!valid_id(&"a".repeat(41)));
        assert_eq!(from_file("../x", "hi", 0), None);
    }

    #[test]
    fn long_text_is_cut() {
        let n = from_file("a", &"x".repeat(MAX_LEN + 50), 0).unwrap();
        assert_eq!(n.text.chars().count(), MAX_LEN);
    }
}

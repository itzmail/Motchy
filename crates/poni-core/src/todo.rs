//! A small personal task list.

use serde::{Deserialize, Serialize};

pub const MAX_TASK_LEN: usize = 200;
pub const MAX_TASKS: usize = 200;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: u64,
    pub text: String,
    pub done: bool,
    pub created_unix: i64,
    #[serde(default)]
    pub done_unix: Option<i64>,
    /// When the text or the done mark last changed (Unix seconds; 0 = before
    /// this was kept). The newer side wins when Reminders and the list differ.
    #[serde(default)]
    pub modified_unix: i64,
    /// The task's reminder in the Mac's Reminders, once synced.
    #[serde(default)]
    pub reminder_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TaskList {
    pub tasks: Vec<Task>,
    pub next_id: u64,
    /// The reminders that were tied to a task when the last sync ended. A
    /// reminder listed here that no task holds was deleted here; a task
    /// whose reminder is listed but gone from Reminders was deleted there.
    pub linked: Vec<String>,
}

/// A reminder of the synced Reminders list, as the sync reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct Remote {
    pub id: String,
    pub text: String,
    pub done: bool,
    /// Its last change (Unix seconds; 0 = unknown).
    pub modified: i64,
}

/// What the sync leaves for the caller to do in Reminders.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Plan {
    /// Tasks (by id) that have no reminder yet: make one, then [`TaskList::link`] it.
    pub create: Vec<u64>,
    /// Reminders to bring to a task's text and done mark: (reminder id, text, done).
    pub update: Vec<(String, String, bool)>,
    /// Reminders whose task was deleted here.
    pub delete: Vec<String>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.create.is_empty() && self.update.is_empty() && self.delete.is_empty()
    }
}

/// Task text as the list keeps it: one line, spaces squeezed, cut to [`MAX_TASK_LEN`].
pub fn clean(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(MAX_TASK_LEN).collect()
}

impl Default for TaskList {
    fn default() -> Self {
        Self { tasks: Vec::new(), next_id: 1, linked: Vec::new() }
    }
}

impl TaskList {
    /// Add a task; returns its id, or `None` for blank text / a full list.
    pub fn add(&mut self, text: &str, now_unix: i64) -> Option<u64> {
        let text = clean(text);
        if text.is_empty() || self.tasks.len() >= MAX_TASKS {
            return None;
        }
        let id = self.next_id.max(1);
        self.next_id = id + 1;
        self.tasks.push(Task { id, text, done: false, created_unix: now_unix, done_unix: None, modified_unix: now_unix, reminder_id: None });
        Some(id)
    }

    pub fn toggle(&mut self, id: u64, now_unix: i64) -> bool {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
            t.done = !t.done;
            t.done_unix = if t.done { Some(now_unix) } else { None };
            t.modified_unix = now_unix;
            true
        } else {
            false
        }
    }

    pub fn remove(&mut self, id: u64) -> bool {
        let before = self.tasks.len();
        self.tasks.retain(|t| t.id != id);
        self.tasks.len() != before
    }

    pub fn clear_done(&mut self) -> usize {
        let before = self.tasks.len();
        self.tasks.retain(|t| !t.done);
        before - self.tasks.len()
    }

    pub fn open_count(&self) -> usize {
        self.tasks.iter().filter(|t| !t.done).count()
    }

    /// Tasks completed at or after `day_start_unix`.
    pub fn done_since(&self, day_start_unix: i64) -> usize {
        self.tasks.iter().filter(|t| t.done && t.done_unix.is_some_and(|d| d >= day_start_unix)).count()
    }

    /// "done today / (open + done today)" — e.g. 1/3.
    pub fn today_progress(&self, day_start_unix: i64) -> (usize, usize) {
        let done = self.done_since(day_start_unix);
        (done, done + self.open_count())
    }

    /// Tie a task to the reminder just made for it.
    pub fn link(&mut self, id: u64, reminder_id: String) {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.id == id) {
            if !self.linked.contains(&reminder_id) {
                self.linked.push(reminder_id.clone());
            }
            t.reminder_id = Some(reminder_id);
        }
    }

    /// Bring the list and the synced Reminders list together: reminders that
    /// are new here become tasks, tasks gone from Reminders go too, the
    /// newer side of a changed task wins, and what the caller still has to
    /// do in Reminders comes back as the [`Plan`]. Only call this with a
    /// complete read of that list: an empty read that was really a failure
    /// would delete every task.
    pub fn merge(&mut self, remote: &[Remote], now_unix: i64) -> Plan {
        let mut plan = Plan::default();
        let was_linked = std::mem::take(&mut self.linked);
        let known = |id: &str| was_linked.iter().any(|k| k == id);

        // Reminders: new, changed or deleted here.
        for r in remote {
            match self.tasks.iter_mut().find(|t| t.reminder_id.as_deref() == Some(r.id.as_str())) {
                Some(t) => {
                    let (text, done) = (clean(&r.text), r.done);
                    if (t.text == text && t.done == done) || text.is_empty() {
                        continue;
                    }
                    if r.modified >= t.modified_unix {
                        let at = if r.modified > 0 { r.modified } else { now_unix };
                        if t.done != done {
                            t.done_unix = done.then_some(at);
                        }
                        (t.text, t.done, t.modified_unix) = (text, done, at);
                    } else {
                        plan.update.push((r.id.clone(), t.text.clone(), t.done));
                    }
                }
                None if known(&r.id) => plan.delete.push(r.id.clone()),
                None => {
                    let text = clean(&r.text);
                    if text.is_empty() || self.tasks.len() >= MAX_TASKS {
                        continue;
                    }
                    let id = self.next_id.max(1);
                    self.next_id = id + 1;
                    let at = if r.modified > 0 { r.modified } else { now_unix };
                    self.tasks.push(Task { id, text, done: r.done, created_unix: at, done_unix: r.done.then_some(at), modified_unix: at, reminder_id: Some(r.id.clone()) });
                }
            }
        }

        // Tasks: deleted in Reminders, or not there yet.
        self.tasks.retain_mut(|t| {
            let Some(rid) = t.reminder_id.clone() else { return true };
            if remote.iter().any(|r| r.id == rid) {
                return true;
            }
            if known(&rid) {
                return false;
            }
            // Its reminder was already deleted (the task came back with an undo): a new one.
            t.reminder_id = None;
            true
        });
        plan.create = self.tasks.iter().filter(|t| t.reminder_id.is_none()).map(|t| t.id).collect();
        self.linked = self.tasks.iter().filter_map(|t| t.reminder_id.clone()).collect();
        plan
    }

    /// Display order: open tasks first (oldest first), then today's done tasks.
    pub fn display_order(&self, day_start_unix: i64) -> Vec<&Task> {
        let mut open: Vec<&Task> = self.tasks.iter().filter(|t| !t.done).collect();
        let mut done: Vec<&Task> = self
            .tasks
            .iter()
            .filter(|t| t.done && t.done_unix.is_some_and(|d| d >= day_start_unix))
            .collect();
        open.sort_by_key(|t| (t.created_unix, t.id));
        done.sort_by_key(|t| std::cmp::Reverse(t.done_unix));
        open.extend(done);
        open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_toggle_remove() {
        let mut l = TaskList::default();
        assert_eq!(l.add("   ", 0), None);
        let a = l.add("  buy   milk ", 10).unwrap();
        let b = l.add("ship poni", 20).unwrap();
        assert_eq!(l.tasks[0].text, "buy milk");
        assert!(l.toggle(a, 100));
        assert_eq!(l.today_progress(50), (1, 2));
        assert_eq!(l.today_progress(200), (0, 1));
        let order: Vec<u64> = l.display_order(50).iter().map(|t| t.id).collect();
        assert_eq!(order, vec![b, a]);
        assert_eq!(l.clear_done(), 1);
        assert!(l.remove(b));
        assert!(!l.remove(b));
    }

    #[test]
    fn truncates_long_text() {
        let mut l = TaskList::default();
        let id = l.add(&"x".repeat(500), 0).unwrap();
        assert_eq!(l.tasks.iter().find(|t| t.id == id).unwrap().text.len(), MAX_TASK_LEN);
    }

    #[test]
    fn ids_survive_default_zero() {
        let mut l = TaskList { tasks: vec![], next_id: 0, linked: vec![] };
        assert_eq!(l.add("a", 0), Some(1));
        assert_eq!(l.add("b", 0), Some(2));
    }

    fn remote(id: &str, text: &str, done: bool, modified: i64) -> Remote {
        Remote { id: id.into(), text: text.into(), done, modified }
    }

    /// A list with one task, tied to reminder "r1" as a finished sync leaves it.
    fn synced() -> (TaskList, u64) {
        let mut l = TaskList::default();
        let id = l.add("milk", 10).unwrap();
        l.link(id, "r1".into());
        (l, id)
    }

    #[test]
    fn first_sync_swaps_both_ways() {
        let mut l = TaskList::default();
        let a = l.add("local", 10).unwrap();
        let plan = l.merge(&[remote("r9", "from reminders", true, 50)], 100);
        assert_eq!(plan.create, vec![a]);
        assert!(plan.update.is_empty() && plan.delete.is_empty());
        let added = l.tasks.iter().find(|t| t.reminder_id.as_deref() == Some("r9")).unwrap();
        assert!(added.done && added.done_unix == Some(50) && added.text == "from reminders");
        // The caller makes the reminder for `local`; the next sync leaves both alone.
        l.link(a, "r1".into());
        let plan = l.merge(&[remote("r9", "from reminders", true, 50), remote("r1", "local", false, 100)], 200);
        assert!(plan.is_empty());
        assert_eq!(l.tasks.len(), 2);
    }

    #[test]
    fn newer_side_wins() {
        let (mut l, id) = synced();
        // Done in Reminders later than the list's last change.
        let plan = l.merge(&[remote("r1", "milk", true, 500)], 600);
        assert!(plan.is_empty());
        let t = &l.tasks[0];
        assert!(t.done && t.done_unix == Some(500) && t.modified_unix == 500);
        // Then un-done here, later: Reminders is told.
        assert!(l.toggle(id, 700));
        let plan = l.merge(&[remote("r1", "milk", true, 500)], 710);
        assert_eq!(plan.update, vec![("r1".to_string(), "milk".to_string(), false)]);
        assert!(!l.tasks[0].done);
    }

    #[test]
    fn deleted_in_reminders_goes_here() {
        let (mut l, _) = synced();
        let plan = l.merge(&[], 100);
        assert!(plan.is_empty() && l.tasks.is_empty() && l.linked.is_empty());
    }

    #[test]
    fn deleted_here_goes_from_reminders() {
        let (mut l, id) = synced();
        assert!(l.remove(id));
        let plan = l.merge(&[remote("r1", "milk", false, 10)], 100);
        assert_eq!(plan.delete, vec!["r1".to_string()]);
        assert!(l.tasks.is_empty());
        // A reminder still listed after a failed delete comes back as a task, not a loop.
        let plan = l.merge(&[remote("r1", "milk", false, 10)], 200);
        assert!(plan.is_empty() && l.tasks.len() == 1);
    }

    #[test]
    fn undo_after_the_reminder_went_makes_a_new_one() {
        let (mut l, id) = synced();
        let task = l.tasks[0].clone();
        l.remove(id);
        l.merge(&[remote("r1", "milk", false, 10)], 100);
        l.tasks.push(task);
        let plan = l.merge(&[], 200);
        assert_eq!(plan.create, vec![id]);
        assert_eq!(l.tasks[0].reminder_id, None);
    }

    #[test]
    fn a_full_list_takes_no_more_and_texts_are_cleaned() {
        let mut l = TaskList::default();
        for n in 0..MAX_TASKS {
            l.add(&format!("t{n}"), 1);
        }
        l.merge(&[remote("r1", "one more", false, 5)], 10);
        assert_eq!(l.tasks.len(), MAX_TASKS);
        let (mut l, _) = synced();
        let plan = l.merge(&[remote("r1", "  milk  ", false, 99)], 100);
        assert!(plan.is_empty() && l.tasks[0].modified_unix == 10);
    }
}

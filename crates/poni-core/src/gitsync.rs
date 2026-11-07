//! Sync to the person's own git repository: the rules, with no I/O.
//!
//! The repository is theirs (any host: it is only `git`), private, and
//! separate from this app's code. It holds `config/settings.json` (the
//! choices that are the same on every Mac) and, later, one folder of
//! activity per machine, so two Macs never write the same file.
//!
//! One sync a day: the app remembers the day it last synced ([`Memory`],
//! kept on this Mac only) and a day that differs starts the next one. The
//! rest of the day nothing runs.

use crate::settings::Settings;
use crate::sha256::{hex, sha256};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Where the shared settings sit in the repository.
pub const CONFIG_FILE: &str = "config/settings.json";

/// What this Mac remembers between syncs. Never synced itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Memory {
    /// The day the last sync finished ("2026-11-06", the Mac's own date).
    pub last_day: String,
    /// Fingerprint of the shared settings as of that sync ([`fingerprint`]).
    pub synced: String,
    /// Why the last attempt failed; empty after a good one.
    pub error: String,
}

/// Is a sync due on `today`? A different day from the last good one.
pub fn due(m: &Memory, today: &str) -> bool {
    m.last_day != today
}

/// The settings file as the repository keeps it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConfigFile {
    /// When this version was written (Unix seconds): the newer one wins.
    pub updated_at: u64,
    /// Which Mac wrote it, for the person reading the history.
    pub machine: String,
    pub settings: Value,
}

/// Choices that belong to one Mac and are never shared: where its island
/// sits and which display, where it is for Prayer times, the repository
/// itself, whether it listens for the editors and whether Tasks follows
/// Reminders (each Mac decides).
fn strip(v: &mut Value) {
    if let Some(o) = v.as_object_mut() {
        for k in ["island_x", "monitor", "sync", "coding", "reminders_sync"] {
            o.remove(k);
        }
        if let Some(p) = o.get_mut("prayer").and_then(Value::as_object_mut) {
            for k in ["place", "world_place"] {
                p.remove(k);
            }
        }
    }
}

/// The settings that are the same on every Mac, as JSON.
pub fn shared(s: &Settings) -> Value {
    let mut v = serde_json::to_value(s).unwrap_or(Value::Null);
    strip(&mut v);
    v
}

/// A fingerprint of what [`shared`] returns: equal when the shared choices are.
pub fn fingerprint(s: &Settings) -> String {
    hex(&sha256(shared(s).to_string().as_bytes()))
}

/// `local` with the shared choices from `remote` laid over it, so this Mac's
/// own choices stay. Objects merge key by key; anything else is replaced.
pub fn apply(local: &Settings, remote: &Value) -> Result<Settings, String> {
    fn overlay(into: &mut Value, from: &Value) {
        match (into, from) {
            (Value::Object(a), Value::Object(b)) => {
                for (k, v) in b {
                    match a.get_mut(k) {
                        Some(slot) => overlay(slot, v),
                        None => {
                            a.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
            (slot, v) => *slot = v.clone(),
        }
    }
    let mut from = remote.clone();
    strip(&mut from);
    let mut v = serde_json::to_value(local).map_err(|e| e.to_string())?;
    overlay(&mut v, &from);
    serde_json::from_value::<Settings>(v).map(Settings::sanitized).map_err(|e| e.to_string())
}

/// What a sync does with the settings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigAction {
    Nothing,
    /// Write this Mac's shared settings to the repository.
    Push,
    /// Take the repository's shared settings.
    Pull,
}

/// Decide from fingerprints. `synced` is the one remembered from the last
/// sync (empty if never), `remote` the repository's file as (fingerprint,
/// `updated_at`) or None when it has none, and `local_time` when this Mac's
/// settings last changed (Unix seconds).
///
/// A Mac linking a repository for the first time takes what is there (a new
/// Mac must not overwrite a good file with its defaults). When both sides
/// changed since the last sync, the newer one wins.
pub fn decide(local: &str, synced: &str, local_time: u64, remote: Option<(&str, u64)>) -> ConfigAction {
    let Some((remote, remote_time)) = remote else { return ConfigAction::Push };
    if remote == local {
        return ConfigAction::Nothing;
    }
    if synced.is_empty() {
        return ConfigAction::Pull;
    }
    match (local != synced, remote != synced) {
        (true, false) => ConfigAction::Push,
        (false, true) => ConfigAction::Pull,
        (true, true) if remote_time > local_time => ConfigAction::Pull,
        (true, true) => ConfigAction::Push,
        (false, false) => ConfigAction::Nothing,
    }
}

/// A folder name for this Mac: its host name in lowercase letters, digits
/// and hyphens.
pub fn machine_id(host: &str) -> String {
    let mut out = String::new();
    for c in host.trim().trim_end_matches(".local").chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-');
    if out.is_empty() { "mac".into() } else { out.chars().take(40).collect() }
}

/// Is this something to `git clone`? Only HTTPS, SSH (both spellings) and a
/// folder on this Mac. Nothing that starts with a hyphen (git would take it
/// for an option) and no `ext::` style helpers, which run commands.
pub fn valid_repo_url(url: &str) -> bool {
    if url.is_empty() || url.starts_with('-') || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return false;
    }
    if let Some(rest) = url.strip_prefix("https://").or_else(|| url.strip_prefix("ssh://")) {
        // No name or token in the address: it would sit in settings.json and
        // in git's own files. SSH keys and credential helpers do that job.
        let host = rest.split('/').next().unwrap_or("");
        return rest.contains('/') && !host.is_empty() && (url.starts_with("ssh://") || !host.contains('@'));
    }
    if url.starts_with('/') {
        return true;
    }
    // user@host:path
    match url.split_once(':') {
        Some((who, path)) => {
            !path.is_empty() && !path.starts_with(':') && who.split_once('@').is_some_and(|(u, h)| !u.is_empty() && !h.is_empty() && !h.contains('/'))
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sync_is_due_only_on_a_new_day() {
        let m = Memory::default();
        assert!(due(&m, "2026-11-06"), "never synced");
        let m = Memory { last_day: "2026-11-06".into(), ..Memory::default() };
        assert!(!due(&m, "2026-11-06"), "same day: nothing runs");
        assert!(due(&m, "2026-11-07"), "next day");
        assert!(due(&m, "2026-11-05"), "the clock moved back: a day that differs is a new day");
    }

    #[test]
    fn this_macs_own_choices_stay_out_of_the_shared_file() {
        let mut s = Settings::default();
        s.island_x = Some(0.3);
        s.prayer.place = Some("jakarta".into());
        s.prayer.world_place = Some("x".into());
        s.use_24h = false;
        let v = shared(&s);
        assert!(v.get("island_x").is_none() && v.get("monitor").is_none() && v.get("sync").is_none() && v.get("coding").is_none() && v.get("reminders_sync").is_none());
        assert!(v["prayer"].get("place").is_none() && v["prayer"].get("world_place").is_none());
        assert_eq!(v["use_24h"], false);
        assert_eq!(v["prayer"]["remind_before_min"], 10);
    }

    #[test]
    fn the_fingerprint_follows_shared_choices_only() {
        let a = Settings::default();
        let mut b = a.clone();
        b.island_x = Some(0.9);
        b.prayer.place = Some("bandung".into());
        assert_eq!(fingerprint(&a), fingerprint(&b), "a moved island is not a change to share");
        b.use_24h = !b.use_24h;
        assert_ne!(fingerprint(&a), fingerprint(&b));
    }

    #[test]
    fn applying_keeps_this_macs_choices_and_takes_the_rest() {
        let mut local = Settings::default();
        local.island_x = Some(0.2);
        local.prayer.place = Some("jakarta".into());
        local.use_24h = true;
        let mut other = Settings::default();
        other.use_24h = false;
        other.island_x = Some(0.8);
        other.prayer.place = Some("medan".into());
        other.prayer.remind_before_min = 25;
        other.set_addon("sholat", true);
        let got = apply(&local, &shared(&other)).unwrap();
        assert!(!got.use_24h && got.prayer.remind_before_min == 25 && got.addon_on("sholat"));
        assert_eq!(got.island_x, Some(0.2));
        assert_eq!(got.prayer.place.as_deref(), Some("jakarta"));
        // A hostile or hand-edited file is cleaned up like any settings file.
        let got = apply(&local, &serde_json::json!({"expanded_width": 99999})).unwrap();
        assert_eq!(got.expanded_width, 900.0);
        // A value of the wrong type is an error, not a half-applied file.
        assert!(apply(&local, &serde_json::json!({"use_24h": "yes"})).is_err());
    }

    #[test]
    fn decide_covers_each_way_the_two_sides_can_differ() {
        use ConfigAction::*;
        assert_eq!(decide("a", "", 5, None), Push, "an empty repository gets this Mac's");
        assert_eq!(decide("a", "a", 5, Some(("a", 9))), Nothing, "the same");
        assert_eq!(decide("a", "", 99, Some(("b", 1))), Pull, "first link: a new Mac takes what is there");
        assert_eq!(decide("b", "a", 5, Some(("a", 1))), Push, "only this Mac changed");
        assert_eq!(decide("a", "a", 5, Some(("b", 9))), Pull, "only the repository changed");
        assert_eq!(decide("b", "a", 5, Some(("c", 9))), Pull, "both changed, the repository's is newer");
        assert_eq!(decide("b", "a", 9, Some(("c", 5))), Push, "both changed, this Mac's is newer");
    }

    #[test]
    fn machine_ids_make_safe_folder_names() {
        assert_eq!(machine_id("Ismails-MacBook-Pro.local"), "ismails-macbook-pro");
        assert_eq!(machine_id("  My Mac!! "), "my-mac");
        assert_eq!(machine_id("../../etc"), "etc");
        assert_eq!(machine_id(""), "mac");
        assert_eq!(machine_id("...."), "mac");
        assert_eq!(machine_id(&"a".repeat(100)).len(), 40);
    }

    #[test]
    fn only_clonable_addresses_pass() {
        for ok in ["https://github.com/me/motchy-data.git", "ssh://git@gitlab.com/me/x.git", "git@github.com:me/motchy-data.git", "/Users/me/data.git"] {
            assert!(valid_repo_url(ok), "{ok}");
        }
        for bad in ["", "-u", "--upload-pack=sh", "ext::sh -c id", "http://github.com/me/x", "file:///etc", "github.com/me/x", "https://", "https:///x", "git@host:", "a b", "git@:x", "@h:p", "ftp://x/y", "https://me:token@github.com/me/x.git"] {
            assert!(!valid_repo_url(bad), "{bad}");
        }
    }
}

//! Sync to the person's own git repository, with the system's `git`
//! (`poni_core::gitsync` has the rules). Their SSH keys or credential helper
//! do the signing in; this app keeps no token.
//!
//! One run is a short thread: clone the repository once, pull, put the
//! shared settings where the rules say, commit, push, and report. Nothing
//! stays running after it.
//!
//! A run never waits on a question: git may not ask for a password
//! (`GIT_TERMINAL_PROMPT=0`), ssh may not ask for a passphrase
//! (`BatchMode`), and a run that takes longer than [`TIMEOUT`] is stopped.

use poni_core::gitsync::{self, ConfigAction, ConfigFile, Memory, CONFIG_FILE};
use poni_core::settings::Settings;
use poni_core::textnotes::{self, Note};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Longest a single git command may take: a slow line still clones a small
/// repository, a hung prompt doesn't hold the thread.
const TIMEOUT: Duration = Duration::from_secs(90);
/// Asking whether a repository answers is one short round trip.
const CHECK_TIMEOUT: Duration = Duration::from_secs(20);

/// Everything a run needs, read on the UI thread before it starts.
pub struct Job {
    pub url: String,
    /// Where the days of coding are kept on this Mac (`services::wakatime`):
    /// they are joined with the repository's.
    pub coding: Option<PathBuf>,
    /// The local clone.
    pub dir: PathBuf,
    pub settings: Settings,
    pub memory: Memory,
    /// When the settings file last changed (Unix seconds).
    pub settings_time: u64,
    pub now: u64,
    /// The Mac's own date, "2026-11-06".
    pub today: String,
    pub machine: String,
    /// The Notes page's notes (tombstones too), read on the UI thread.
    pub notes: Vec<Note>,
}

/// What a good run found out.
pub struct Done {
    pub memory: Memory,
    /// Settings to take on, when the repository's were chosen.
    pub settings: Option<Settings>,
    /// The notes of this Mac and the repository, as one set: the app takes
    /// them on with the notes it has by then (the newer of each wins).
    pub notes: Vec<Note>,
}

/// This Mac's name for the repository's folders (`poni_core::gitsync::machine_id`).
pub fn machine() -> String {
    let host = Command::new("/bin/hostname").output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    gitsync::machine_id(&host)
}

/// Run one sync. The error is one line for the person; it never holds the
/// repository's address, which may carry a name.
pub fn sync(job: &Job) -> Result<Done, String> {
    run(job).map_err(|e| e.replace(&job.url, "the repository"))
}

/// Does the repository answer? `git ls-remote` reads its branches and writes
/// nothing, and an empty repository answers too. The error is one line,
/// without the address.
pub fn check(url: &str) -> Result<(), String> {
    git_within(&std::env::temp_dir(), &["ls-remote", "--", url, "HEAD"], CHECK_TIMEOUT).map(drop).map_err(|e| e.replace(url, "the repository"))
}

fn run(j: &Job) -> Result<Done, String> {
    let (taken, notes) = git_part(j, &j.url)?;
    let memory = Memory { last_day: j.today.clone(), synced: gitsync::fingerprint(taken.as_ref().unwrap_or(&j.settings)), error: String::new() };
    Ok(Done { memory, settings: taken, notes })
}

/// Pull, put the shared settings and the days where the rules say, commit,
/// push. The settings to take on, if the repository's were chosen, and the
/// notes as one set.
fn git_part(j: &Job, url: &str) -> Result<(Option<Settings>, Vec<Note>), String> {
    git(Path::new("."), &["--version"]).map_err(|_| "git isn't installed".to_string())?;
    ensure_clone(j, url)?;
    let dir = j.dir.as_path();

    // Bring in what other Macs pushed.
    let branch = git(dir, &["symbolic-ref", "--short", "HEAD"])?;
    let remote_has = !git(dir, &["ls-remote", "--heads", "origin", &format!("refs/heads/{branch}")])?.is_empty();
    if remote_has {
        git(dir, &["pull", "--rebase", "--quiet", "origin", &branch]).inspect_err(|_| {
            let _ = git(dir, &["rebase", "--abort"]);
        })?;
    }

    // The settings.
    let path = dir.join(CONFIG_FILE);
    let remote = match fs::read_to_string(&path) {
        Ok(text) => {
            let file: ConfigFile = serde_json::from_str(&text).map_err(|e| format!("{CONFIG_FILE} can't be read: {e}"))?;
            let settings = gitsync::apply(&Settings::default(), &file.settings)?;
            Some((file, settings))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{CONFIG_FILE}: {e}")),
    };
    let local_fp = gitsync::fingerprint(&j.settings);
    let remote_fp = remote.as_ref().map(|(f, s)| (gitsync::fingerprint(s), f.updated_at));
    let action = gitsync::decide(&local_fp, &j.memory.synced, j.settings_time, remote_fp.as_ref().map(|(fp, t)| (fp.as_str(), *t)));
    let mut taken = None;
    match (action, &remote) {
        (ConfigAction::Push, _) => {
            let file = ConfigFile { updated_at: j.now, machine: j.machine.clone(), settings: gitsync::shared(&j.settings) };
            let text = serde_json::to_string_pretty(&file).map_err(|e| e.to_string())?;
            fs::create_dir_all(path.parent().ok_or("no folder")?).map_err(|e| e.to_string())?;
            fs::write(&path, text + "\n").map_err(|e| e.to_string())?;
        }
        (ConfigAction::Pull, Some((file, _))) => taken = Some(gitsync::apply(&j.settings, &file.settings)?),
        _ => {}
    }

    // The days: this Mac's and the repository's become one set, so a day that
    // left WakaTime's window is still there, and so is one another Mac saw.
    if let Some(local) = &j.coding {
        let repo = dir.join("activity/wakatime");
        let both = poni_core::wakatime::union(super::wakatime::load(local), super::wakatime::load(&repo));
        super::wakatime::store(local, &both)?;
        super::wakatime::store(&repo, &both)?;
        readme(dir, &both, &j.today)?;
    }

    let notes = notes_part(dir, &j.notes)?;

    // Send what changed.
    git(dir, &["add", "-A"])?;
    if !git(dir, &["status", "--porcelain"])?.is_empty() {
        let msg = format!("motchy sync {} ({})", j.today, j.machine);
        // A Mac with no git identity still commits.
        let named = !git(dir, &["config", "user.email"]).unwrap_or_default().is_empty();
        let who: &[&str] = if named { &[] } else { &["-c", "user.name=Motchy", "-c", "user.email=motchy@localhost"] };
        let args: Vec<&str> = who.iter().copied().chain(["commit", "--quiet", "-m", &msg]).collect();
        git(dir, &args)?;
    }
    if git(dir, &["rev-parse", "--verify", "-q", "HEAD"]).is_ok() {
        git(dir, &["push", "--quiet", "-u", "origin", "HEAD"])?;
    }

    Ok((taken, notes))
}

/// Largest note file read: a text of [`textnotes::MAX_LEN`] characters fits
/// with room to spare; anything bigger is not one of ours.
const MAX_NOTE_FILE: u64 = 200_000;

/// The notes of this Mac and the repository's `notes/` folder become one set
/// (the newer of each note wins, a deleted one stays a tombstone), and the
/// folder gets every one that changed. Nothing is made when there are none.
fn notes_part(dir: &Path, local: &[Note]) -> Result<Vec<Note>, String> {
    let folder = dir.join("notes");
    let merged = textnotes::merge(local, &load_notes(&folder));
    if merged.is_empty() {
        return Ok(merged);
    }
    fs::create_dir_all(&folder).map_err(|e| format!("notes: {e}"))?;
    for n in &merged {
        let path = folder.join(format!("{}.md", n.id));
        put(&path, &fs::read_to_string(&path).unwrap_or_default(), &textnotes::to_file(n))?;
    }
    Ok(merged)
}

/// The notes in a folder of `<id>.md` files. A file that isn't text, is too
/// big or has a name that is not an id is left alone.
fn load_notes(folder: &Path) -> Vec<Note> {
    let Ok(entries) = fs::read_dir(folder) else { return Vec::new() };
    entries
        .flatten()
        .take(4 * textnotes::MAX_NOTES)
        .filter_map(|e| {
            let path = e.path();
            let id = path.file_name()?.to_str()?.strip_suffix(".md")?.to_string();
            let meta = e.metadata().ok().filter(|m| m.is_file() && m.len() <= MAX_NOTE_FILE)?;
            let at = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
            textnotes::from_file(&id, &fs::read_to_string(&path).ok()?, at)
        })
        .collect()
}

/// Put the coding report in the repository: this month in the `README.md`'s
/// marked block (what the person wrote stays), and the whole history as
/// generated files in `coding-activity/`. Nothing is written when there are no
/// days yet, when the README can't be read as text (it is theirs), or when a
/// file would not change.
fn readme(dir: &Path, days: &[poni_core::wakatime::Day], today: &str) -> Result<(), String> {
    use poni_core::codingreport as report;
    if days.is_empty() {
        return Ok(());
    }
    let path = dir.join("README.md");
    match fs::read_to_string(&path) {
        Ok(old) => put(&path, &old, &report::splice(&old, &report::report(days, today)))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => put(&path, "", &report::splice("", &report::report(days, today)))?,
        Err(_) => {}
    }
    for (file, text) in report::history(days, today) {
        let path = dir.join(&file);
        fs::create_dir_all(path.parent().ok_or("no folder")?).map_err(|e| format!("{file}: {e}"))?;
        put(&path, &fs::read_to_string(&path).unwrap_or_default(), &text)?;
    }
    Ok(())
}

/// Write `new` to `path` unless it is what is there.
fn put(path: &Path, old: &str, new: &str) -> Result<(), String> {
    if new != old {
        fs::write(path, new).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(())
}

/// Clone the repository on the first run, and keep `origin` pointing at the
/// address in Settings when it changes.
fn ensure_clone(j: &Job, url: &str) -> Result<(), String> {
    if j.dir.join(".git").exists() {
        if git(&j.dir, &["remote", "get-url", "origin"]).ok().as_deref() != Some(url) {
            git(&j.dir, &["remote", "set-url", "origin", url])?;
        }
        return Ok(());
    }
    let parent = j.dir.parent().ok_or("no folder")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    git(parent, &["clone", "--quiet", "--", url, &j.dir.to_string_lossy()]).map(drop)
}

/// Run `git` in `dir`; its output (trimmed) or its last line of complaint.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    git_within(dir, args, TIMEOUT)
}

/// [`git`], giving up after `limit`.
fn git_within(dir: &Path, args: &[&str], limit: Duration) -> Result<String, String> {
    let mut c = Command::new("git");
    c.current_dir(dir).args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    c.env("GIT_TERMINAL_PROMPT", "0").env("GIT_ALLOW_PROTOCOL", "https:ssh:file");
    if std::env::var_os("GIT_SSH_COMMAND").is_none() {
        c.env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes");
    }
    // An app opened from Finder has a short PATH: Homebrew's git is not in it.
    let path = std::env::var("PATH").unwrap_or_default();
    c.env("PATH", format!("{path}:/opt/homebrew/bin:/usr/local/bin"));
    let mut child = c.spawn().map_err(|e| e.to_string())?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(s) => break s,
            None if started.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("git {} took too long", args.first().unwrap_or(&"")));
            }
            None => std::thread::sleep(Duration::from_millis(40)),
        }
    };
    fn read(p: Option<impl Read>) -> String {
        let mut s = String::new();
        if let Some(mut p) = p {
            let _ = p.read_to_string(&mut s);
        }
        s.trim().to_string()
    }
    let (out, err) = (read(child.stdout.take()), read(child.stderr.take()));
    if status.success() {
        return Ok(out);
    }
    let line = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("git failed");
    Err(line.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("motchy-gitsync-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    fn job(url: &str, dir: PathBuf, settings: Settings, memory: Memory, time: u64, machine: &str) -> Job {
        Job { url: url.into(), coding: None, dir, settings, memory, settings_time: time, now: time, today: "2026-11-06".into(), machine: machine.into(), notes: Vec::new() }
    }

    #[test]
    fn two_macs_share_settings_through_a_repository() {
        if git(Path::new("."), &["--version"]).is_err() {
            return;
        }
        let root = temp("two");
        let remote = root.join("remote.git");
        git(&root, &["init", "--bare", "--quiet", "-b", "main", &remote.to_string_lossy()]).unwrap();
        let url = remote.to_string_lossy().into_owned();

        // Mac A links an empty repository: its settings go up.
        let mut a = Settings::default();
        a.use_24h = false;
        let done = sync(&job(&url, root.join("a"), a.clone(), Memory::default(), 100, "mac-a")).unwrap();
        assert!(done.settings.is_none());
        assert_eq!(done.memory.last_day, "2026-11-06");
        let mut a_mem = done.memory;

        // Mac B links it for the first time with its defaults: it takes A's.
        let b_def = Settings::default();
        let done = sync(&job(&url, root.join("b"), b_def, Memory::default(), 500, "mac-b")).unwrap();
        let b = done.settings.expect("a new Mac takes what is there");
        assert!(!b.use_24h, "A's choice arrived");
        let mut b_mem = done.memory;

        // B changes something and syncs: it goes up; A takes it next time.
        let mut b2 = b.clone();
        b2.prayer.remind_before_min = 30;
        b2.island_x = Some(0.4);
        let done = sync(&job(&url, root.join("b"), b2, b_mem.clone(), 600, "mac-b")).unwrap();
        assert!(done.settings.is_none());
        b_mem = done.memory;
        let done = sync(&job(&url, root.join("a"), a.clone(), a_mem.clone(), 100, "mac-a")).unwrap();
        let a2 = done.settings.expect("A takes B's change");
        assert_eq!(a2.prayer.remind_before_min, 30);
        assert_eq!(a2.island_x, None, "B's island position stays on B");
        a_mem = done.memory;

        // Nothing changed: a sync does nothing, and the repository has two config commits.
        let done = sync(&job(&url, root.join("a"), a2.clone(), a_mem, 700, "mac-a")).unwrap();
        assert!(done.settings.is_none());
        let n = git(&root.join("a"), &["rev-list", "--count", "HEAD"]).unwrap();
        assert_eq!(n, "2");
        let _ = b_mem;
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_wrong_address_fails_in_one_line_without_the_address() {
        if git(Path::new("."), &["--version"]).is_err() {
            return;
        }
        let root = temp("bad");
        let url = root.join("nothing-here.git").to_string_lossy().into_owned();
        let e = sync(&job(&url, root.join("c"), Settings::default(), Memory::default(), 1, "m")).err().expect("fails");
        assert!(!e.contains(&url) && !e.contains('\n'), "{e}");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_readme_gets_the_coding_report_and_keeps_the_persons_words() {
        if git(Path::new("."), &["--version"]).is_err() {
            return;
        }
        use poni_core::wakatime::{Day, Share};
        let root = temp("readme");
        let remote = root.join("remote.git");
        git(&root, &["init", "--bare", "--quiet", "-b", "main", &remote.to_string_lossy()]).unwrap();
        let url = remote.to_string_lossy().into_owned();
        // The person's own README is already in the repository.
        let seed = root.join("seed");
        git(&root, &["clone", "--quiet", &url, &seed.to_string_lossy()]).unwrap();
        fs::write(seed.join("README.md"), "# Mine\n\nHello.\n").unwrap();
        git(&seed, &["add", "-A"]).unwrap();
        git(&seed, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "--quiet", "-m", "mine"]).unwrap();
        git(&seed, &["push", "--quiet", "-u", "origin", "HEAD"]).unwrap();

        let coding = root.join("coding");
        let days = vec![Day { date: "2026-11-05".into(), total: 3600, projects: vec![Share { name: "motchy".into(), secs: 3600 }], ..Day::default() }, Day { date: "2026-11-06".into(), total: 600, ..Day::default() }];
        super::super::wakatime::store(&coding, &days).unwrap();
        let mut j = job(&url, root.join("a"), Settings::default(), Memory::default(), 100, "mac-a");
        j.coding = Some(coding);
        let done = sync(&j).unwrap();
        let text = fs::read_to_string(root.join("a/README.md")).unwrap();
        assert!(text.starts_with("# Mine\n\nHello.\n"), "{text}");
        assert!(text.contains("## Coding report · November 2026") && text.contains("| motchy | 1h 00m |"), "{text}");
        // The whole history is in its folder, one file for the year.
        let year = fs::read_to_string(root.join("a/coding-activity/2026.md")).unwrap();
        assert!(year.contains("# 2026") && year.contains("<summary>November 2026"), "{year}");
        assert!(root.join("a/coding-activity/README.md").exists());
        // It went to the repository too.
        let check = root.join("check");
        git(&root, &["clone", "--quiet", &url, &check.to_string_lossy()]).unwrap();
        assert_eq!(fs::read_to_string(check.join("README.md")).unwrap(), text);

        // The same days on the same date change nothing: no new commit.
        let commits = |d: &Path| git(d, &["rev-list", "--count", "HEAD"]).unwrap();
        let before = commits(&root.join("a"));
        j.memory = done.memory;
        sync(&j).unwrap();
        assert_eq!(commits(&root.join("a")), before);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_repository_that_is_there_answers_and_one_that_is_gone_does_not() {
        if git(Path::new("."), &["--version"]).is_err() {
            return;
        }
        let root = temp("check");
        let repo = root.join("there.git");
        git(&root, &["init", "--quiet", "--bare", &repo.to_string_lossy()]).unwrap();
        let url = repo.to_string_lossy().into_owned();
        // An empty repository answers too: it is there.
        assert_eq!(check(&url), Ok(()));
        let _ = fs::remove_dir_all(&repo);
        let e = check(&url).err().expect("gone");
        assert!(!e.contains(&url) && !e.contains('\n'), "{e}");
        // An address that would be an option is never passed on as one.
        assert!(check("--upload-pack=true").is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn two_macs_share_notes_and_deletions_through_a_repository() {
        if git(Path::new("."), &["--version"]).is_err() {
            return;
        }
        let note = |id: &str, text: &str, modified: i64| Note { id: id.into(), text: text.into(), modified, deleted: false };
        let root = temp("notes");
        let remote = root.join("remote.git");
        git(&root, &["init", "--bare", "--quiet", "-b", "main", &remote.to_string_lossy()]).unwrap();
        let url = remote.to_string_lossy().into_owned();
        let run = |dir: &str, notes: Vec<Note>, time: u64| {
            let mut j = job(&url, root.join(dir), Settings::default(), Memory::default(), time, dir);
            j.notes = notes;
            sync(&j).unwrap().notes
        };

        // A writes two notes; B, with none, receives them.
        let a = run("a", vec![note("1", "buy milk", 10), note("2", "call the bank", 11)], 100);
        assert_eq!(a.len(), 2);
        assert!(root.join("a/notes/1.md").exists());
        let b = run("b", vec![], 200);
        assert_eq!(b, a);

        // B edits one and deletes the other; A takes both on its next sync.
        let b_edit = vec![note("1", "buy oat milk", 300), Note { deleted: true, text: String::new(), ..note("2", "", 301) }];
        run("b", b_edit, 400);
        let a2 = run("a", a, 500);
        assert_eq!(a2.iter().find(|n| n.id == "1").unwrap().text, "buy oat milk");
        assert!(a2.iter().find(|n| n.id == "2").unwrap().deleted, "the deletion reaches A, the note does not come back");

        // A file written by hand in the repository, and one that is not a note.
        fs::write(root.join("a/notes/todo.md"), "# Plan\n\n- ship\n").unwrap();
        fs::write(root.join("a/notes/.hidden.md"), "no").unwrap();
        fs::write(root.join("a/notes/readme.txt"), "no").unwrap();
        let a3 = run("a", a2, 600);
        assert_eq!(a3.iter().filter(|n| !n.deleted).count(), 2);
        assert!(a3.iter().any(|n| n.id == "todo" && n.text.starts_with("# Plan")));
        assert!(a3.iter().all(|n| n.id != ".hidden" && n.id != "readme"));
        let _ = fs::remove_dir_all(&root);
    }
}

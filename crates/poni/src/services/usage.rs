//! The AI tab's worker: how much of each AI coding plan is left
//! (`studio_usage`), read on one thread, which the Mac app runs too (it
//! shares this file as is). Each service's reader is its own file in
//! `studio_usage`, and when the worker looks and the rules it keeps are
//! `usage::pace`. This file reads what the services ask for (their files, a
//! Mac keychain item, the keys typed into the tab), makes their requests,
//! keeps the typed keys sealed (`services::secret` at
//! `paths::usage_key_path`: DPAPI on Windows, an owner-only file on a Mac).
//!
//! The thread starts the first time the AI tab opens or the person has the
//! heads-up notices on (`Watch`), and reads only while one of them wants it:
//! as the tab opens (at most once in 15 s), then once a minute while it is
//! on screen, and every 5 minutes for the notices alone; never while nobody
//! is at the computer, nor for the notices on battery saver. A service's own
//! server is asked at that service's pace (every 2 minutes for most while
//! the tab is open, every 10 for the notices alone), and failures wait
//! longer. A code on screen is asked about every few seconds until it is
//! approved, refused or runs out (the server gives it 10 minutes). In
//! between, the thread sleeps on a condition variable at zero CPU.
//!
//! Requests go through a client of the worker's own (`Client::private`): it
//! follows no redirect (a 3xx is the answer, so a sign-in never goes on to
//! another host), keeps no cookies and caches nothing. A typed key a server
//! refused isn't sent there again this run (`pace::Refusals`).
//!
//! Nothing is written but the typed keys, and the log gets one line per
//! change of a service's state: never a token, a key, an answer or an email.

use crate::commands::UsageCmd;
use crate::events::{AppEvent, EventSink};
use crate::services::http::{Client, FetchError, Method, MtaGuard};
use studio_usage::alerts::{Alert, AlertCfg, Memory};
use studio_usage::pace::{self, KeyGate, Look, Looks, Pace, Refusals};
use studio_usage::{self as usage, Dirs, Got, NetFail, Os, Outcome, Provider, ProviderId, Reach, Reply, Request, Snapshot, Trouble, Want};
use std::cell::RefCell;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

const USER_AGENT: &str = concat!("OyenzaStudio/", env!("CARGO_PKG_VERSION"), " (+https://github.com/lucacadalora/oyenza-studio)");
/// Budget for one request: connect, headers and body.
const TIMEOUT: Duration = Duration::from_secs(8);
/// A typed key longer than this is a paste gone wrong.
const KEY_MAX: usize = 4096;

/// What the island wants from the worker (`App::set_usage_watch`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Watch {
    /// The AI tab is on screen and someone is at the computer.
    pub open: bool,
    /// The heads-up notices are on, someone is at the computer and battery
    /// saver is off.
    pub notices: bool,
    /// The notices speak once a limit passes this percent
    /// (`settings.usage.near_pct`).
    pub near_pct: u8,
    /// Reset times as the app's clock says them (`settings.use_24h`).
    pub use_24h: bool,
    /// The services the person took off the tab, never read
    /// (`settings.usage.hidden`, as [`picked`] bits)...
    pub hidden: u32,
    /// ...and the ones they put on it, read even when nothing of them is on
    /// this computer (`settings.usage.added`).
    pub added: u32,
}

/// Service keys (`settings.usage.hidden` / `added`) as [`Watch`] bits.
pub fn picked(keys: &[String]) -> u32 {
    pace::bits(keys)
}

/// What the worker tells the island.
#[derive(Clone, Debug, PartialEq)]
pub enum UsageEvent {
    /// One service's refresh, finished at unix `at`: what it read, or why
    /// there is nothing.
    Reading { id: ProviderId, outcome: Outcome, at: i64 },
    /// The secret names that hold a key (the typed ones).
    Keys(BTreeSet<&'static str>),
    /// Heads-up notices for the island (`studio_usage::alerts`), the
    /// most urgent first.
    Alerts(Vec<Alert>),
    /// A look at the services started (true) or ended, while the tab is open.
    Refreshing(bool),
}

pub struct UsageService {
    shared: Arc<Shared>,
}

impl UsageService {
    pub fn start(sink: EventSink) -> Result<Self, String> {
        let shared = Arc::new(Shared { q: Mutex::new(Queue::default()), cv: Condvar::new() });
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("poni-usage".into())
            .spawn(move || worker_main(worker, sink))
            .map_err(|e| format!("could not start the AI usage thread: {e}"))?;
        Ok(Self { shared })
    }

    /// The tab opened or closed, someone came or went, the notices, battery
    /// saver or the tab's choices changed (UI thread safe).
    pub fn set_watch(&self, w: Watch) {
        let mut q = self.shared.lock();
        if q.watch != w {
            q.watch = w;
            drop(q);
            self.shared.cv.notify_all();
        }
    }

    /// Something the AI tab asked for (UI thread safe).
    pub fn command(&self, c: UsageCmd) {
        self.shared.lock().cmds.push(c);
        self.shared.cv.notify_all();
    }
}

impl Drop for UsageService {
    fn drop(&mut self) {
        // The worker exits at its next wait; a request in flight ends on its
        // own timeout. Never join from the UI thread.
        self.shared.lock().shutdown = true;
        self.shared.cv.notify_all();
    }
}

#[derive(Default)]
struct Queue {
    watch: Watch,
    cmds: Vec<UsageCmd>,
    shutdown: bool,
}

struct Shared {
    q: Mutex<Queue>,
    cv: Condvar,
}

impl Shared {
    /// The queue holds plain data, so a panic elsewhere can't leave it
    /// inconsistent: recover from poisoning instead of panicking.
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.q.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn wait<'a>(&self, g: MutexGuard<'a, Queue>, timeout: Option<Duration>) -> MutexGuard<'a, Queue> {
        match timeout {
            Some(d) => match self.cv.wait_timeout(g, d) {
                Ok((g, _)) => g,
                Err(p) => p.into_inner().0,
            },
            None => self.cv.wait(g).unwrap_or_else(|p| p.into_inner()),
        }
    }
}

/// One service as the worker knows it.
#[derive(Default)]
struct Svc {
    pace: Pace,
    /// The last good reading: the one the tab shows.
    kept: Option<Snapshot>,
    /// What the notices remember of it.
    memory: Memory,
    /// Its latest trouble; None when it worked.
    trouble: Option<Trouble>,
    /// What the log last said of it.
    said: String,
    /// The log said once that it broke the rules or had a line break in a
    /// header.
    warned: bool,
    /// Its reader failed (a panic): left alone for the rest of the run, so
    /// the other services and the tab carry on.
    failed: bool,
}

struct Worker {
    sink: EventSink,
    client: Option<Client>,
    dirs: Dirs,
    /// Every name a key is kept under, and those that hold one now.
    names: Vec<&'static str>,
    keys: BTreeSet<&'static str>,
    /// One per service, in `ProviderId::ALL` order.
    svcs: Vec<Svc>,
    /// Where the typed keys were refused, so they aren't sent there again.
    refusals: Refusals,
    looks: Looks,
    /// What the island wanted when the worker last looked at its queue.
    watch: Watch,
    /// The notices' choices as of the last look.
    cfg: AlertCfg,
    /// The pacing's steady clock.
    epoch: Instant,
}

fn worker_main(shared: Arc<Shared>, sink: EventSink) {
    let _com = MtaGuard::new();
    let mut w = Worker::new(sink);
    w.restore();
    loop {
        let cmds = {
            let mut q = shared.lock();
            loop {
                if q.shutdown {
                    return;
                }
                w.saw(q.watch);
                if !q.cmds.is_empty() {
                    break;
                }
                match w.next_wait() {
                    Some(d) if d.is_zero() => break,
                    d => q = shared.wait(q, d),
                }
            }
            std::mem::take(&mut q.cmds)
        };
        for c in cmds {
            w.run(c);
        }
        w.due(&shared);
    }
}

impl Worker {
    fn new(sink: EventSink) -> Worker {
        let dirs = dirs();
        let names = pace::secret_names(&dirs).into_iter().filter(|n| pace::safe_name(n)).collect();
        Worker {
            sink,
            client: None,
            dirs,
            names,
            keys: BTreeSet::new(),
            svcs: ProviderId::ALL.iter().map(|_| Svc::default()).collect(),
            refusals: Refusals::default(),
            looks: Looks::default(),
            watch: Watch::default(),
            cfg: AlertCfg::default(),
            epoch: Instant::now(),
        }
    }

    fn send(&self, ev: UsageEvent) {
        self.sink.send(AppEvent::Usage(ev));
    }

    /// Seconds on the pacing's clock.
    fn tick(&self) -> i64 {
        self.epoch.elapsed().as_secs() as i64
    }

    /// The keys kept from earlier runs. A
    /// key counts once it unseals (a file brought from another computer
    /// doesn't).
    fn restore(&mut self) {
        self.keys = self.names.iter().copied().filter(|n| load_key(n).is_some()).collect();
        self.send(UsageEvent::Keys(self.keys.clone()));
    }

    /// What the island wants, each time the worker looks at its queue.
    fn saw(&mut self, w: Watch) {
        let now = self.tick();
        self.looks.saw(w.open, w.notices, now);
        // A service put on the tab, or no longer hidden, is read at once.
        let more = (w.added & !self.watch.added) != 0 || (self.watch.hidden & !w.hidden) != 0;
        if more && (w.open || w.notices) {
            self.looks.now(now);
        }
        self.watch = w;
    }

    /// How long the worker may sleep: until the next look (None: until
    /// something changes).
    fn next_wait(&self) -> Option<Duration> {
        self.looks.due(self.watch.open, self.watch.notices).map(|t| Duration::from_secs(t.saturating_sub(self.tick()).max(0) as u64))
    }

    /// What is due: a look.
    fn due(&mut self, shared: &Shared) {
        if self.looks.due(self.watch.open, self.watch.notices).is_some_and(|t| t <= self.tick()) {
            self.look(shared);
        }
    }

    // ------------------------------------------------------------ looking

    /// Every shown service, once (`pace` says what each may do).
    fn look(&mut self, shared: &Shared) {
        let w = self.watch;
        let look = self.looks.begin(w.open, self.tick());
        if w.open {
            self.send(UsageEvent::Refreshing(true));
        }
        self.cfg = self.alert_cfg();
        let mut alerts = Vec::new();
        let mut cut = false;
        for (i, p) in usage::all().into_iter().enumerate() {
            if self.stop(shared) {
                cut = true;
                break;
            }
            let wants = p.wants(&self.dirs);
            if !self.svcs[i].failed && self.shown(p, &wants) {
                alerts.extend(self.refresh(i, p, &wants, look));
            }
        }
        if !cut {
            self.looks.done();
        }
        if w.open {
            self.send(UsageEvent::Refreshing(false));
        }
        self.say(alerts);
    }

    /// A look stops between services when the app quits, nobody is there
    /// any more, or the tab asked for something (the look then carries on
    /// right after it).
    fn stop(&mut self, shared: &Shared) -> bool {
        let q = shared.lock();
        if q.shutdown || !(q.watch.open || q.watch.notices) {
            return true;
        }
        let asked = !q.cmds.is_empty();
        drop(q);
        if asked {
            self.looks.cut(self.tick());
        }
        asked
    }

    /// On the tab: not hidden, and added, or more than a service that only
    /// takes a typed key, or holding its key.
    fn shown(&self, p: &dyn Provider, wants: &[Want]) -> bool {
        let bit = pace::bit(p.id());
        if self.watch.hidden & bit != 0 {
            return false;
        }
        let var_set = p.env().iter().any(|v| self.dirs.var(v).is_some());
        self.watch.added & bit != 0 || !pace::typed_key_only(wants, var_set) || wants.iter().any(|w| matches!(w, Want::Secret(s) if self.keys.contains(s)))
    }

    /// The notices' choices and the app's clock.
    fn alert_cfg(&self) -> AlertCfg {
        let offset_min = poni_core::oyenza::offset_minutes(&crate::clock::local_now(), crate::clock::unix_now());
        AlertCfg { near: pace::near(self.watch.near_pct), offset_min, use_24h: self.watch.use_24h, ..AlertCfg::default() }
    }

    /// One service: what it asks for read, its requests made, what it found
    /// kept and told. Returns its notices.
    fn refresh(&mut self, i: usize, p: &dyn Provider, wants: &[Want], look: Look) -> Vec<Alert> {
        let Some(reach) = self.svcs[i].pace.reach(self.tick(), p.min_gap_s(), look) else { return Vec::new() };
        let now = crate::clock::unix_now();
        let Worker { dirs, names, client, refusals, .. } = self;
        // The service's typed key, if it reads one, goes through the door:
        // never again to an address that refused it.
        let gate = RefCell::new(KeyGate::new(refusals, p.takes_key().map(|k| k.secret)));
        // A reader reads other apps' files and servers' answers: should one
        // of them trip it up, the rest of the tab carries on without it.
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let read = |w: &Want| {
                let got = read(w, names);
                gate.borrow_mut().saw(w, &got);
                got
            };
            pace::run(p, dirs, wants, now, reach, read, |req| gate.borrow_mut().ask(req, |r| fetch(client, r)))
        }));
        let gate = gate.into_inner();
        match run {
            Ok(mut run) => {
                gate.done(run.outcome.as_ref());
                // The Mac's keychain is never asked from the background (its
                // prompt would come out of nowhere): a sign-in kept only there
                // reads as missing on a notices-only look, which means "not
                // asked yet", not "signed out". What the tab had stays.
                if look == Look::Notices && wants.iter().any(|w| matches!(w, Want::Keychain { .. })) && matches!(run.outcome, Some(Err(Trouble::NotFound | Trouble::SignedOut))) {
                    run.outcome = None;
                }
                self.heard(i, p, reach, run)
            }
            Err(_) => {
                self.svcs[i].failed = true;
                crate::pwarn!("usage: {}: its reader failed; left alone until the app starts again", p.id().key());
                Vec::new()
            }
        }
    }

    /// What a refresh came to: kept, logged and told. Every good reading
    /// goes through the notices' memory, so it stays current while they're
    /// off (nothing old to say once they come on, and "back" still comes
    /// after a break, when nothing was read); what it says goes to the
    /// island only while they're on.
    fn heard(&mut self, i: usize, p: &dyn Provider, reach: Reach, run: pace::Run) -> Vec<Alert> {
        self.heard_at(i, p, reach, run, crate::clock::unix_now())
    }

    /// [`Worker::heard`] at `now` (the tests' own clock: a reset they set
    /// must not pass because the real one moved on).
    fn heard_at(&mut self, i: usize, p: &dyn Provider, reach: Reach, run: pace::Run, now: i64) -> Vec<Alert> {
        let (id, tick) = (p.id(), self.tick());
        let svc = &mut self.svcs[i];
        svc.pace.heard(tick, reach, run.asked > 0, run.outcome.as_ref());
        if (run.broke.is_some() || run.unsent) && !svc.warned {
            svc.warned = true;
            match run.broke {
                Some(rule) => crate::pwarn!("usage: {}: {rule}; stopped", id.key()),
                None => crate::pwarn!("usage: {}: a sign-in or key with a line break in it, not sent", id.key()),
            }
        }
        // Stopped for breaking the rules, or an answer set aside: the tab
        // keeps what it had.
        let Some(outcome) = run.outcome else { return Vec::new() };
        match outcome {
            Ok(snap) => {
                if pace::replaces(svc.kept.as_ref(), &snap) {
                    svc.kept = Some(snap);
                }
                let show = svc.kept.clone().expect("kept just now");
                svc.trouble = None;
                let alerts = svc.memory.changes(&show, now, &self.cfg);
                self.note(i, "numbers in");
                self.send(UsageEvent::Reading { id, outcome: Ok(show), at: now });
                if self.watch.notices {
                    alerts
                } else {
                    Vec::new()
                }
            }
            // Only "not asked online yet": never a trouble on the tab.
            Err(Trouble::NeedsOnline) => Vec::new(),
            Err(t) => {
                if matches!(t, Trouble::NotFound | Trouble::SignedOut | Trouble::NeedsKey | Trouble::Disabled) {
                    // The way in is gone (or switched off), and with it its
                    // numbers and what the notices knew.
                    svc.kept = None;
                    svc.memory = Memory::default();
                }
                svc.trouble = Some(t.clone());
                self.note(i, &trouble_words(&t));
                self.send(UsageEvent::Reading { id, outcome: Err(t), at: now });
                Vec::new()
            }
        }
    }

    /// One log line per change of a service's state.
    fn note(&mut self, i: usize, state: &str) {
        let svc = &mut self.svcs[i];
        if svc.said != state {
            svc.said = state.to_string();
            crate::pinfo!("usage: {}: {state}", ProviderId::ALL[i].key());
        }
    }

    /// Notices for the island, the most urgent first.
    fn say(&self, mut alerts: Vec<Alert>) {
        if !alerts.is_empty() {
            usage::sort_small_by(&mut alerts, |a, b| a.kind.urgency() < b.kind.urgency());
            self.send(UsageEvent::Alerts(alerts));
        }
    }

    // ------------------------------------------------------------ the tab's asks

    fn run(&mut self, c: UsageCmd) {
        match c {
            // Every key is tried again, wherever it was refused. (The Mac app
            // tells its keychain reader to ask again first.)
            UsageCmd::Refresh => {
                self.refusals.clear();
                self.looks.ask(self.tick());
            }
            UsageCmd::SetKey { secret, key } => self.set_key(secret, &key),
            UsageCmd::ClearKey(secret) => self.clear_key(secret),
            // The app keeps these in its settings; the watch brings them.
            UsageCmd::Hide(_) | UsageCmd::Show(_) => {}
        }
    }

    /// Keep a typed key sealed, say which keys are kept, read its services
    /// at once.
    fn set_key(&mut self, name: &'static str, key: &str) {
        if !self.names.contains(&name) {
            crate::pwarn!("usage: no service keeps a key called {name}");
            return;
        }
        let key = key.trim();
        if key.is_empty() {
            return self.clear_key(name);
        }
        if key.len() > KEY_MAX || key.chars().any(char::is_control) {
            // A key is one line: one with a line break in it would only be refused.
            crate::pwarn!("usage: {name}: not kept (a line break in it, or far too long for a key)");
            self.send(UsageEvent::Keys(self.keys.clone()));
            return;
        }
        match crate::services::secret::save(&crate::paths::usage_key_path(name), key) {
            Ok(()) => {
                crate::pinfo!("usage: {name}: key kept");
                self.keys.insert(name);
            }
            Err(e) => crate::pwarn!("usage: {name}: couldn't keep the key ({e})"),
        }
        self.refusals.forget(name);
        self.send(UsageEvent::Keys(self.keys.clone()));
        self.reread(name);
    }

    /// Forget a typed key, say so, and read its
    /// services at once.
    fn clear_key(&mut self, name: &'static str) {
        if !self.names.contains(&name) {
            return;
        }
        self.drop_key(name);
        self.reread(name);
    }

    /// Delete a kept key, with what was refused of it, and say which keys
    /// are kept.
    fn drop_key(&mut self, name: &'static str) {
        match crate::services::secret::delete(&crate::paths::usage_key_path(name)) {
            Ok(()) => {
                if self.keys.remove(name) {
                    crate::pinfo!("usage: {name}: key removed");
                }
            }
            Err(e) => crate::pwarn!("usage: {name}: couldn't remove the key ({e})"),
        }
        self.refusals.forget(name);
        self.send(UsageEvent::Keys(self.keys.clone()));
    }

    /// A key changed: what was read with the old one goes, and each shown
    /// service that uses it is read at once, asking its server (the point of
    /// a new key).
    fn reread(&mut self, name: &str) {
        self.cfg = self.alert_cfg();
        let mut alerts = Vec::new();
        for (i, p) in usage::all().into_iter().enumerate() {
            let wants = p.wants(&self.dirs);
            if self.svcs[i].failed || self.watch.hidden & pace::bit(p.id()) != 0 || !wants.iter().any(|w| matches!(w, Want::Secret(s) if *s == name)) {
                continue;
            }
            let said = std::mem::take(&mut self.svcs[i].said);
            self.svcs[i] = Svc { said, ..Svc::default() };
            alerts.extend(self.refresh(i, p, &wants, Look::Asked));
        }
        self.say(alerts);
    }
}

/// What one want gave: files (whole, the newest of a folder, a piece) as
/// `pace` reads them, a Mac keychain item, a kept key.
fn read(w: &Want, names: &[&'static str]) -> Got {
    match w {
        Want::File { path, max } => pace::read_file(path, *max),
        Want::Newest { dir, suffix, depth, count, tail } => pace::read_newest(dir, suffix, *depth, *count, *tail),
        Want::Keychain { service, account } => keychain_item(service, account.as_deref()),
        Want::Secret(name) if names.contains(name) => load_key(name).map_or(Got::Missing, Got::Text),
        Want::Secret(_) => Got::Missing,
        Want::Range { path, offset, len } => pace::read_range(path, *offset, *len),
    }
}

/// A typed key, unsealed.
fn load_key(name: &str) -> Option<String> {
    crate::services::secret::load(&crate::paths::usage_key_path(name))
}

/// One request with the worker's own client (made on the first one, kept:
/// no redirects, cookies or cache). An answer comes back whole; a failure
/// never brings its text, which can name the URL.
fn fetch(client: &mut Option<Client>, req: &Request) -> Result<Reply, NetFail> {
    if client.is_none() {
        *client = Some(Client::private(USER_AGENT).map_err(|_| NetFail::Other("no HTTP client".into()))?);
    }
    let c = client.as_ref().expect("client was just made");
    let method = match req.method {
        usage::Method::Get => Method::Get,
        usage::Method::Post => Method::Post,
    };
    match c.send(method, &req.url, &pace::send_headers(req), req.json.as_deref(), TIMEOUT) {
        Ok(r) => Ok(Reply { status: r.status, body: r.body, retry_after_s: r.retry_after_s }),
        Err(FetchError::Timeout) => Err(NetFail::Timeout),
        Err(FetchError::Network(_)) => Err(NetFail::Network("no connection".into())),
        Err(FetchError::Other(_)) => Err(NetFail::Other("the request failed".into())),
    }
}

/// A trouble as the log says it. Unreadable's reason is the service's own
/// few words ("HTTP 404"), never an answer's text.
fn trouble_words(t: &Trouble) -> String {
    match t {
        Trouble::NotFound => "not on this computer".into(),
        Trouble::SignedOut => "signed out".into(),
        Trouble::Expired => "sign-in expired".into(),
        Trouble::NeedsKey => "no key".into(),
        Trouble::Refused => "sign-in or key refused".into(),
        Trouble::Disabled => "account switched off".into(),
        Trouble::Busy { .. } => "too many asks, waiting".into(),
        Trouble::Offline => "no answer, waiting".into(),
        Trouble::Unreadable(why) => format!("answer not read ({})", why.chars().take(60).collect::<String>()),
        Trouble::Denied => "the keychain said no".into(),
        Trouble::NeedsOnline => "needs its server".into(),
    }
}

/// Where things live on this computer, and the variables the services read
/// (some are keys: `Dirs` never prints them). A Mac app started from the
/// Finder or the Dock doesn't see variables set in a shell profile, so there
/// a key typed into the tab is the way in.
fn dirs() -> Dirs {
    let var = |k: &str| std::env::var_os(k).map(PathBuf::from).filter(|p| !p.as_os_str().is_empty());
    #[cfg(target_os = "macos")]
    let (os, home, config, local) = {
        let home = var("HOME").unwrap_or_default();
        let lib = home.join("Library");
        (Os::Mac, home, lib.join("Application Support"), lib.join("Caches"))
    };
    #[cfg(not(target_os = "macos"))]
    let (os, home, config, local) = {
        let home = var("USERPROFILE").unwrap_or_default();
        let config = var("APPDATA").unwrap_or_else(|| home.join("AppData").join("Roaming"));
        let local = var("LOCALAPPDATA").unwrap_or_else(|| home.join("AppData").join("Local"));
        (Os::Windows, home, config, local)
    };
    let env = pace::env_names().into_iter().filter_map(|k| std::env::var(k).ok().filter(|v| !v.trim().is_empty()).map(|v| (k.to_string(), v))).collect();
    Dirs { os, home, config, local, env }
}

/// One keychain item, through the Mac's reader (`services::keychain`), which
/// keeps macOS's prompt in hand itself: at most once per opening of the tab,
/// never from the background, a Deny kept until Refresh.
#[cfg(target_os = "macos")]
fn keychain_item(service: &str, account: Option<&str>) -> Got {
    crate::services::keychain::read(service, account)
}

/// Windows has no keychain.
#[cfg(not(target_os = "macos"))]
fn keychain_item(_service: &str, _account: Option<&str>) -> Got {
    Got::Missing
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    /// A service's place in `ProviderId::ALL`, and so in the worker's list.
    fn at(id: ProviderId) -> usize {
        ProviderId::ALL.iter().position(|p| *p == id).unwrap_or(0)
    }
    use crate::events::SendHwnd;
    use studio_usage::{Money, Source, Window, WindowKind};
    use std::sync::mpsc::{channel, Receiver};

    const NOW: i64 = 1_793_077_200;

    fn worker() -> (Worker, Receiver<AppEvent>) {
        let (tx, rx) = channel();
        (Worker::new(EventSink::new(tx, SendHwnd(0))), rx)
    }

    fn told(rx: &Receiver<AppEvent>) -> Vec<UsageEvent> {
        rx.try_iter()
            .filter_map(|e| match e {
                AppEvent::Usage(u) => Some(u),
                _ => None,
            })
            .collect()
    }

    fn ran(outcome: Outcome, asked: usize) -> pace::Run {
        pace::Run { outcome: Some(outcome), asked, broke: None, unsent: false }
    }

    fn codex(source: Source, as_of: i64, used: f32) -> Snapshot {
        let mut s = Snapshot::new(ProviderId::Codex, as_of, source);
        s.windows.push(Window::new(WindowKind::Session, "5-hour", used).resets(Some(NOW + 3600)).length(Some(18_000)));
        s
    }

    /// The worker hears a reading as it is taken, so the clock is the
    /// reading's own time (NOW for a trouble).
    fn heard(w: &mut Worker, id: ProviderId, reach: Reach, run: pace::Run) -> Vec<Alert> {
        let now = match &run.outcome {
            Some(Ok(s)) => s.as_of,
            _ => NOW,
        };
        w.heard_at(at(id), usage::provider(id), reach, run, now)
    }

    fn reading(ev: &UsageEvent) -> &Outcome {
        match ev {
            UsageEvent::Reading { outcome, .. } => outcome,
            other => panic!("not a reading: {other:?}"),
        }
    }

    #[test]
    fn readings_are_kept_and_told_but_never_needs_online() {
        let (mut w, rx) = worker();
        let online = codex(Source::Online, NOW, 0.4);
        heard(&mut w, ProviderId::Codex, Reach::Online, ran(Ok(online.clone()), 1));
        assert_eq!(told(&rx).iter().map(reading).collect::<Vec<_>>(), [&Ok(online.clone())]);
        // A log line older than the server's answer: the tab keeps the answer.
        heard(&mut w, ProviderId::Codex, Reach::Local, ran(Ok(codex(Source::Local, NOW - 60, 0.3)), 0));
        assert_eq!(told(&rx).iter().map(reading).collect::<Vec<_>>(), [&Ok(online.clone())]);
        // A newer one replaces it.
        let newer = codex(Source::Local, NOW + 30, 0.45);
        heard(&mut w, ProviderId::Codex, Reach::Local, ran(Ok(newer.clone()), 0));
        assert_eq!(told(&rx).iter().map(reading).collect::<Vec<_>>(), [&Ok(newer.clone())]);
        // "Not asked online yet" is never told.
        heard(&mut w, ProviderId::Codex, Reach::Local, ran(Err(Trouble::NeedsOnline), 0));
        assert!(told(&rx).is_empty());
        // A trouble is told and keeps the numbers; a sign-in that's gone drops them.
        heard(&mut w, ProviderId::Codex, Reach::Online, ran(Err(Trouble::Offline), 1));
        assert_eq!(told(&rx).iter().map(reading).collect::<Vec<_>>(), [&Err(Trouble::Offline)]);
        assert_eq!(w.svcs[at(ProviderId::Codex)].kept, Some(newer));
        heard(&mut w, ProviderId::Codex, Reach::Online, ran(Err(Trouble::SignedOut), 1));
        assert_eq!(told(&rx).iter().map(reading).collect::<Vec<_>>(), [&Err(Trouble::SignedOut)]);
        assert_eq!(w.svcs[at(ProviderId::Codex)].kept, None);
        // So does an account switched off on the service's side.
        heard(&mut w, ProviderId::Codex, Reach::Online, ran(Ok(online.clone()), 1));
        heard(&mut w, ProviderId::Codex, Reach::Online, ran(Err(Trouble::Disabled), 1));
        assert_eq!(w.svcs[at(ProviderId::Codex)].kept, None);
    }

    #[test]
    fn a_rule_breaker_changes_nothing() {
        let (mut w, rx) = worker();
        let online = codex(Source::Online, NOW, 0.4);
        heard(&mut w, ProviderId::Codex, Reach::Online, ran(Ok(online.clone()), 1));
        told(&rx);
        let broke = pace::Run { outcome: None, asked: 4, broke: Some("asked its server more than 4 times in one refresh"), unsent: false };
        heard(&mut w, ProviderId::Codex, Reach::Online, broke);
        assert!(told(&rx).is_empty());
        assert_eq!(w.svcs[at(ProviderId::Codex)].kept, Some(online));
        assert!(w.svcs[at(ProviderId::Codex)].warned);
    }

    #[test]
    fn notices_speak_only_when_they_are_on() {
        let (mut w, _rx) = worker();
        w.watch.notices = true;
        // The first reading only sets where things stand.
        assert!(heard(&mut w, ProviderId::Codex, Reach::Online, ran(Ok(codex(Source::Online, NOW, 0.5)), 1)).is_empty());
        let alerts = heard(&mut w, ProviderId::Codex, Reach::Online, ran(Ok(codex(Source::Online, NOW + 120, 0.85)), 1));
        assert_eq!((alerts.len(), alerts.first().map(|a| a.provider)), (1, Some(ProviderId::Codex)), "{alerts:?}");
        // Said once a period.
        assert!(heard(&mut w, ProviderId::Codex, Reach::Online, ran(Ok(codex(Source::Online, NOW + 240, 0.86)), 1)).is_empty());
        // Past the line with the notices off: nothing on the island, and
        // nothing old to say once they come back on.
        let (mut off, _rx2) = worker();
        heard(&mut off, ProviderId::Codex, Reach::Online, ran(Ok(codex(Source::Online, NOW, 0.5)), 1));
        assert!(heard(&mut off, ProviderId::Codex, Reach::Online, ran(Ok(codex(Source::Online, NOW + 120, 0.85)), 1)).is_empty());
        off.watch.notices = true;
        assert!(heard(&mut off, ProviderId::Codex, Reach::Online, ran(Ok(codex(Source::Online, NOW + 240, 0.86)), 1)).is_empty());
    }

    #[test]
    fn which_services_a_look_reads() {
        let (mut w, _rx) = worker();
        // Pretend nothing is set, whatever this computer has.
        w.dirs.env.clear();
        let shown = |w: &Worker, id: ProviderId| {
            let p = usage::provider(id);
            w.shown(p, &p.wants(&w.dirs))
        };
        assert!(shown(&w, ProviderId::Codex));
        // OpenRouter takes only a typed key (or its variable).
        assert!(!shown(&w, ProviderId::OpenRouter));
        w.keys.insert("openrouter");
        assert!(shown(&w, ProviderId::OpenRouter));
        w.keys.clear();
        w.watch.added = picked(&["openrouter".into()]);
        assert!(shown(&w, ProviderId::OpenRouter));
        w.dirs.env.push(("OPENROUTER_API_KEY".into(), "sk-or-v1-test-token".into()));
        w.watch.added = 0;
        assert!(shown(&w, ProviderId::OpenRouter));
        // Hidden is never read.
        w.watch.hidden = picked(&["codex".into(), "openrouter".into()]);
        assert!(!shown(&w, ProviderId::Codex) && !shown(&w, ProviderId::OpenRouter));
        assert!(shown(&w, ProviderId::Claude));
    }

    #[test]
    fn a_keychain_sign_in_isnt_gone_when_the_background_cant_ask() {
        let (mut w, rx) = worker();
        // A Mac's Claude Code: its sign-in in the keychain, nothing found here.
        w.dirs = Dirs::sample(Os::Mac);
        let (i, p) = (at(ProviderId::Claude), usage::provider(ProviderId::Claude));
        let wants = p.wants(&w.dirs);
        assert!(wants.iter().any(|x| matches!(x, Want::Keychain { .. })));
        let kept = Snapshot::new(ProviderId::Claude, NOW, Source::Online);
        w.svcs[i].kept = Some(kept.clone());
        // The notices alone never ask the keychain: "missing" is "not asked yet".
        w.refresh(i, p, &wants, Look::Notices);
        assert!(told(&rx).is_empty());
        assert_eq!(w.svcs[i].kept, Some(kept));
        // With the tab open the keychain is asked, and its answer counts.
        w.refresh(i, p, &wants, Look::Open);
        assert_eq!(told(&rx).iter().map(reading).collect::<Vec<_>>(), [&Err(Trouble::NotFound)]);
        assert_eq!(w.svcs[i].kept, None);
    }

    #[test]
    fn the_worker_wakes_for_a_look_as_the_tab_opens() {
        let (mut w, _rx) = worker();
        assert_eq!(w.next_wait(), None);
        w.saw(Watch { open: true, ..Watch::default() });
        assert_eq!(w.next_wait(), Some(Duration::ZERO));
        w.looks.begin(true, w.tick());
        assert!(w.next_wait().is_some_and(|d| d > Duration::from_secs(50)));
        w.saw(Watch::default());
        assert_eq!(w.next_wait(), None);
    }
}

//! Coding time on the Mac: a small server on this Mac's loopback that
//! WakaTime's editor plugins send their heartbeats to
//! (`poni_core::wakatime` has the rules), and the files the days are kept in.
//!
//! It runs only while the person has switched Track coding time on. It
//! listens on 127.0.0.1 and nothing else, and one thread waits in `accept`,
//! so at rest it costs nothing. A request that carries an `Origin` header
//! (a web page's `fetch`) or a `Host` other than this Mac's own (DNS
//! rebinding) is refused, so a site can't make up coding time. Bodies are
//! capped, reads time out, and no heartbeat is stored: only the days.
//!
//! The sync (`services::gitsync`) mirrors the days into the person's
//! repository, which is where they outlive the Mac.

use crate::events::{AppEvent, EventSink};
use poni_core::store::{load_json, write_bytes_atomic, write_json_atomic};
use poni_core::wakatime::{self as waka, Day, Tracker};
use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// A request's body: heartbeats are small, a bulk of a thousand is a few hundred KB.
const MAX_BODY: usize = 1024 * 1024;
const MAX_HEAD: usize = 16 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The running server. Dropping it stops it.
pub struct Server {
    stop: Arc<AtomicBool>,
    addr: SocketAddr,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Server {
    /// Listen on `port` (0 = any, for tests) and keep the days in `dir`.
    /// Each batch of heartbeats ends with `AppEvent::Coding`.
    pub fn start(dir: PathBuf, port: u16, sink: EventSink) -> Result<Server, String> {
        let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("port {port} can't be used ({e})"))?;
        let addr = listener.local_addr().map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new().name("coding".into()).spawn(move || serve(listener, dir, flag, sink)).map_err(|e| e.to_string())?;
        Ok(Server { stop, addr, thread: Some(thread) })
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // `accept` wakes for a connection: this one.
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(300));
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// What the thread keeps while it runs.
struct State {
    dir: PathBuf,
    days: Vec<Day>,
    tracker: Tracker,
}

fn tracker_path(dir: &Path) -> PathBuf {
    dir.join("tracker.json")
}

fn serve(listener: TcpListener, dir: PathBuf, stop: Arc<AtomicBool>, sink: EventSink) {
    let mut state = State { days: load(&dir), tracker: load_json::<Tracker>(&tracker_path(&dir)).0, dir };
    for conn in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(mut s) = conn else { continue };
        let _ = s.set_read_timeout(Some(READ_TIMEOUT));
        let _ = s.set_write_timeout(Some(READ_TIMEOUT));
        if let Some(counted) = handle(&mut s, &mut state) {
            if counted {
                sink.send(AppEvent::Coding);
            }
        }
        let _ = s.shutdown(Shutdown::Both);
    }
}

/// One request. None when nothing could be read; Some(true) when it added
/// heartbeats.
fn handle(s: &mut TcpStream, state: &mut State) -> Option<bool> {
    let (head, mut body) = read_head(s)?;
    let mut lines = head.split("\r\n");
    let mut parts = lines.next()?.split_whitespace();
    let (method, target) = (parts.next()?.to_string(), parts.next()?.to_string());
    let headers: Vec<(String, String)> = lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string())).collect();
    let header = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());

    // Only this Mac's own programs: no web page (Origin), no other name (Host).
    let host = header("host").unwrap_or("").rsplit_once(':').map_or(header("host").unwrap_or(""), |(h, _)| h);
    if header("origin").is_some() || !(host == "127.0.0.1" || host == "localhost") {
        reply(s, 403, "Forbidden", r#"{"error":"forbidden"}"#);
        return Some(false);
    }
    if header("transfer-encoding").is_some() {
        reply(s, 411, "Length Required", r#"{"error":"length required"}"#);
        return Some(false);
    }
    let len = header("content-length").and_then(|l| l.parse::<usize>().ok()).unwrap_or(0);
    if len > MAX_BODY {
        reply(s, 413, "Payload Too Large", r#"{"error":"too large"}"#);
        return Some(false);
    }
    while body.len() < len {
        let mut chunk = [0u8; 16 * 1024];
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n.min(len - body.len())]),
        }
    }

    let path = target.split('?').next().unwrap_or("");
    match (method.as_str(), path) {
        ("POST", p) if p.ends_with("/heartbeats.bulk") || p.ends_with("/heartbeats") => {
            let bulk = p.ends_with(".bulk");
            let Ok((beats, total)) = waka::parse_beats(&String::from_utf8_lossy(&body)) else {
                reply(s, 400, "Bad Request", r#"{"error":"bad heartbeats"}"#);
                return Some(false);
            };
            let counted = !beats.is_empty();
            if counted {
                count(state, beats);
            }
            if bulk {
                reply(s, 201, "Created", &waka::bulk_response(total));
            } else {
                reply(s, 201, "Created", waka::BEAT_RESPONSE);
            }
            Some(counted)
        }
        ("GET", p) if p.ends_with("/statusbar/today") || p.ends_with("/status_bar/today") => {
            let today = crate::clock::local_now().day_key();
            let secs = state.days.iter().find(|d| d.date == today).map_or(0, |d| d.total);
            reply(s, 200, "OK", &waka::status_bar_response(secs));
            Some(false)
        }
        _ => {
            reply(s, 404, "Not Found", r#"{"error":"not found"}"#);
            Some(false)
        }
    }
}

/// Count heartbeats into the days and keep them: the days on disk (another
/// Mac's, from the repository) are joined first so nothing of theirs is lost.
fn count(state: &mut State, beats: Vec<waka::Beat>) {
    state.tracker.add_all(&mut state.days, beats, |t| crate::clock::local_at(t as i64).day_key());
    state.days = waka::union(load(&state.dir), std::mem::take(&mut state.days));
    if let Err(e) = store(&state.dir, &state.days).and_then(|()| write_json_atomic(&tracker_path(&state.dir), &state.tracker).map_err(|e| e.to_string())) {
        pwarn!("coding: couldn't keep the days ({e})");
    }
}

/// The request's head and whatever of the body came with it.
fn read_head(s: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..i]).into_owned();
            return Some((head, buf[i + 4..].to_vec()));
        }
        if buf.len() > MAX_HEAD {
            return None;
        }
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

fn reply(s: &mut TcpStream, code: u16, reason: &str, body: &str) {
    let _ = write!(s, "HTTP/1.1 {code} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
}

// ---------------------------------------------------------------- the plugins' config file

fn cfg_path() -> Option<PathBuf> {
    // The plugins look in WAKATIME_HOME first.
    let home = std::env::var_os("WAKATIME_HOME").or_else(|| std::env::var_os("HOME"))?;
    Some(PathBuf::from(home).join(".wakatime.cfg"))
}

fn backup_path(cfg: &Path) -> PathBuf {
    cfg.with_file_name(".wakatime.cfg.motchy-backup")
}

/// Do the plugins send to this Mac's app?
pub fn plugins_ready() -> bool {
    cfg_path().and_then(|p| std::fs::read_to_string(p).ok()).is_some_and(|t| waka::points_here(&t))
}

/// Point the plugins at this app: `api_url` in `~/.wakatime.cfg`. The file
/// as it was is kept once, next to it, for [`restore_plugins`].
pub fn setup_plugins() -> Result<(), String> {
    set_up(&cfg_path().ok_or("no home folder")?)
}

fn set_up(cfg: &Path) -> Result<(), String> {
    let text = match std::fs::read_to_string(cfg) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("{}: {e}", cfg.display())),
    };
    let backup = backup_path(cfg);
    if !backup.exists() {
        write_bytes_atomic(&backup, text.as_bytes()).map_err(|e| e.to_string())?;
    }
    write_bytes_atomic(cfg, waka::with_api_url(&text, &waka::local_api_url()).as_bytes()).map_err(|e| e.to_string())
}

/// Put the file back as it was before [`setup_plugins`].
pub fn restore_plugins() -> Result<(), String> {
    restore(&cfg_path().ok_or("no home folder")?)
}

fn restore(cfg: &Path) -> Result<(), String> {
    let backup = backup_path(cfg);
    let old = std::fs::read(&backup).map_err(|_| "there is no backup to restore: remove the api_url line from ~/.wakatime.cfg".to_string())?;
    write_bytes_atomic(cfg, &old).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&backup);
    Ok(())
}

// ---------------------------------------------------------------- the days

/// Every day kept in `dir` (the month files), in date order.
pub fn load(dir: &Path) -> Vec<Day> {
    let mut all = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return all };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().is_some_and(|x| x == "jsonl") {
            if let Ok(t) = std::fs::read_to_string(&p) {
                all = waka::union(all, waka::from_jsonl(&t));
            }
        }
    }
    all
}

/// Write `days` into `dir`, one file per month. A file that wouldn't change
/// isn't touched, so a day with nothing new makes no commit.
pub fn store(dir: &Path, days: &[Day]) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let mut months: Vec<String> = days.iter().map(|d| waka::month_file(&d.date)).collect();
    months.sort();
    months.dedup();
    for m in months {
        let month: Vec<Day> = days.iter().filter(|d| waka::month_file(&d.date) == m).cloned().collect();
        let text = waka::to_jsonl(&month);
        let path = dir.join(&m);
        if std::fs::read_to_string(&path).is_ok_and(|old| old == text) {
            continue;
        }
        write_bytes_atomic(&path, text.as_bytes()).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use poni_core::wakatime::Share;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("motchy-waka-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn day(date: &str, total: u32) -> Day {
        Day { date: date.into(), total, projects: vec![Share { name: "motchy".into(), secs: total }], ..Day::default() }
    }

    /// Send a raw request to the server and read the whole answer.
    fn ask(port: u16, head_lines: &[&str], body: &str, method_path: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut req = format!("{method_path} HTTP/1.1\r\n");
        for h in head_lines {
            req += &format!("{h}\r\n");
        }
        req += &format!("Content-Length: {}\r\n\r\n{body}", body.len());
        s.write_all(req.as_bytes()).unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    }

    fn sink() -> EventSink {
        EventSink::new(std::sync::mpsc::channel().0)
    }

    #[test]
    fn days_are_kept_in_a_file_per_month_and_read_back() {
        let dir = temp("files");
        let days = vec![day("2026-10-31", 60), day("2026-11-01", 120), day("2026-11-02", 30)];
        store(&dir, &days).unwrap();
        let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names, ["2026-10.jsonl", "2026-11.jsonl"]);
        assert_eq!(load(&dir), days);
        let before = std::fs::metadata(dir.join("2026-11.jsonl")).unwrap().modified().unwrap();
        std::thread::sleep(Duration::from_millis(30));
        store(&dir, &days).unwrap();
        assert_eq!(std::fs::metadata(dir.join("2026-11.jsonl")).unwrap().modified().unwrap(), before, "same days: the files are left alone");
        assert!(load(&dir.join("nothing-here")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_server_counts_what_a_plugin_sends_and_refuses_what_it_should() {
        let dir = temp("server");
        let server = Server::start(dir.clone(), 0, sink()).unwrap();
        let port = server.port();
        let t0 = 1_793_000_000.0_f64;
        let host = format!("Host: 127.0.0.1:{port}");
        let beats = format!(
            r#"[{{"time":{},"project":"motchy","language":"Rust"}},{{"time":{},"project":"motchy","language":"Rust"}},{{"time":"bad"}}]"#,
            t0,
            t0 + 100.0
        );
        // A bulk of three entries, one unreadable: three answers, two counted.
        let r = ask(port, &[&host, "Authorization: Basic eA=="], &beats, "POST /api/v1/users/current/heartbeats.bulk");
        assert!(r.starts_with("HTTP/1.1 201"), "{r}");
        assert_eq!(r.matches("201]").count(), 3, "{r}");
        let date = crate::clock::local_at(t0 as i64).day_key();
        let days = load(&dir);
        assert_eq!(days.iter().find(|d| d.date == date).map(|d| d.total), Some(100));
        // A single heartbeat, and the status bar's today.
        let one = format!(r#"{{"time":{},"project":"motchy","language":"Rust"}}"#, t0 + 150.0);
        assert!(ask(port, &[&host], &one, "POST /api/v1/users/current/heartbeats").starts_with("HTTP/1.1 201"));
        let r = ask(port, &[&host], "", "GET /api/v1/users/current/statusbar/today");
        assert!(r.starts_with("HTTP/1.1 200") && r.contains("grand_total"), "{r}");
        // A web page can't: it sends an Origin. Nor another name: DNS rebinding.
        assert!(ask(port, &[&host, "Origin: https://evil.example"], &beats, "POST /api/v1/users/current/heartbeats.bulk").starts_with("HTTP/1.1 403"));
        assert!(ask(port, &["Host: evil.example"], &beats, "POST /api/v1/users/current/heartbeats.bulk").starts_with("HTTP/1.1 403"));
        assert!(ask(port, &["Host: localhost:1"], "[]", "POST /api/v1/users/current/heartbeats.bulk").starts_with("HTTP/1.1 201"), "localhost is this Mac too");
        // Nonsense: a clear error each, and nothing counted.
        assert!(ask(port, &[&host], "nope", "POST /api/v1/users/current/heartbeats.bulk").starts_with("HTTP/1.1 400"));
        assert!(ask(port, &[&host], "", "GET /somewhere").starts_with("HTTP/1.1 404"));
        // Too big is refused from the header alone, before any of it is read.
        let mut big = TcpStream::connect(("127.0.0.1", port)).unwrap();
        big.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        write!(big, "POST /api/v1/users/current/heartbeats.bulk HTTP/1.1\r\n{host}\r\nContent-Length: {}\r\n\r\n", MAX_BODY + 1).unwrap();
        let mut answer = String::new();
        let _ = big.read_to_string(&mut answer);
        assert!(answer.starts_with("HTTP/1.1 413"), "{answer}");
        assert!(ask(port, &[&host, "Transfer-Encoding: chunked"], "", "POST /api/v1/users/current/heartbeats.bulk").starts_with("HTTP/1.1 411"));
        let after = load(&dir).iter().map(|d| d.total).sum::<u32>();
        assert_eq!(after, 150, "100 s from the bulk, 50 s from the single one");
        // Stopping frees the port.
        drop(server);
        assert!(TcpListener::bind(("127.0.0.1", port)).is_ok(), "the port is free again");
        // A new server carries on from the same last heartbeat.
        let server = Server::start(dir.clone(), 0, sink()).unwrap();
        let more = format!(r#"{{"time":{},"project":"motchy","language":"Rust"}}"#, t0 + 200.0);
        assert!(ask(server.port(), &[&format!("Host: 127.0.0.1:{}", server.port())], &more, "POST /api/v1/users/current/heartbeats").starts_with("HTTP/1.1 201"));
        assert_eq!(load(&dir).iter().map(|d| d.total).sum::<u32>(), 200);
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The real wakatime-cli, the program every editor plugin runs, sends to
    /// this server and its answer is accepted. Skipped where it isn't installed.
    #[test]
    fn the_real_wakatime_cli_is_accepted() {
        let Some(cli) = std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".wakatime/wakatime-cli")).filter(|p| p.exists()) else { return };
        let dir = temp("cli");
        let server = Server::start(dir.join("days"), 0, sink()).unwrap();
        let url = format!("http://127.0.0.1:{}/api", server.port());
        let t0 = 1_793_000_000_i64;
        // A made-up key in the shape the cli accepts, put together here so no
        // key-looking text sits in the source (secret scanners flag it).
        let fake_key = ["waka", "00000000-0000-4000-8000-000000000000"].join("_");
        // The cli skips a file that isn't on disk.
        let file = dir.join("motchy-cli-test.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();
        let run = |time: i64, project: &str| {
            std::process::Command::new(&cli)
                .args(["--entity", &file.to_string_lossy(), "--plugin", "motchy-test/1.0", "--key", &fake_key])
                .args(["--api-url", &url, "--project", project, "--language", "Rust", "--write", "--disable-offline"])
                .args(["--time", &time.to_string(), "--config", &dir.join("empty.cfg").to_string_lossy(), "--internal-config", &dir.join("internal.cfg").to_string_lossy()])
                .args(["--log-file", &dir.join("cli.log").to_string_lossy(), "--heartbeat-rate-limit-seconds", "0"])
                // Its own HOME: the cli reads AI tools' logs and an offline queue
                // from there, and must not touch this person's.
                .env("HOME", &dir)
                .env("WAKATIME_HOME", &dir)
                .output()
                .expect("runs")
        };
        let (a, b) = (run(t0, "cli-project"), run(t0 + 90, "cli-project"));
        assert!(a.status.success() && b.status.success(), "{} {}", String::from_utf8_lossy(&a.stderr), String::from_utf8_lossy(&b.stderr));
        let days = load(&dir.join("days"));
        assert_eq!(days.iter().map(|d| d.total).sum::<u32>(), 90, "{days:?}");
        assert_eq!(days[0].projects[0].name, "cli-project");
        assert_eq!(days[0].languages[0].name, "Rust");
        drop(server);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn setting_up_the_plugins_keeps_the_file_and_restoring_brings_it_back() {
        let dir = temp("cfg");
        let cfg = dir.join(".wakatime.cfg");
        let original = "[settings]\napi_key = waka_x\n; api_key = waka_old\nstatus_bar_coding_activity = true\n";
        std::fs::write(&cfg, original).unwrap();
        set_up(&cfg).unwrap();
        let now = std::fs::read_to_string(&cfg).unwrap();
        assert!(waka::points_here(&now) && now.contains("api_key = waka_x"), "{now}");
        assert_eq!(std::fs::read_to_string(backup_path(&cfg)).unwrap(), original);
        // A second set-up keeps the first backup: the original, not the changed file.
        set_up(&cfg).unwrap();
        assert_eq!(std::fs::read_to_string(backup_path(&cfg)).unwrap(), original);
        restore(&cfg).unwrap();
        assert_eq!(std::fs::read_to_string(&cfg).unwrap(), original);
        assert!(!backup_path(&cfg).exists());
        assert!(restore(&cfg).is_err(), "nothing left to restore");
        // No file at all: set-up makes one, restore leaves it empty.
        let none = dir.join("sub").join(".wakatime.cfg");
        std::fs::create_dir_all(none.parent().unwrap()).unwrap();
        set_up(&none).unwrap();
        assert!(waka::points_here(&std::fs::read_to_string(&none).unwrap()));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

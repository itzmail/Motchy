//! `poni.exe --selftest <dir>`: drive the real app through every visual state
//! with fake (original) data, save screenshots, verify pixel invariants and
//! frame timings, and write `<dir>/report.json`.
//!
//! The app calls [`SelfTest::step`] every 50 ms (`TIMER_SELFTEST`). Each step
//! re-applies the current scene (so stray OS events can't change it), waits
//! until the island's springs are at rest, then renders one frame through the
//! normal pipeline and checks it. PNG encoding runs on short-lived worker
//! threads so a step never blocks the UI thread for long.
//!
//! Output per state: `<nn>-<name>.png` (composited over a wallpaper-like
//! gradient, the way it looks on a desktop) and `<nn>-<name>-alpha.png` (the
//! raw alpha channel). The exit code is 0 when every check passes, else 1.
//!
//! PONI_SELFTEST_THEME=glass runs every scene in the Glass theme over a page
//! the self-test draws itself (no screen copy): PONI_SELFTEST_GLASS=light (a
//! light browser page, Windows in light mode), dark (a dark video page, dark
//! mode) or flip (the dark page in light mode: small glass turns dark, the
//! open island stays light). Its screenshots are composited over that page.
//!
//! PONI_SELFTEST_THEME=pill runs every scene in the Pill theme, where the
//! island floats below the top edge: its ground is checked inside the body,
//! and its own checks say the gap above the body shows the screen and the
//! shadow fades out inside the window (see `pill_checks`).
//!
//! PONI_SELFTEST_PRAYER_AT=HH:MM shows the Prayer page at that time of the
//! day (the other Prayer scenes stay 8 minutes before Ashar).
//!
//! PONI_SELFTEST_ADDONS=download also downloads the Prayer times add-on's
//! files from their release while the scenes run and checks each one
//! (`addon-download`; Windows CI only, it needs github.com).

use crate::activities::{HudTag, Mode};
use crate::app::App;
use crate::clock;
use crate::gfx::{Image, ImageData};
use crate::icons::Icon;
use crate::pages::usage::Picture as AiPicture;
use crate::pages::{self, PageKind};
use crate::paths;
use crate::png;
use crate::state::{
    AppState, CityFile, Hud, HudKind, InlineAction, Locate, LyricsState, LyricsStatus, MediaSession, Notice, NoticeAction, PowerState, PrayerState, SysStats, VolumeState,
};
use crate::ui::wid;
use poni_core::addons::{Status as AddonStatus, NOTIFICATIONS, PRAYER_TIMES, SCRIPT_READER, SYSTEM};
use poni_core::notifications::NotificationSettings;
use poni_core::geom::{palette, Color, Point, Rect};
use poni_core::lrc::{parse_lrc, Lyrics};
use poni_core::prayer::{PlaceList, World};
use poni_core::pomodoro::{Phase as PomoPhase, RunState};
use poni_core::power::PowerReading;
use poni_core::settings::{IdleStyle, Theme};
use poni_core::shelf::ShelfItem;
use poni_core::sysmon::{
    AppUse, BatteryReading, CoreKind, CoreLoad, CpuReading, DiskActivity, Drive, GpuReading, MemoryReading, Module, Monitor, NetReading, Reading,
    HISTORY_LEN,
};
use poni_core::timeline::Timeline;
use serde::Serialize;
use std::cell::Cell;
use std::fs;
use std::path::PathBuf;
use std::thread::JoinHandle;
use std::time::Instant;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowLongPtrW, GetWindowRect, IsWindowVisible, GWL_EXSTYLE, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST,
};

// ---------------------------------------------------------------- timing

/// A scene must stay settled for this many consecutive steps before capture.
const STABLE_STEPS: u32 = 2;
/// Minimum steps per scene (lets the content cross-fade start and finish).
const MIN_STEPS: u32 = 3;
/// Seconds a scene may take to settle before it counts as a failure.
const SETTLE_TIMEOUT: f64 = 3.0;
/// The same on a software rasterizer (CI runners have no GPU). A fresh
/// virtual machine can take longer to bring a scene to rest: the first open
/// page with Japanese lyrics took 3.1-4.2 s there. Still short enough to
/// catch an island that never settles.
const SOFT_GPU_SETTLE_TIMEOUT: f64 = 8.0;
/// Seconds to wait for a simulated tab click to switch the page.
const CLICK_TIMEOUT: f64 = 1.5;
/// Stop visiting scenes after this many seconds. The 89 scenes take about
/// 45 s under Wine at 100 % (a busy machine is the slow end); on CI the Oyen
/// theme with Japanese lyrics is the slowest run, about 63 s with 85 (it
/// reached the old 50 s at scene 80 once the Today scenes came in, and 65 s
/// was tight for four more). With 121 (Notifications and the AI tab
/// together, 28 Oct) the Classic run passed 70 s at scene 119. The CI and
/// Store test steps wait 180 s for the whole run (this plus the writers'
/// 3.5 s and the Classic runs' notes turn of about
/// 5 s, which the deadline leaves out, or 40 s more for the animation runs,
/// stays under it), the Wine runner 180 s.
const SCENE_DEADLINE: f64 = 90.0;
/// Give up waiting for PNG writers at this point: 3.5 s after the scene
/// deadline, so the last scenes' pictures get their time however long the
/// run took. (It was a fixed 43.5 s, left behind when SCENE_DEADLINE went
/// from 40 to 50; once the Glass runs took over 43.5 s, the last picture was
/// still encoding and png-files failed.)
const WRITE_DEADLINE: f64 = SCENE_DEADLINE + 3.5;
/// One frame (layout + Direct2D render + present + readback) must fit in a 60 Hz frame.
/// On a software rasterizer (CI runners) frame times are reported, not
/// counted: they land on whole screen refreshes (about 16 or 31 ms) on a CPU
/// shared with other machines, which says little about a real PC.
const FRAME_BUDGET_MS: f64 = 16.0;
/// Frames re-rendered when the first sample misses the budget (filters scheduler noise).
const FRAME_SAMPLES: usize = 3;
/// The Glass theme on Microsoft's software rasterizer (CI runners have no
/// GPU): the glass shader runs on the CPU there, so a busy runner can push a
/// tall page past one 60 Hz frame. Two frames, and more tries, before it is
/// reported as slow. On a real GPU the glass pass takes well under a
/// millisecond, and the 60 Hz budget holds.
const GLASS_SOFT_GPU_BUDGET_MS: f64 = 32.0;
const GLASS_FRAME_SAMPLES: usize = 6;

// Eye-blink timing of the idle "eyes" style (activities.rs). Only used to
// avoid capturing a mid-blink screenshot; harmless if it drifts.
const BLINK_PERIOD: f64 = 5.5;
const BLINK_GUARD: (f64, f64) = (0.3, 0.3);

// ---------------------------------------------------------------- fake data (all original)

const TITLE: &str = "Paper Boats";
const ARTIST: &str = "The Lanterns";
const ALBUM: &str = "Harbor Lights";
const TRACK_LEN_S: f64 = 201.0;
const TRACK_POS_S: f64 = 64.0;
/// Pomodoro time left in the running focus session (18:24).
const TIMER_LEFT_S: f64 = 1104.5;
const ART_PX: u32 = 240;
/// The battery in the low-battery scene (Windows warns at 20 %).
const LOW_BATTERY: u8 = 19;

/// Original placeholder lyrics written for the self-test (not from any real song).
const LYRICS_LRC: &str = "\
[ti:Paper Boats]
[ar:The Lanterns]
[00:38.00]Folded up the morning into something small
[00:43.50]Set it on the water by the harbor wall
[00:49.00]Ink along the edges of a name I used to know
[00:54.50]Watched it find the current, watched it go
[01:00.00]Paper boats, carry what I couldn't say
[01:05.50]Over the grey and out across the bay
[01:11.00]If the tide is gentle they will find the light
[01:16.50]Lanterns on the pier keep burning through the night
[01:22.00]Every little sail a letter never sent
[01:27.50]Every little ripple something someone meant
[01:33.00]So I fold another and I let it drift away
[01:38.50]Paper boats, I'll meet you there someday
";

const NOTES_TXT: &str = "Weekend trip\n\n- ferry leaves at 9:40\n- pack the blue jacket\n- ask about the lighthouse tour\n";

const GIB: u64 = 1024 * 1024 * 1024;

/// Paths of the sample apps (keys for their icons only; never opened) and their tile colours.
const SAMPLE_ICONS: [(&str, u32); 8] = [
    ("C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe", 0x1A9FD9),
    ("C:\\Users\\Sample\\AppData\\Roaming\\Spotify\\Spotify.exe", 0x1DB954),
    ("C:\\Windows\\explorer.exe", 0xF2C14E),
    ("C:\\Users\\Sample\\AppData\\Local\\Programs\\Microsoft VS Code\\Code.exe", 0x2F80ED),
    ("C:\\Windows\\System32\\dwm.exe", 0x00A4EF),
    ("C:\\Users\\Sample\\AppData\\Local\\Discord\\app-1.0.9200\\Discord.exe", 0x5865F2),
    ("C:\\ProgramData\\Microsoft\\Windows Defender\\Platform\\4.18.25010.11-0\\MsMpEng.exe", 0x0078D4),
    ("C:\\Program Files\\Microsoft Office\\root\\Office16\\WINWORD.EXE", 0x2B579A),
];

/// A plain rounded tile standing in for an app icon, laid out like the
/// shell's icon pictures (the tile fills the middle 72 %).
fn sample_icon(rgb: u32) -> Image {
    const PX: u32 = 48;
    let (r, g, b) = ((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8);
    let inset = PX as f32 * 0.14;
    let (lo, hi, radius) = (inset, PX as f32 - inset, PX as f32 * 0.16);
    let mut px = vec![0u8; (PX * PX * 4) as usize];
    for y in 0..PX {
        for x in 0..PX {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // Distance outside the rounded square, smoothed over one pixel.
            let dx = (lo + radius - fx).max(fx - (hi - radius)).max(0.0);
            let dy = (lo + radius - fy).max(fy - (hi - radius)).max(0.0);
            let a = (radius + 0.5 - (dx * dx + dy * dy).sqrt()).clamp(0.0, 1.0);
            let i = ((y * PX + x) * 4) as usize;
            let k = |c: u8| (f32::from(c) * a).round() as u8;
            px[i..i + 4].copy_from_slice(&[k(b), k(g), k(r), (a * 255.0).round() as u8]);
        }
    }
    ImageData::new(PX, PX, px)
}

fn sample_app(i: usize, name: &str, cpu: f32, memory: u64, io: f64) -> AppUse {
    let path = SAMPLE_ICONS[i].0;
    let image = path.rsplit('\\').next().unwrap_or(path).to_lowercase();
    AppUse { image, name: name.into(), path: Some(path.into()), pid: 1000 + i as u64, cpu, memory, io, processes: 1 }
}

/// The kernel's own entry: no file, so the page shows Windows' mark.
fn sample_system(cpu: f32, memory: u64, io: f64) -> AppUse {
    AppUse { image: "system".into(), name: "System".into(), path: None, pid: 4, cpu, memory, io, processes: 1 }
}

/// The System page's sample minute (all made up): a laptop with a hybrid
/// CPU, or a desktop without a battery (with its power: from the power
/// supply, or estimated); nothing yet when the page has just opened.
fn fake_system(scene: Scene) -> Monitor {
    let mut m = Monitor::default();
    if scene == Scene::SystemWaiting {
        return m;
    }
    const MIB: f64 = 1024.0 * 1024.0;
    for i in 0..HISTORY_LEN {
        let t = i as f32;
        let last = i + 1 == HISTORY_LEN;
        let spike = if (38..45).contains(&i) { 0.22 * ((t - 38.0) / 6.0 * std::f32::consts::PI).sin() } else { 0.0 };
        let cpu = if last { 0.23 } else { (0.19 + 0.05 * (t / 4.3).sin() + 0.03 * (t / 1.7 + 1.1).sin() + spike).clamp(0.03, 0.95) };
        let gpu = if last { 0.31 } else { (0.24 + 0.08 * (t / 5.1 + 0.4).sin() + 0.04 * (t / 2.3).sin() + if (18..30).contains(&i) { 0.3 } else { 0.0 }).clamp(0.02, 0.98) };
        let burst = |at: usize, len: usize, peak: f64| if (at..at + len).contains(&i) { peak * (((i - at) as f64 + 0.5) / len as f64 * std::f64::consts::PI).sin() } else { 0.0 };
        let read = if last { 1.2 * MIB } else { 0.15 * MIB + burst(12, 6, 6.5 * MIB) + burst(41, 4, 2.8 * MIB) };
        let write = if last { 0.33 * MIB } else { 0.05 * MIB + burst(20, 9, 1.9 * MIB) };
        let down = if last { 1.2 * MIB } else { 0.08 * MIB + burst(5, 14, 3.4 * MIB) + burst(46, 10, 1.6 * MIB) };
        let up = if last { 0.082 * MIB } else { 0.02 * MIB + burst(25, 8, 0.6 * MIB) };
        let sample = Sample { cpu, gpu, mem: 0.6 + 0.004 * (t / 6.0).sin(), read, write, down, up, t };
        let desktop = matches!(scene, Scene::SystemMemory | Scene::SystemPowerEstimate);
        let mut r = sample_reading(!desktop, &sample);
        if scene == Scene::SystemPowerEstimate {
            r.power = PowerReading::estimate(r.cpu.as_ref().and_then(|c| c.power_w), r.gpu.as_ref().and_then(|g| g.power_w), true);
        }
        m.push(r);
    }
    m
}

/// One second of the sample minute.
struct Sample {
    cpu: f32,
    gpu: f32,
    mem: f32,
    read: f64,
    write: f64,
    down: f64,
    up: f64,
    /// Seconds into the minute.
    t: f32,
}

fn sample_reading(laptop: bool, s: &Sample) -> Reading {
    let total = GIB * 16;
    const MIB: u64 = 1024 * 1024;
    // Six performance cores with two threads each, then eight efficiency cores.
    let cores = (0..20)
        .map(|k| {
            let p = k < 12;
            let wave = 0.5 + 0.5 * (s.t / 3.0 + k as f32 * 1.7).sin();
            let load = if p { s.cpu * (0.6 + 1.1 * wave) } else { s.cpu * (0.3 + 0.5 * wave) };
            CoreLoad { load: load.clamp(0.02, 1.0), kind: if p { CoreKind::Performance } else { CoreKind::Efficiency } }
        })
        .collect();
    Reading {
        cpu: Some(CpuReading {
            load: s.cpu,
            user: s.cpu * 0.62,
            kernel: s.cpu * 0.38,
            interrupt: s.cpu * 0.03,
            cores,
            logical: 20,
            base_mhz: Some(2_600),
            mhz: Some(3_400),
            mhz_p: Some(3_900),
            mhz_e: Some(2_800),
            temp_c: laptop.then_some(54.0),
            // Package power follows the load (a laptop's chip is a smaller one).
            power_w: Some(if laptop { 18.5 } else { 34.0 + 165.0 * s.cpu }),
            uptime_s: 3 * 86_400 + 4 * 3600 + 12 * 60,
        }),
        gpu: Some(GpuReading {
            name: "NVIDIA GeForce RTX 4060 Laptop GPU".into(),
            util: s.gpu,
            engine_3d: s.gpu,
            copy: 0.04,
            video_decode: 0.12,
            video_encode: 0.0,
            tiler: 0.0,
            mem_used: GIB * 21 / 10,
            mem_total: GIB * 8,
            integrated: false,
            temp_c: Some(61.0),
            fan_rpm: Some(2_150),
            mhz: Some(1_890),
            power_w: Some(12.0 + 95.0 * s.gpu),
        }),
        memory: Some(MemoryReading {
            total,
            used: (total as f64 * f64::from(s.mem)) as u64,
            committed: GIB * 121 / 10,
            commit_limit: GIB * 184 / 10,
            cached: GIB * 42 / 10,
            wired: GIB * 9 / 10,
            compressed: GIB * 6 / 10,
            free: GIB * 22 / 10,
            page_used: GIB * 3 / 10,
            page_total: GIB * 24 / 10,
        }),
        drives: vec![
            Drive { root: "C:".into(), label: "Windows".into(), total: GIB * 476, free: GIB * 212, system: true },
            Drive { root: "D:".into(), label: "Data".into(), total: GIB * 931, free: GIB * 640, system: false },
        ],
        disk: Some(DiskActivity { read_bps: s.read, write_bps: s.write, active: Some(0.14) }),
        network: Some(NetReading {
            online: true,
            adapter: "Wi-Fi".into(),
            ipv4: Some("192.168.1.42".into()),
            mac: "3C:22:FB:0A:11:9E".into(),
            link_bps: 866_700_000,
            down_bps: s.down,
            up_bps: s.up,
            received: GIB * 34 / 10,
            sent: 410 * MIB,
        }),
        battery: laptop.then(|| BatteryReading {
            percent: 76,
            secs_left: Some(2 * 3600 + 14 * 60),
            design_mwh: Some(57_000),
            full_mwh: Some(51_870),
            cycles: Some(193),
            rate_mw: Some(-8_200),
            ..BatteryReading::default()
        }),
        power: None,
        top_cpu: vec![
            sample_app(0, "Microsoft Edge", 0.091, GIB * 21 / 10, 1.2e6),
            sample_app(1, "Spotify", 0.042, 412 * MIB, 4.1e5),
            sample_system(0.023, 0, 8.2e5),
            sample_app(4, "Desktop Window Manager", 0.018, 88 * MIB, 0.0),
            sample_app(2, "Windows Explorer", 0.013, 96 * MIB, 4.0e4),
            sample_app(5, "Discord", 0.009, 520 * MIB, 1.2e4),
            sample_app(3, "Visual Studio Code", 0.008, 940 * MIB, 9.6e4),
            sample_app(6, "Antimalware Service Executable", 0.004, 280 * MIB, 3.1e6),
        ],
        top_memory: vec![
            sample_app(0, "Microsoft Edge", 0.091, GIB * 21 / 10, 1.2e6),
            sample_app(3, "Visual Studio Code", 0.008, 940 * MIB, 9.6e4),
            sample_app(5, "Discord", 0.009, 520 * MIB, 1.2e4),
            sample_app(1, "Spotify", 0.042, 412 * MIB, 4.1e5),
            sample_app(6, "Antimalware Service Executable", 0.004, 280 * MIB, 3.1e6),
            sample_app(7, "Microsoft Word", 0.002, 210 * MIB, 3.0e3),
            sample_app(2, "Windows Explorer", 0.013, 96 * MIB, 4.0e4),
            sample_app(4, "Desktop Window Manager", 0.018, 88 * MIB, 0.0),
        ],
        top_io: vec![
            sample_app(6, "Antimalware Service Executable", 0.004, 280 * MIB, 3.1e6),
            sample_app(0, "Microsoft Edge", 0.091, GIB * 21 / 10, 1.2e6),
            sample_system(0.023, 0, 8.2e5),
            sample_app(1, "Spotify", 0.042, 412 * MIB, 4.1e5),
            sample_app(3, "Visual Studio Code", 0.008, 940 * MIB, 9.6e4),
            sample_app(2, "Windows Explorer", 0.013, 96 * MIB, 4.0e4),
            sample_app(5, "Discord", 0.009, 520 * MIB, 1.2e4),
            sample_app(7, "Microsoft Word", 0.002, 210 * MIB, 3.0e3),
        ],
    }
}

/// Judge one reading from the real sources: the core readings must be
/// there; the rest is reported (a build server has no battery or GPU counters).
fn system_readings(p: crate::services::sysmon::Probe) -> (bool, String) {
    use poni_core::sysmon::fmt_bytes;
    let r = &p.reading;
    let cpu = r.cpu.as_ref().filter(|c| (0.0..=1.0).contains(&c.load) && c.logical > 0);
    let mem = r.memory.as_ref().filter(|m| m.total > 0 && m.used <= m.total);
    let ok = cpu.is_some() && mem.is_some() && !r.drives.is_empty() && !r.top_memory.is_empty();
    let apps: Vec<String> = r.top_memory.iter().take(3).map(|a| format!("{} ({}{})", a.name, fmt_bytes(a.memory), if a.path.is_some() { ", path" } else { "" })).collect();
    let cores = cpu.map_or("missing".into(), |c| match c.hybrid() {
        Some((p, e)) => format!("{} ({p} P + {e} E)", c.cores.len()),
        None if c.cores.is_empty() => "total only".into(),
        None => c.cores.len().to_string(),
    });
    let ms = |d: std::time::Duration| format!("{:.1} ms", d.as_secs_f64() * 1000.0);
    let detail = format!(
        "cpu {} · cores {} · temperature {} · package power {} · gpu {} · memory {} · drives {} · disk activity {} · network {} · battery {} · top memory {} · light reading {} · first full {} · full {} ({}) · light reading CPU {}",
        cpu.map_or("missing".into(), |c| format!("{:.0}% of {} threads{}", c.load * 100.0, c.logical, c.mhz.map_or(String::new(), |m| format!(" at {m} MHz")))),
        cores,
        cpu.and_then(|c| c.temp_c).map_or("not reported".into(), |t| format!("{t:.0} °C")),
        cpu.and_then(|c| c.power_w).map_or("not reported".into(), |w| format!("{w:.1} W")),
        r.gpu.as_ref().map_or("none".into(), |g| format!("{} {:.0}% {} of {}", g.name, g.util * 100.0, fmt_bytes(g.mem_used), fmt_bytes(g.mem_total))),
        mem.map_or("missing".into(), |m| format!("{} of {} (wired {}, compressed {}, cached {}, free {})", fmt_bytes(m.used), fmt_bytes(m.total), fmt_bytes(m.wired), fmt_bytes(m.compressed), fmt_bytes(m.cached), fmt_bytes(m.free))),
        if r.drives.is_empty() { "none".into() } else { r.drives.iter().map(|d| d.root.clone()).collect::<Vec<_>>().join(" ") },
        if r.disk.is_some() { "yes" } else { "no" },
        r.network.as_ref().map_or("unavailable".into(), |n| if n.online { format!("{} {} {}", n.adapter, n.ipv4.as_deref().unwrap_or(""), poni_core::sysmon::fmt_link(n.link_bps)) } else { "offline".into() }),
        r.battery.as_ref().map_or("none".into(), |b| format!("{}%", b.percent)),
        if apps.is_empty() { "none".into() } else { apps.join(", ") },
        ms(p.light),
        ms(p.first_full),
        ms(p.full),
        p.parts.iter().map(|(name, d)| format!("{name} {}", ms(*d))).collect::<Vec<_>>().join(", "),
        match (p.light_cycles, cpu.and_then(|c| c.mhz)) {
            (Some(cycles), Some(mhz)) => format!("{cycles} cycles = {:.2} ms at {mhz} MHz", cycles as f64 / (f64::from(mhz) * 1000.0)),
            (Some(cycles), None) => format!("{cycles} cycles"),
            _ => "not measured".into(),
        },
    );
    (ok, detail)
}

fn fake_stats() -> SysStats {
    SysStats {
        valid: true,
        cpu: 0.23,
        mem_used: GIB * 96 / 10,
        mem_total: GIB * 16,
        disk_free: GIB * 212,
        disk_total: GIB * 476,
        uptime_s: 3 * 86_400 + 4 * 3600 + 12 * 60,
    }
}

/// The WhatsApp message in the Notifications scenes (made up: the self-test
/// reads no notifications).
fn sample_message() -> poni_core::notifications::Seen {
    poni_core::notifications::Seen {
        app_id: "5319275A.WhatsAppDesktop_cv1g1gvanyjgm!App".into(),
        app: "WhatsApp".into(),
        lines: vec!["Mom".into(), "Dinner at 7 tonight?".into()],
    }
}

/// The Notifications scenes: what Windows answered, and the apps the page
/// lists (WhatsApp and Telegram on, Phone Link switched off by hand, Outlook
/// as every other app starts: off). Every other scene has the add-on's
/// choices as they start.
fn seed_notifications(st: &mut AppState, scene: Scene) {
    use crate::state::NotifyAccess;
    st.notify_access = match scene {
        Scene::NotifyAsk => NotifyAccess::Ask,
        Scene::NotifyNoPackage => NotifyAccess::NoPackage,
        Scene::Page(PageKind::Notifications) | Scene::MessageNotice | Scene::MessageHidden => NotifyAccess::Allowed,
        _ => NotifyAccess::Unknown,
    };
    let n = &mut st.settings.notifications;
    *n = NotificationSettings::default();
    if scene == Scene::Page(PageKind::Notifications) {
        for (id, name) in [
            ("5319275A.WhatsAppDesktop_cv1g1gvanyjgm!App", "WhatsApp"),
            ("Telegram.TelegramDesktop", "Telegram"),
            ("Microsoft.YourPhone_8wekyb3d8bbwe!App", "Phone Link"),
            ("Microsoft.Office.OUTLOOK.EXE.15", "Outlook"),
        ] {
            n.remember(id, name);
        }
        n.set("Microsoft.YourPhone_8wekyb3d8bbwe!App", false);
    }
}

/// The AI tab's scenes on made-up readings, every glide at its end
/// (`pages::usage::picture`); every other scene: nothing read yet and the
/// notices off, as on a first start (nothing reads it there, and no key
/// typed in one scene stays for the next).
fn seed_ai(st: &mut AppState, scene: Scene, now: f64) {
    if let Scene::Ai(p) = scene {
        pages::usage::picture(st, p, now);
    } else {
        st.usage = crate::state::UsageState::default();
        st.settings.usage = poni_core::settings::UsageSettings::default();
        st.ui.usage = pages::usage::UsageUi::default();
    }
}

/// Prayer times for its scenes: on, in Kota Jakarta (Edinburgh, picked by
/// hand, for `PrayerWorld`), its clock 8 minutes before today's Ashar (the
/// real lists arrive with the add-on's download; these few places are
/// copied from them).
fn seed_prayer(st: &mut AppState, scene: Scene, until: f64) {
    let on = matches!(
        scene,
        Scene::Page(PageKind::Prayer) | Scene::PrayerPicker | Scene::PrayerCountdown | Scene::PrayerNotice | Scene::PrayerPickerWorld | Scene::PrayerWorld | Scene::PrayerOk
    );
    if !on {
        st.prayer = PrayerState::default();
        return;
    }
    st.addons.insert(PRAYER_TIMES.id, AddonStatus::Ready);
    if st.prayer.places.is_none() {
        let list = PlaceList::parse(SAMPLE_PLACES).expect("sample places");
        st.prayer.places = Some(std::sync::Arc::new(list));
    }
    let world = World::parse(poni_core::prayer::world::SAMPLE.into()).expect("sample world places");
    let list = st.prayer.places.clone().expect("places");
    st.prayer.place = if scene == Scene::PrayerWorld { world.place(0) } else { list.find("jakarta").cloned() };
    st.settings.prayer.world_place = st.prayer.place.as_ref().and_then(|p| p.world_lines.clone());
    // The website's "ed" file, as if it had come.
    st.prayer.cities = [("ed".to_string(), CityFile::Here(std::sync::Arc::new(world)))].into();
    st.prayer.found = None;
    st.settings.prayer.follow_location = scene != Scene::PrayerWorld;
    st.prayer.suggested = false;
    st.prayer.locate = Locate::Found { km: 2.0 };
    st.prayer.located_at = Some(st.unix);
    st.prayer.picking = matches!(scene, Scene::PrayerPicker | Scene::PrayerPickerWorld);
    st.prayer.query = match scene {
        Scene::PrayerPicker => "band".into(),
        Scene::PrayerPickerWorld => "edin".into(),
        _ => String::new(),
    };
    let site = st.prayer.site().expect("the place's site");
    let today = site.local_day(st.unix);
    let ashar = poni_core::prayer::day_times(&site, today).expect("today's times").ashar;
    // PONI_SELFTEST_PRAYER_AT=HH:MM: the Prayer page at that time of
    // Jakarta's day instead (pictures of the night side: the moon on the arc).
    let at = std::env::var("PONI_SELFTEST_PRAYER_AT").ok().filter(|_| scene == Scene::Page(PageKind::Prayer)).and_then(|v| {
        let (h, m) = v.trim().split_once(':')?;
        Some(h.parse::<i64>().ok()? * 3600 + m.parse::<i64>().ok()? * 60)
    });
    st.prayer.clock_shift = match at {
        Some(secs) => site.midnight(today) + secs - st.unix,
        None => ashar - 8 * 60 - st.unix,
    };
    st.prayer.handled_until = Some(st.prayer_now());
    st.prayer.adzan = scene == Scene::PrayerNotice;
    if scene == Scene::PrayerNotice {
        let place = st.prayer.place.as_ref().map_or(String::new(), |p| p.label.clone());
        st.notice = Some(Notice {
            icon: Icon::Moon,
            color: st.accent,
            title: crate::pages::prayer::moment_title(poni_core::prayer::Prayer::Ashar),
            title_dim: String::new(),
            subtitle: format!("{} \u{b7} {place}", site.hhmm(ashar)),
            until,
            action: Some(NoticeAction::OpenPage(PageKind::Prayer)),
            inline: Some(InlineAction::StopAdzan),
            reveal: None,
        });
    }
}

const SAMPLE_PLACES: &str = r#"{"places":[
    {"id":"jakarta","label":"Kota Jakarta","kind":"kota","prov":"DKI Jakarta","lat":-6.1833,"lng":106.8333,"tz":"Asia/Jakarta"},
    {"id":"banda-aceh","label":"Kota Banda Aceh","kind":"kota","prov":"Aceh","lat":5.55,"lng":95.3167,"tz":"Asia/Jakarta"},
    {"id":"bandar-lampung","label":"Kota Bandar Lampung","kind":"kota","prov":"Lampung","lat":-5.4167,"lng":105.2667,"tz":"Asia/Jakarta"},
    {"id":"kab-bandung","label":"Kab. Bandung","kind":"kab","prov":"Jawa Barat","lat":-7.0167,"lng":107.5333,"tz":"Asia/Jakarta","h0":-2},
    {"id":"bandung","label":"Kota Bandung","kind":"kota","prov":"Jawa Barat","lat":-6.9,"lng":107.6167,"tz":"Asia/Jakarta","h0":-2},
    {"id":"kab-bandung-barat","label":"Kab. Bandung Barat","kind":"kab","prov":"Jawa Barat","lat":-6.85,"lng":107.5167,"tz":"Asia/Jakarta","h0":-2},
    {"id":"makkah","label":"Makkah","kind":"abroad","prov":"Arab Saudi","lat":21.4225,"lng":39.8261,"tz":"Asia/Riyadh","method":"ummalqura"}
]}"#;

fn fake_power(plugged: bool) -> PowerState {
    PowerState { has_battery: true, percent: 76, charging: plugged, on_ac: plugged, saver: false }
}

// ---------------------------------------------------------------- scenes

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scene {
    IdleClock,
    IdleEyes,
    Media,
    Timer,
    MediaTimer,
    VolumeHud,
    ChargingHud,
    BluetoothHud,
    Page(PageKind),
    /// The shelf while files are dragged over the island.
    ShelfDrop,
    /// An empty shelf: the drop zone.
    ShelfEmpty,
    /// Click the first header tab on the Today page; expect the Music page.
    TabClick,
    /// Click the gear in the header; expect Settings.
    GearClick,
    /// Settings on Settings page.
    /// System page on a desktop (no battery): Memory and its apps.
    SystemMemory,
    /// System page: the drives and disk activity.
    SystemDrives,
    /// System page: the network.
    SystemNetwork,
    /// System page: the laptop's battery.
    SystemBattery,
    /// System page just opened: no reading yet.
    SystemWaiting,
    /// Settings with Stay on top chosen for full-screen apps.
    SettingsStayOnTop,
    /// System page: the graphics card.
    SystemGpu,
    /// Add-ons: Prayer times downloading.
    AddonsDownloading,
    /// Add-ons: Prayer times on, its files in place.
    AddonsOn,
    /// Add-ons: switched on without internet.
    AddonsOffline,
    /// Add-ons: a file came back wrong.
    AddonsFailed,
    /// Prayer times: the place picker, "band" typed.
    PrayerPicker,
    /// Prayer times: the collapsed countdown, 8 minutes before Ashar.
    PrayerCountdown,
    /// Prayer times: Ashar's time came, the adzan playing.
    PrayerNotice,
    /// Prayer times: the place picker, "edin" typed (cities abroad).
    PrayerPickerWorld,
    /// Prayer times in Edinburgh, picked by hand (a city abroad, its method beside its name).
    PrayerWorld,
    /// The idle pill in the status bar look (the Pill theme's; the other
    /// themes show the clock).
    IdleStatus,
    /// Music playing while a synced lyric line is sung (the Pill theme shows
    /// the line on the closed island).
    MediaLyric,
    /// The battery ran low: the pop-up, not charging.
    LowBatteryHud,
    /// The Script page partway through a reading (paused, so the shot holds still).
    ScriptReading,
    /// The same reading with Oyenza Studio Recorder installed: Record shows.
    ScriptRecorder,
    /// System page on a desktop, Power: the estimate.
    SystemPowerEstimate,
    /// A maximized window under the island, Step aside on (no longer offered): the
    /// clock waits out of the way, an invisible strip at the top edge.
    AsideIdle,
    /// ...and while a song sings its lines.
    AsideLyric,
    /// ...a new song's title still drops down for its few seconds.
    AsidePeek,
    /// ...and so does the volume pop-up.
    AsideHud,
    /// Settings with Step aside off (no row for it since 0.2.6).
    SettingsAsideStay,
    /// A new version is out: the island tells once, with Update.
    UpdateNotice,
    /// The first start after an update: "Updated to", with What's new.
    UpdatedNotice,
    /// Settings while a new version is offered: Update to it in the version's row.
    SettingsUpdate,
    /// The Update sheet (the notice or the menu's "Update to" line opens it).
    UpdateSheet,
    /// The Update sheet once Windows has the Setup: Install now, or on next start.
    UpdateReady,
    /// A recording with Oyenza Studio Recorder: the Record page while it
    /// records (`Page(Record)` shows its choices), then the closed island's
    /// recording pill, recording and paused.
    RecordLive,
    RecordActivity,
    RecordPaused,
    /// The Add-ons page with the recorder installed: Screen recorder's row
    /// (eight rows with Notifications, a line each, the window no taller).
    AddonsRecorder,
    /// Prayer times: the countdown 8 minutes before Ashar over a song with
    /// synced lyrics, its OK pressed (`Phase::PressOk`): the song's line again.
    PrayerOk,
    /// The Notifications add-on: a WhatsApp message on the closed island...
    MessageNotice,
    /// ...with "Message text" switched off: it waits for Show.
    MessageHidden,
    /// The Notifications page before Windows was asked: Allow, no apps yet.
    NotifyAsk,
    /// The Notifications page in the download from GitHub, which can't read them.
    NotifyNoPackage,
    /// The AI tab on made-up readings, as the page's picture shows it
    /// (`pages::usage::picture`): its plans, troubles, nothing found, ...
    Ai(AiPicture),
    /// The AI tab's heads-up notice: Claude Code close to its 5-hour limit.
    AiNotice,
    /// ...and Codex out of it.
    AiNoticeOut,
    /// Settings with Today and Tasks taken off the tab row: their chips
    /// struck through, the header's row without them.
    SettingsTabs,
    /// The Add-ons page.
    AddonsPage,
}

/// A later release than this build, as the update file describes one.
fn fake_release() -> poni_core::update::Release {
    let mut parts: Vec<u64> = crate::version::VERSION.split('.').filter_map(|p| p.parse().ok()).collect();
    if let Some(last) = parts.last_mut() {
        *last += 1;
    }
    let version = parts.iter().map(u64::to_string).collect::<Vec<_>>().join(".");
    let repo = "https://github.com/lucacadalora/oyenza-studio/releases";
    poni_core::update::Release {
        date: "2026-10-26".into(),
        headline: "A smoother island, and fixes".into(),
        notes: format!("{repo}/tag/v{version}"),
        windows: Some(poni_core::update::Download { url: format!("{repo}/download/v{version}/Oyenza-Studio-Setup-{version}.exe"), size: 2_500_000, sha256: "0".repeat(64) }),
        mac: None,
        version,
    }
}

/// The AI tab's notice in its scenes, worded by the real alerts from two
/// readings of a 5-hour limit that resets in 2 h 20 min, on the app's clock:
/// Claude Code from 70 to 90 %, or Codex from 70 % to all used.
fn ai_alert(st: &AppState, out: bool) -> Option<studio_usage::alerts::Alert> {
    use studio_usage::{alerts, ProviderId, Snapshot, Source, Window, WindowKind};
    let id = if out { ProviderId::Codex } else { ProviderId::Claude };
    let read = |used| Snapshot {
        windows: vec![Window::new(WindowKind::Session, "5-hour", used).resets(Some(st.unix + 2 * 3600 + 20 * 60)).length(Some(18_000))],
        ..Snapshot::new(id, st.unix, Source::Online)
    };
    alerts::changes(Some(&read(0.7)), &read(if out { 1.0 } else { 0.9 }), st.unix, &st.usage_alert_cfg()).into_iter().next()
}

/// Running under Wine: its ntdll exports `wine_get_version`.
fn under_wine() -> bool {
    use windows::core::{s, w};
    use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
    unsafe { GetModuleHandleW(w!("ntdll.dll")).is_ok_and(|h| GetProcAddress(h, s!("wine_get_version")).is_some()) }
}

/// What the Script page reads in its self-test scenes.
const SAMPLE_SCRIPT: &str = "Hi everyone, this one takes two minutes. Today I'll show you the Shelf, and how to send a file to your phone.\n\n\
Drop any file on the island and it waits on the Shelf. Press the phone button, scan the code, and the file is on your phone. \
No cable, no account, nothing to install on the phone.\n\nThat's it. Thanks for watching.";

const SCENES: [Scene; 74] = [
    Scene::IdleClock,
    Scene::IdleEyes,
    Scene::Media,
    Scene::Timer,
    Scene::MediaTimer,
    Scene::VolumeHud,
    Scene::ChargingHud,
    Scene::BluetoothHud,
    Scene::Page(PageKind::Media),
    Scene::Page(PageKind::Focus),
    Scene::Page(PageKind::Shelf),
    Scene::ShelfDrop,
    // Last of the shelf scenes: hiding the items resets their fade-in.
    Scene::ShelfEmpty,
    Scene::Page(PageKind::Tasks),
    Scene::Page(PageKind::Today),
    Scene::TabClick,
    Scene::GearClick,
    Scene::Page(PageKind::System),
    Scene::SystemMemory,
    Scene::SystemDrives,
    Scene::SystemNetwork,
    Scene::SystemBattery,
    Scene::SystemWaiting,
    Scene::SettingsStayOnTop,
    // Appended so the numbers of the screenshots above stay the same. They
    // show an empty shelf: items shown again after ShelfEmpty would fade in.
    Scene::SystemGpu,
    // The Add-ons page: off (the "+" tab), then each state of a download.
    Scene::Page(PageKind::Addons),
    Scene::AddonsDownloading,
    Scene::AddonsOn,
    Scene::AddonsOffline,
    Scene::AddonsFailed,
    // Prayer times on, its clock 8 minutes before today's Ashar in Jakarta.
    Scene::Page(PageKind::Prayer),
    Scene::PrayerPicker,
    Scene::PrayerCountdown,
    Scene::PrayerNotice,
    Scene::PrayerPickerWorld,
    Scene::PrayerWorld,
    // Appended so the numbers above stay. Every theme runs them; the Pill
    // theme draws the first two its own way.
    Scene::IdleStatus,
    Scene::MediaLyric,
    Scene::LowBatteryHud,
    // The Script page: empty, then partway through a reading.
    Scene::Page(PageKind::Script),
    Scene::ScriptReading,
    // ...and with the recorder installed (Record by Paste).
    Scene::ScriptRecorder,
    // The whole PC's power: read from the power supply, then estimated.
    Scene::SystemPowerEstimate,
    // A maximized window under the island: what rests steps aside, what
    // happens still shows. Then the Settings page with Stay picked.
    Scene::AsideIdle,
    Scene::AsideLyric,
    Scene::AsidePeek,
    Scene::AsideHud,
    Scene::SettingsAsideStay,
    // New versions: the island's notice, "Updated to", Settings offering
    // one, and the Update sheet before and after the Setup is in.
    Scene::UpdateNotice,
    Scene::UpdatedNotice,
    Scene::SettingsUpdate,
    Scene::UpdateSheet,
    Scene::UpdateReady,
    // Oyenza Studio Recorder installed: the Record page's choices and the
    // recording, the closed island while it records and while paused, and
    // the Add-ons page with Screen recorder's row.
    Scene::Page(PageKind::Record),
    Scene::RecordLive,
    Scene::RecordActivity,
    Scene::RecordPaused,
    Scene::AddonsRecorder,
    // The prayer countdown's OK, pressed: the closed island sings again.
    Scene::PrayerOk,
    // The Notifications add-on: a message on the closed island (shown, and
    // waiting for Show), then its page once Windows allows it, before
    // Windows was asked, and in the download from GitHub.
    Scene::MessageNotice,
    Scene::MessageHidden,
    Scene::Page(PageKind::Notifications),
    Scene::NotifyAsk,
    Scene::NotifyNoPackage,
    // The AI tab, a core tab after Focus, in every picture the page has
    // (`AiPicture::ALL`): plans, troubles, nothing found, the first look,
    // the Services list and a key being typed there.
    Scene::Ai(AiPicture::Plans),
    Scene::Ai(AiPicture::Troubles),
    Scene::Ai(AiPicture::Empty),
    Scene::Ai(AiPicture::Looking),
    Scene::Ai(AiPicture::Services),
    Scene::Ai(AiPicture::Key),
    // Its heads-up notices on the closed island.
    Scene::AiNotice,
    Scene::AiNoticeOut,
    // Settings' tabs row with two of the island's pages off the row.
    Scene::SettingsTabs,
    // The Add-ons page.
    Scene::AddonsPage,
];

impl Scene {
    fn name(self) -> String {
        match self {
            Scene::IdleClock => "idle-clock".into(),
            Scene::IdleEyes => "idle-eyes".into(),
            Scene::Media => "media-activity".into(),
            Scene::Timer => "timer-activity".into(),
            Scene::MediaTimer => "media-timer".into(),
            Scene::VolumeHud => "volume-hud".into(),
            Scene::ChargingHud => "charging-hud".into(),
            Scene::BluetoothHud => "bluetooth-hud".into(),
            Scene::Page(p) => format!("page-{}", p.key()),
            Scene::ShelfDrop => "page-shelf-drop".into(),
            Scene::ShelfEmpty => "page-shelf-empty".into(),
            Scene::TabClick => "tab-click-media".into(),
            // Reached by clicking the gear: the Settings page.
            Scene::GearClick => "page-settings".into(),
            Scene::SystemMemory => "page-system-memory".into(),
            Scene::SystemDrives => "page-system-drives".into(),
            Scene::SystemNetwork => "page-system-network".into(),
            Scene::SystemBattery => "page-system-battery".into(),
            Scene::SystemWaiting => "page-system-waiting".into(),
            Scene::SettingsStayOnTop => "page-settings-stay-on-top".into(),
            Scene::SettingsTabs => "page-settings-tabs".into(),
            Scene::SystemGpu => "page-system-gpu".into(),
            Scene::AddonsDownloading => "page-addons-downloading".into(),
            Scene::AddonsOn => "page-addons-on".into(),
            Scene::AddonsOffline => "page-addons-offline".into(),
            Scene::AddonsFailed => "page-addons-failed".into(),
            Scene::PrayerPicker => "page-prayer-picker".into(),
            Scene::PrayerCountdown => "prayer-countdown".into(),
            Scene::PrayerNotice => "prayer-notice".into(),
            Scene::PrayerPickerWorld => "page-prayer-picker-world".into(),
            Scene::PrayerWorld => "page-prayer-world".into(),
            Scene::IdleStatus => "idle-status".into(),
            Scene::MediaLyric => "media-lyric".into(),
            Scene::LowBatteryHud => "low-battery-hud".into(),
            Scene::ScriptReading => "page-script-reading".into(),
            Scene::ScriptRecorder => "page-script-recorder".into(),
            Scene::SystemPowerEstimate => "page-system-power-estimate".into(),
            Scene::AsideIdle => "aside-idle".into(),
            Scene::AsideLyric => "aside-media-lyric".into(),
            Scene::AsidePeek => "aside-new-song".into(),
            Scene::AsideHud => "aside-volume-hud".into(),
            Scene::SettingsAsideStay => "page-settings-maximized-stay".into(),
            Scene::UpdateNotice => "update-notice".into(),
            Scene::UpdatedNotice => "updated-notice".into(),
            Scene::SettingsUpdate => "page-settings-update".into(),
            Scene::UpdateSheet => "page-update-sheet".into(),
            Scene::UpdateReady => "page-update-ready".into(),
            Scene::RecordLive => "page-record-live".into(),
            Scene::RecordActivity => "record-activity".into(),
            Scene::RecordPaused => "record-paused-activity".into(),
            Scene::AddonsRecorder => "page-addons-recorder".into(),
            Scene::PrayerOk => "prayer-countdown-ok".into(),
            Scene::MessageNotice => "message-notice".into(),
            Scene::MessageHidden => "message-hidden-notice".into(),
            Scene::NotifyAsk => "page-notifications-ask".into(),
            Scene::NotifyNoPackage => "page-notifications-download".into(),
            // "page-ai", "page-ai-troubles", ...: the page's own names.
            Scene::Ai(p) => p.name().into(),
            Scene::AiNotice => "ai-notice".into(),
            Scene::AiNoticeOut => "ai-notice-out".into(),
            Scene::AddonsPage => "page-addons-page".into(),
        }
    }

    fn page(self) -> Option<PageKind> {
        match self {
            Scene::Page(p) => Some(p),
            Scene::ShelfDrop | Scene::ShelfEmpty => Some(PageKind::Shelf),
            Scene::TabClick => Some(PageKind::ALL[0]),
            Scene::GearClick
            | Scene::SettingsStayOnTop
            | Scene::SettingsTabs
            | Scene::SettingsAsideStay
            | Scene::SettingsUpdate
            | Scene::UpdateSheet
            | Scene::UpdateReady => Some(PageKind::Settings),
            Scene::SystemMemory
            | Scene::SystemDrives
            | Scene::SystemNetwork
            | Scene::SystemBattery
            | Scene::SystemWaiting
            | Scene::SystemGpu
            | Scene::SystemPowerEstimate => Some(PageKind::System),
            Scene::AddonsDownloading | Scene::AddonsOn | Scene::AddonsOffline | Scene::AddonsFailed => Some(PageKind::Addons),
            Scene::PrayerPicker | Scene::PrayerPickerWorld | Scene::PrayerWorld => Some(PageKind::Prayer),
            Scene::ScriptReading | Scene::ScriptRecorder => Some(PageKind::Script),
            Scene::RecordLive => Some(PageKind::Record),
            Scene::AddonsRecorder | Scene::AddonsPage => Some(PageKind::Addons),
            Scene::NotifyAsk | Scene::NotifyNoPackage => Some(PageKind::Notifications),
            Scene::Ai(_) => Some(PageKind::Ai),
            _ => None,
        }
    }

    /// Oyenza Studio Recorder is installed (Record by the Script page's Paste,
    /// the Record tab, Screen recorder's row).
    fn recorder(self) -> bool {
        matches!(
            self,
            Scene::ScriptRecorder | Scene::Page(PageKind::Record) | Scene::RecordLive | Scene::RecordActivity | Scene::RecordPaused | Scene::AddonsRecorder
        )
    }

    /// What the recorder last told the island: recording (0:42 on the page,
    /// 12:34 on the closed island) or paused, held still for the pictures.
    fn recording(self, now: f64) -> crate::state::RecView {
        use poni_core::recorder::Phase;
        let (phase, elapsed_ms) = match self {
            Scene::RecordLive => (Phase::Recording, 42_000),
            Scene::RecordActivity => (Phase::Recording, 754_000),
            Scene::RecordPaused => (Phase::Paused, 754_000),
            _ => return crate::state::RecView::default(),
        };
        // Sent "later" than now, so the time doesn't count on while the scene settles.
        crate::state::RecView { phase: Some(phase), elapsed_ms, at: now + 3600.0, ..Default::default() }
    }

    fn expanded(self) -> bool {
        self.page().is_some()
    }

    fn media_playing(self) -> bool {
        matches!(self, Scene::Media | Scene::MediaTimer | Scene::MediaLyric | Scene::AsideLyric | Scene::AsidePeek | Scene::PrayerOk) || self.expanded()
    }

    /// A maximized window is under the island.
    fn window_under(self) -> bool {
        matches!(self, Scene::AsideIdle | Scene::AsideLyric | Scene::AsidePeek | Scene::AsideHud)
    }

    /// The island rests stepped aside: nothing to see, a strip to point at.
    fn rests_aside(self) -> bool {
        matches!(self, Scene::AsideIdle | Scene::AsideLyric)
    }

    fn timer_running(self) -> bool {
        matches!(self, Scene::Timer | Scene::MediaTimer) || self.expanded()
    }
}



// ---------------------------------------------------------------- report

#[derive(Clone, Debug, Serialize)]
struct Check {
    name: String,
    ok: bool,
    detail: String,
}

fn check(name: &str, ok: bool, detail: impl Into<String>) -> Check {
    Check { name: name.into(), ok, detail: detail.into() }
}

#[derive(Debug, Serialize)]
struct StateReport {
    index: usize,
    name: String,
    expanded: bool,
    page: Option<&'static str>,
    image: String,
    alpha_image: String,
    ok: bool,
    settle_ms: u64,
    /// Best of the sampled frame times (full frame: layout, render, present, readback).
    frame_ms: f64,
    frame_samples_ms: Vec<f64>,
    canvas_px: [i32; 2],
    window_pos_px: [i32; 2],
    /// Island body in canvas DIPs: x, y, w, h.
    body_dip: [f32; 4],
    measured_width_px: i32,
    checks: Vec<Check>,
}

#[derive(Debug, Serialize)]
struct Report {
    app: &'static str,
    version: &'static str,
    ok: bool,
    exit_code: i32,
    total_ms: u64,
    scale: f32,
    expanded_width_dip: f32,
    /// "gpu", "gpu-warp" or "software".
    renderer: String,
    frame_budget_ms: f64,
    states_passed: usize,
    states_total: usize,
    failed_checks: Vec<String>,
    /// Frames over the budget on a software GPU (CI runners), where frame
    /// time is reported and doesn't fail the run: "scene: detail".
    slow_frames: Vec<String>,
    /// Checks that are not tied to a single screenshot (fixtures, input, files).
    checks: Vec<Check>,
    states: Vec<StateReport>,
}

// ---------------------------------------------------------------- driver

enum Phase {
    /// Record one transition frame by frame (PONI_SELFTEST_ANIM=1).
    Anim { seq: usize },
    /// Record the media pill playing, then paused (PONI_SELFTEST_LOOP=1).
    Loop,
    /// Film the lyrics moving to the next line (PONI_SELFTEST_SCROLL=<Hz>).
    Scroll,
    Settle { i: usize, since: f64, steps: u32, stable: u32 },
    Click { i: usize },
    ClickWait { i: usize, since: f64, from: PageKind, want: PageKind },
    /// After the scenes: a real click on the Settings page's last row,
    /// Advanced settings (`clicked` once pressed): the window opens and the
    /// island closes.
    Advanced { since: f64, clicked: bool },
    /// Then the Settings window's `k`-th section as a picture.
    Window { k: usize },
    /// Scene `i` shows the prayer countdown; once it rests with its OK
    /// drawn, press OK (`Scene::PrayerOk`).
    PressOk { i: usize, since: f64 },
    Finish { since: f64 },
    Done(i32),
}

pub struct SelfTest {
    dir: PathBuf,
    t0: Instant,
    phase: Phase,
    art: Option<Image>,
    art_color: Option<Color>,
    lyrics: LyricsState,
    track_key: String,
    /// Where the media-lyric scene holds the song: inside a sung line.
    lyric_pos: f64,
    states: Vec<StateReport>,
    checks: Vec<Check>,
    writes: Vec<(String, JoinHandle<Vec<String>>)>,
    scale: f32,
    expanded_width: f32,
    /// PONI_SELFTEST_DPI: render at this DPI instead of the monitor's.
    dpi: Option<u32>,
    /// PONI_SELFTEST_ANIM=1: after the scenes, record transitions frame by frame.
    anim: bool,
    /// PONI_SELFTEST_LOOP=1: after the scenes, record the media pill on a held
    /// clock, frame by frame.
    media_loop: bool,
    /// PONI_SELFTEST_SCROLL=<Hz>: after the scenes, film the lyrics moving to
    /// the next line on a held clock, one frame per refresh at that rate.
    scroll_hz: Option<f64>,
    dpi_applied: bool,
    /// PONI_SELFTEST_THEME=glass|pill: run every scene in that theme.
    theme: Theme,
    /// Glass: which page is under the island (PONI_SELFTEST_GLASS), the
    /// window size its backdrop was drawn for, and the scene last seeded.
    glass: Option<GlassRun>,
    /// The frame budget the scenes were held to (for the report).
    budget_ms: f64,
    /// Frames over that budget on a software GPU, reported and not counted.
    slow_frames: Vec<String>,
    glass_size: Cell<(i32, i32)>,
    glass_scene: Cell<Option<Scene>>,
    renderer: &'static str,
    /// The seeded shelf and its thumbnails (the empty-shelf scene hides
    /// them, and the page forgets thumbnails of items that left).
    shelf_items: Vec<ShelfItem>,
    shelf_thumbs: Vec<(String, Option<Image>)>,
    /// The real add-on download (PONI_SELFTEST_ADDONS=download), running beside the scenes.
    addon_fetch: Option<std::sync::mpsc::Receiver<(bool, String)>>,
}

/// Transitions recorded in animation mode: (name, from, to).
const ANIMS: [(&str, Scene, Scene); 5] = [
    ("expand", Scene::Media, Scene::Page(PageKind::Media)),
    ("page", Scene::Page(PageKind::Media), Scene::Page(PageKind::Focus)),
    ("collapse", Scene::Page(PageKind::Media), Scene::Media),
    ("hud", Scene::Media, Scene::VolumeHud),
    ("notice", Scene::IdleClock, Scene::UpdateNotice),
];
/// Frames recorded per transition (~16 ms apart).
const ANIM_FRAMES: usize = 40;
/// The media loop recording: seconds playing, seconds paused, frames a
/// second, and the clock step it starts on.
const LOOP_PLAY_S: f64 = 4.8;
const LOOP_PAUSE_S: f64 = 2.4;
const LOOP_FPS: f64 = 30.0;
const LOOP_ALIGN_S: f64 = 4.8;
const LOOP_PREROLL_S: f64 = 2.4;
/// The lyrics scroll film (PONI_SELFTEST_SCROLL): where the track starts
/// (the next line starts 0.5 s in), unsaved lead-in, and the filmed length.
const SCROLL_FROM_S: f64 = 65.0;
const SCROLL_PREROLL_S: f64 = 1.0;
const SCROLL_FILM_S: f64 = 1.3;

impl SelfTest {
    pub fn new(dir: String) -> Self {
        let track_key = MediaSession::make_track_key(TITLE, ARTIST, ALBUM);
        Self {
            dir: PathBuf::from(dir),
            t0: Instant::now(),
            phase: Phase::Settle { i: 0, since: 0.0, steps: 0, stable: 0 },
            art: None,
            art_color: None,
            lyrics: LyricsState { track_key: track_key.clone(), ..Default::default() },
            track_key,
            lyric_pos: TRACK_POS_S,
            states: Vec::new(),
            checks: Vec::new(),
            writes: Vec::new(),
            scale: 1.0,
            expanded_width: 0.0,
            dpi: std::env::var("PONI_SELFTEST_DPI").ok().and_then(|v| v.trim().parse().ok()),
            anim: std::env::var("PONI_SELFTEST_ANIM").is_ok_and(|v| v.trim() == "1"),
            media_loop: std::env::var("PONI_SELFTEST_LOOP").is_ok_and(|v| v.trim() == "1"),
            scroll_hz: std::env::var("PONI_SELFTEST_SCROLL").ok().and_then(|v| v.trim().parse().ok()).filter(|hz: &f64| (20.0..=500.0).contains(hz)),
            dpi_applied: false,
            theme: match std::env::var("PONI_SELFTEST_THEME").map(|v| v.trim().to_ascii_lowercase()).as_deref() {
                Ok("glass") => Theme::Glass,
                Ok("pill") => Theme::Pill,
                _ => Theme::Classic,
            },
            glass: None,
            budget_ms: FRAME_BUDGET_MS,
            slow_frames: Vec::new(),
            glass_size: Cell::new((0, 0)),
            glass_scene: Cell::new(None),

            renderer: "software",
            shelf_items: Vec::new(),
            shelf_thumbs: Vec::new(),
            addon_fetch: None,
        }
    }

    /// Seed the app with fake data. Called once before the first frame.
    pub fn begin(&mut self, st: &mut AppState) {
        // The real readers behind the System page (its scenes show sample
        // readings); before the scene clock starts, as it takes a moment.
        let (ok, detail) = system_readings(crate::services::sysmon::probe());
        self.checks.push(check("system-readings", ok, detail));
        // Every picture the AI page has is a scene (the Mac renders take
        // them all from the same list), so a new one can't go unseen here.
        let missing: Vec<&str> = AiPicture::ALL.into_iter().filter(|p| !SCENES.contains(&Scene::Ai(*p))).map(AiPicture::name).collect();
        let detail = if missing.is_empty() { format!("all {} of the AI page's pictures", AiPicture::ALL.len()) } else { format!("no scene for {}", missing.join(", ")) };
        self.checks.push(check("ai-pictures", missing.is_empty(), detail));
        self.t0 = Instant::now();
        if self.theme == Theme::Glass {
            let run = GlassRun::from_env();
            crate::pinfo!("selftest: Glass theme over the {} page, Windows in {} mode", if run.dark_page() { "dark" } else { "light" }, if run.app_light() { "light" } else { "dark" });
            self.glass = Some(run);
        }
        crate::pinfo!("selftest: output folder {}", self.dir.display());
        match fs::create_dir_all(&self.dir) {
            Ok(()) => self.clear_old_outputs(),
            Err(e) => self.checks.push(check("output-folder", false, format!("{}: {e}", self.dir.display()))),
        }

        // Quiet, deterministic settings (defaults otherwise: the home folder is fresh).
        st.settings.sounds = false;
        st.settings.show_media_activity = true;
        st.settings.lyrics = true;
        st.settings.hud_volume = true;
        st.settings.hud_power = true;
        st.settings.hud_bluetooth = true;
        st.accent = palette::BLUE;
        st.pinned = false;
        st.volume = VolumeState { level: 0.64, muted: false, known: true };
        st.power = fake_power(false);
        st.stats = fake_stats();

        // Album art + lyrics.
        let art = ImageData::new(ART_PX, ART_PX, make_art(ART_PX));
        self.art_color = Some(art.accent_color());
        self.art = Some(art);
        // PONI_SELFTEST_LRC=<file>: a real song for a one-off check on CI
        // (fetched there at run time; song lyrics never go in the repo).
        let file = std::env::var("PONI_SELFTEST_LRC").ok().and_then(|p| fs::read_to_string(p.trim()).ok());
        let lrc = file.as_deref().unwrap_or(LYRICS_LRC);
        let parsed = parse_lrc(lrc);
        if file.is_none() {
            let lyric_ok = parsed.synced && parsed.lines.len() == 12 && parsed.line_index_at(TRACK_POS_S) == Some(4);
            self.checks.push(check(
                "lyrics-fixture",
                lyric_ok,
                format!("{} lines, synced {}, line at {TRACK_POS_S}s = {:?}", parsed.lines.len(), parsed.synced, parsed.line_index_at(TRACK_POS_S)),
            ));
        }
        if let Some(pos) = lyric_scene_pos(&parsed) {
            self.lyric_pos = pos;
        }
        self.lyrics = LyricsState { track_key: self.track_key.clone(), status: LyricsStatus::Found, lyrics: Some(parsed), source: "Sample".into() };

        seed_tasks(st);
        let today = st.day_key();
        st.store.focus_sessions.insert(today, 2);
        self.seed_shelf(st);

        // Read-only: the Store build must reach the startup task its manifest declares.
        let (ok, detail) = match crate::autostart::describe() {
            Ok(d) => (true, d),
            Err(d) => (false, d),
        };
        self.checks.push(check("start-with-windows", ok, detail));
        // Notifications: the download says it needs the Store copy; inside
        // the package Windows' listener must answer (CI's Store job checks).
        let (ok, detail) = match crate::services::notifications::describe() {
            Ok(d) => (true, d),
            Err(d) => (false, d),
        };
        self.checks.push(check("notifications", ok, detail));

        // PONI_SELFTEST_ADDONS=download (Windows CI): fetch every add-on's files
        // from their releases and check each against the size and SHA-256 built in.
        if std::env::var("PONI_SELFTEST_ADDONS").is_ok_and(|v| v.trim() == "download") {
            let (tx, rx) = std::sync::mpsc::channel();
            let dir = paths::addons_dir().join("selftest-download");
            let _ = std::thread::Builder::new().name("selftest-addon".into()).spawn(move || {
                let (mut ok, mut said) = (true, Vec::new());
                for a in poni_core::addons::ALL {
                    // An add-on whose files aren't on their release yet has
                    // nothing to check; its switch says so (`Status::Unpinned`).
                    if !a.pinned() {
                        said.push(format!("{}: not pinned yet", a.id));
                        continue;
                    }
                    let (fine, detail) = crate::services::addons::fetch_for_selftest(a, &dir);
                    ok &= fine;
                    said.push(format!("{}: {detail}", a.id));
                }
                let _ = tx.send((ok, said.join("; ")));
            });
            self.addon_fetch = Some(rx);
        }
    }

    /// Called every ~50 ms; returns Some(exit code) when finished.
    pub fn step(&mut self, app: &mut App) -> Option<i32> {
        if !self.dpi_applied {
            self.dpi_applied = true;
            if let Some(dpi) = self.dpi {
                crate::pinfo!("selftest: rendering at {dpi} DPI ({:.0} %)", dpi as f32 / 96.0 * 100.0);
                app.selftest_set_dpi(dpi);
            }
        }
        self.scale = app.scale();
        self.expanded_width = app.st.settings.expanded_width;
        self.renderer = app.renderer();
        let now = clock::mono();
        let elapsed = self.t0.elapsed().as_secs_f64();
        if elapsed > SCENE_DEADLINE
            && !matches!(self.phase, Phase::Finish { .. } | Phase::Done(_) | Phase::Anim { .. } | Phase::Loop | Phase::Scroll)
        {
            let at = match &self.phase {
                Phase::Settle { i, .. } | Phase::Click { i } | Phase::ClickWait { i, .. } => SCENES[*i].name(),
                _ => String::new(),
            };
            crate::pwarn!("selftest: deadline reached while at '{at}'");
            self.checks.push(check("deadline", false, format!("stopped after {elapsed:.1} s at '{at}'")));
            self.phase = Phase::Finish { since: now };
        }

        match self.phase {
            Phase::Anim { seq } => {
                self.record_anim(app, seq);
                self.phase = if seq + 1 < ANIMS.len() {
                    Phase::Anim { seq: seq + 1 }
                } else if self.media_loop {
                    Phase::Loop
                } else if self.scroll_hz.is_some() {
                    Phase::Scroll
                } else {
                    Phase::Finish { since: now }
                };
            }
            Phase::Loop => {
                self.record_loop(app);
                self.phase = if self.scroll_hz.is_some() { Phase::Scroll } else { Phase::Finish { since: clock::mono() } };
            }
            Phase::Scroll => {
                if let Some(hz) = self.scroll_hz {
                    self.record_scroll(app, hz);
                }
                self.phase = Phase::Finish { since: clock::mono() };
            }
            Phase::Settle { i, since, steps, stable } => {
                let scene = SCENES[i];
                let since = if steps == 0 { now } else { since };
                self.apply(app, scene);
                let settled = app.selftest_settled();
                let stable = if settled { stable + 1 } else { 0 };
                let steps = steps + 1;
                let waited = now - since;
                let timed_out = waited > self.settle_limit(app);
                // No picture mid-blink.
                let blinking = scene == Scene::IdleEyes && mid_blink();
                let ready = steps >= MIN_STEPS && stable >= STABLE_STEPS && !blinking;
                if ready || timed_out {
                    self.capture(app, i, !timed_out || settled, waited);
                    self.phase = self.next_phase(i + 1, now);
                } else {
                    self.phase = Phase::Settle { i, since, steps, stable };
                }
            }
            Phase::Click { i } => {
                let from = app.st.page;
                // The first tab, or the gear (found by its widget id: its place
                // depends on how wide the clock and date are).
                let (what, at, want) = match SCENES[i] {
                    Scene::GearClick => ("the gear", app.selftest_hit_center(wid(pages::GEAR_ID)), PageKind::Settings),
                    _ => ("the first header tab", Some(first_tab_point(app.island_body())), PageKind::ALL[0]),
                };
                match at {
                    Some(p) => {
                        crate::pinfo!("selftest: clicking {what} at ({:.0}, {:.0}) DIP on page '{}'", p.x, p.y, from.key());
                        app.selftest_click(p);
                    }
                    None => crate::pwarn!("selftest: {what} isn't on page '{}'", from.key()),
                }
                self.phase = Phase::ClickWait { i, since: now, from, want };
            }
            Phase::ClickWait { i, since, from, want } => {
                let page = app.st.page;
                if page != from || now - since > CLICK_TIMEOUT {
                    let ok = from != want && page == want;
                    let name = if SCENES[i] == Scene::GearClick { "gear-click" } else { "tab-click" };
                    self.checks.push(check(
                        name,
                        ok,
                        format!("page '{}' -> '{}' after {:.0} ms (expected '{}')", from.key(), page.key(), (now - since) * 1000.0, want.key()),
                    ));
                    if !ok {
                        crate::pwarn!("selftest: {name} did not switch to '{}' (page is '{}')", want.key(), page.key());
                    }
                    self.phase = Phase::Settle { i, since: now, steps: 0, stable: 0 };
                }
            }
            Phase::Advanced { since, clicked: false } => {
                // The Settings page on the open island, then a real click on
                // its last row once the island has settled.
                self.apply(app, Scene::Page(PageKind::Settings));
                let at = app.selftest_hit_center(wid(pages::settings::ADVANCED_ID)).filter(|_| app.selftest_settled());
                match at {
                    Some(p) => {
                        crate::pinfo!("selftest: clicking Advanced settings at ({:.0}, {:.0}) DIP", p.x, p.y);
                        app.selftest_click(p);
                        self.phase = Phase::Advanced { since: now, clicked: true };
                    }
                    None if now - since > self.settle_limit(app) => {
                        self.checks.push(check("advanced-settings", false, "no Advanced settings row on the Settings page"));
                        self.phase = Phase::Window { k: 0 };
                    }
                    None => {}
                }
            }
            Phase::Advanced { since, clicked: true } => {
                // The window opens and the island closes, so it never sits over the window.
                let opened = app.settings_window_open();
                if opened || now - since > CLICK_TIMEOUT {
                    let closed = !app.selftest_expanded();
                    let ok = opened && closed;
                    let detail = format!(
                        "the Settings window {} after {:.0} ms; the island {}",
                        if opened { "opened" } else { "didn't open" },
                        (now - since) * 1000.0,
                        if closed { "closed" } else { "stayed open" }
                    );
                    if !ok {
                        crate::pwarn!("selftest: Advanced settings: {detail}");
                    }
                    self.checks.push(check("advanced-settings", ok, detail));
                    self.phase = Phase::Window { k: 0 };
                }
            }
            Phase::Window { k } => {
                // Each of the Settings window's sections, as a picture: dark,
                // then General and Appearance light.
                let mut list: Vec<_> = crate::settings_window::sections(&app.st).into_iter().map(|s| (s, false)).collect();
                list.extend([(crate::settings_window::Section::General, true), (crate::settings_window::Section::Appearance, true)]);
                match list.get(k) {
                    Some(&(section, light)) => {
                        let (w, h, px) = app.selftest_settings_shot(section, light);
                        let name = format!("settings-window-{}{}", section.key(), if light { "-light" } else { "" });
                        let sized = w > 0 && h > 0 && px.len() == (w as usize) * (h as usize) * 4;
                        // The card is opaque where the sidebar meets the content.
                        let solid = sized && {
                            let s = app.selftest_settings_scale();
                            let at = |x: f32, y: f32| px[(((y * s) as usize) * w as usize + (x * s) as usize) * 4 + 3];
                            let card = app.selftest_settings_card();
                            at(card.center().x, card.center().y) == 255 && at(1.0, 1.0) == 0
                        };
                        self.checks.push(check(&name, solid, format!("{w}x{h} px{}", if solid { "" } else { ", card not drawn" })));
                        if sized {
                            let scale = app.selftest_settings_scale();
                            self.queue_write(&name, w, h, px, scale, None);
                        }
                        // Frames of a glide redraw the whole window: what one costs.
                        if k == 0 {
                            let median = |ms: &[f64]| ms.get(ms.len() / 2).copied().unwrap_or(f64::INFINITY);
                            let full = app.selftest_settings_frames(30, None);
                            // A glide's frame: the sidebar's highlight going a few rows down.
                            let card = app.selftest_settings_card();
                            let part = app.selftest_settings_frames(30, Some(Rect::new(card.x + 10.0, card.y + 80.0, 200.0, 140.0)));
                            let detail = format!(
                                "whole window {:.2} ms (best {:.2}, worst {:.2}), a glide's frame {:.2} ms (worst {:.2}), {} frames each at {:.0} %; 144 Hz leaves 6.94 ms a frame",
                                median(&full),
                                full.first().copied().unwrap_or(0.0),
                                full.last().copied().unwrap_or(0.0),
                                median(&part),
                                part.last().copied().unwrap_or(0.0),
                                full.len(),
                                app.selftest_settings_scale() * 100.0
                            );
                            self.checks.push(check("settings-window-frame", median(&full) < SETTINGS_FRAME_BUDGET_MS, detail));
                            // A person using it, filmed: what the glides' frames
                            // drawn in part show must be what drawing it all shows.
                            // PONI_SELFTEST_FILM keeps every frame as the screen shows
                            // it (film-settings.bgra: premultiplied BGRA, one frame after
                            // another) and the part each drew (film-settings.json).
                            let mut keep = std::env::var_os("PONI_SELFTEST_FILM").and_then(|_| fs::File::create(self.dir.join("film-settings.bgra")).ok()).map(std::io::BufWriter::new);
                            let mut parts = Vec::new();
                            let film = app.selftest_settings_film(144.0, |k, _, _, px, part| {
                                if let Some(out) = keep.as_mut() {
                                    let _ = std::io::Write::write_all(out, px);
                                    let part = part.map_or("null".to_string(), |r| format!("[{}, {}, {}, {}]", r.left, r.top, r.right, r.bottom));
                                    parts.push(format!("{{\"frame\": {k}, \"part\": {part}}}"));
                                }
                            });
                            if let (Some(mut out), Some(f)) = (keep.take(), film.as_ref()) {
                                let _ = std::io::Write::flush(&mut out);
                                let json = format!("{{\"fps\": 144, \"width\": {}, \"height\": {}, \"frames\": [\n  {}\n]}}\n", f.size.0, f.size.1, parts.join(",\n  "));
                                if let Err(e) = fs::write(self.dir.join("film-settings.json"), json) {
                                    crate::pwarn!("selftest: writing film-settings.json failed: {e}");
                                }
                            }
                            let (ok, detail) = match film {
                                Some(f) => {
                                    let ms = &f.partial_ms;
                                    let worst = match f.worst {
                                        Some((k, n, r)) => format!("; worst frame {k}: {n} pixels in ({}, {})-({}, {})", r.left, r.top, r.right, r.bottom),
                                        None => String::new(),
                                    };
                                    let missing = if f.missing.is_empty() { String::new() } else { format!("; not found: {}", f.missing.join(", ")) };
                                    let ok = f.missing.is_empty() && f.differing.is_empty() && ms.len() >= 60;
                                    // The frames that differ, as runs ("24-30, 151").
                                    let mut runs: Vec<(usize, usize)> = Vec::new();
                                    for &k in &f.differing {
                                        match runs.last_mut() {
                                            Some(r) if r.1 + 1 == k => r.1 = k,
                                            _ => runs.push((k, k)),
                                        }
                                    }
                                    let at = runs.iter().map(|&(a, b)| if a == b { a.to_string() } else { format!("{a}-{b}") }).collect::<Vec<_>>().join(", ");
                                    let detail = format!(
                                        "{} frames at 144 Hz ({}x{} px), {} of them a glide's drawn in part: {:.2} ms (worst {:.2}); {} frames differ from the window drawn whole{}{worst}{missing}",
                                        f.frames,
                                        f.size.0,
                                        f.size.1,
                                        ms.len(),
                                        median(ms),
                                        ms.last().copied().unwrap_or(0.0),
                                        f.differing.len(),
                                        if at.is_empty() { String::new() } else { format!(" ({at})") }
                                    );
                                    (ok, detail)
                                }
                                None => (false, "its windows didn't open".to_string()),
                            };
                            self.checks.push(check("settings-window-glide", ok, detail));
                        }
                        self.phase = Phase::Window { k: k + 1 };
                    }
                    None => {
                        app.selftest_close_settings();
                        self.phase = self.after_window(now);
                    }
                }
            }
            Phase::PressOk { i, since } => {
                // The countdown first, as a person sees it, then a real press
                // on its OK: the island stays closed and the countdown goes.
                self.apply(app, SCENES[i]);
                let at = app.selftest_hit_center(wid(crate::activities::PRAYER_OK_ID)).filter(|_| app.selftest_settled());
                if at.is_some() || now - since > self.settle_limit(app) {
                    let (opened, said) = match at {
                        Some(p) => {
                            crate::pinfo!("selftest: pressing the prayer countdown's OK at ({:.0}, {:.0}) DIP", p.x, p.y);
                            let opened = app.selftest_press(p);
                            (opened, format!("pressed at ({:.0}, {:.0}) DIP, the island {}", p.x, p.y, if opened { "opened" } else { "stayed closed" }))
                        }
                        None => (false, "no OK on the closed island".to_string()),
                    };
                    let away = crate::activities::prayer_countdown(&app.st).is_none();
                    let ok = at.is_some() && !opened && away;
                    self.checks.push(check("prayer-ok", ok, format!("{said}; the countdown {}", if away { "put away" } else { "still there" })));
                    self.phase = Phase::Settle { i, since: now, steps: 0, stable: 0 };
                }
            }
            Phase::Finish { since } => {
                let all_done = self.writes.iter().all(|(_, h)| h.is_finished());
                let deadline = if self.anim || self.media_loop || self.scroll_hz.is_some() { WRITE_DEADLINE + 40.0 } else { WRITE_DEADLINE };
                if all_done || elapsed > deadline {
                    let code = self.finish(all_done, now - since);
                    self.phase = Phase::Done(code);
                    return Some(code);
                }
            }
            Phase::Done(code) => return Some(code),
        }
        None
    }

    fn next_phase(&self, i: usize, now: f64) -> Phase {
        match SCENES.get(i) {
            Some(Scene::TabClick | Scene::GearClick) => Phase::Click { i },
            Some(Scene::PrayerOk) => Phase::PressOk { i, since: now },
            Some(_) => Phase::Settle { i, since: now, steps: 0, stable: 0 },
            // Advanced settings opening the Settings window, then its
            // sections (not over the Glass backdrops).
            None if self.glass.is_none() => Phase::Advanced { since: now, clicked: false },
            None => self.after_window(now),
        }
    }

    /// After the scenes and the Settings window's pictures.
    fn after_window(&self, now: f64) -> Phase {
        self.after_scenes(now)
    }

    fn after_scenes(&self, now: f64) -> Phase {
        if self.anim {
            Phase::Anim { seq: 0 }
        } else if self.media_loop {
            Phase::Loop
        } else if self.scroll_hz.is_some() {
            Phase::Scroll
        } else {
            Phase::Finish { since: now }
        }
    }

    /// Settle the `from` state, switch to `to`, and save every frame of the
    /// transition as anim-<name>-NN.png. Blocks the UI thread for ~1-3 s,
    /// which is fine in self-test mode.
    fn record_anim(&mut self, app: &mut App, seq: usize) {
        let (name, from, to) = ANIMS[seq];
        let t = Instant::now();
        loop {
            self.apply(app, from);
            let _ = app.selftest_capture();
            if (app.selftest_settled() && t.elapsed().as_millis() > 200) || t.elapsed().as_secs_f64() > 2.5 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        self.apply(app, to);
        let mut settle_frame = None;
        // Where each frame sat on screen, to check that the island's centre holds still.
        let mut placed = Vec::with_capacity(ANIM_FRAMES);
        for k in 0..ANIM_FRAMES {
            let (w, h, px, at) = app.selftest_capture();
            if settle_frame.is_none() && app.selftest_settled() {
                settle_frame = Some(k);
            }
            placed.push(format!("{{\"frame\": {k}, \"x\": {}, \"y\": {}, \"w\": {w}, \"h\": {h}}}", at.0, at.1));
            let scale = self.scale;
            self.queue_write(&format!("anim-{name}-{k:02}"), w, h, px, scale, None);
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        let json = format!("{{\"name\": \"{name}\", \"scale\": {}, \"frames\": [\n  {}\n]}}\n", self.scale, placed.join(",\n  "));
        if let Err(e) = fs::write(self.dir.join(format!("anim-{name}.json")), json) {
            crate::pwarn!("selftest: writing anim-{name}.json failed: {e}");
        }
        crate::pinfo!("selftest: anim '{name}' recorded, settled at frame {settle_frame:?}");
    }

    /// The media pill on a held clock: LOOP_PLAY_S playing, then LOOP_PAUSE_S
    /// paused, LOOP_FPS frames a second, saved as loop-media-NNN.png. The
    /// clock starts on a whole LOOP_ALIGN_S, so every build films the same
    /// phase of the music, after LOOP_PREROLL_S of unsaved frames (whatever
    /// builds up while music plays is already there on the first frame).
    fn record_loop(&mut self, app: &mut App) {
        let t = Instant::now();
        loop {
            self.apply(app, Scene::Media);
            let _ = app.selftest_capture();
            if (app.selftest_settled() && t.elapsed().as_millis() > 200) || t.elapsed().as_secs_f64() > 2.5 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        let start = ((clock::mono() + LOOP_PREROLL_S + 1.0) / LOOP_ALIGN_S).ceil() * LOOP_ALIGN_S;
        let paused_at = start + LOOP_PLAY_S;
        let frames = ((LOOP_PLAY_S + LOOP_PAUSE_S) * LOOP_FPS).round() as usize;
        let preroll = (LOOP_PREROLL_S * LOOP_FPS).round() as usize;
        let mut placed = Vec::with_capacity(frames);
        for i in 0..preroll + frames {
            let now = start + (i as f64 - preroll as f64) / LOOP_FPS;
            clock::hold(Some(now));
            self.apply(app, Scene::Media);
            let playing = now < paused_at;
            if playing {
                app.st.media_paused_at = None;
            } else {
                if let Some(m) = app.st.media.as_mut() {
                    m.timeline = Timeline::new(TRACK_POS_S, TRACK_LEN_S, now, false);
                }
                app.st.media_paused_at = Some(paused_at);
            }
            let (w, h, px, at) = app.selftest_capture();
            let Some(k) = i.checked_sub(preroll) else { continue };
            placed.push(format!("{{\"frame\": {k}, \"t\": {:.4}, \"playing\": {playing}, \"x\": {}, \"y\": {}, \"w\": {w}, \"h\": {h}}}", now - start, at.0, at.1));
            let scale = self.scale;
            self.queue_write(&format!("loop-media-{k:03}"), w, h, px, scale, None);
        }
        clock::hold(None);
        let json = format!("{{\"name\": \"media\", \"fps\": {LOOP_FPS}, \"scale\": {}, \"frames\": [\n  {}\n]}}\n", self.scale, placed.join(",\n  "));
        if let Err(e) = fs::write(self.dir.join("loop-media.json"), json) {
            crate::pwarn!("selftest: writing loop-media.json failed: {e}");
        }
        crate::pinfo!("selftest: media loop recorded, {frames} frames");
    }

    /// The Music page on a held clock while the lyrics move on to the next
    /// line ("Over the grey...", at 01:05.50), one frame per refresh at `hz`,
    /// after SCROLL_PREROLL_S of unsaved frames. Saves the lyrics column (grey)
    /// as scroll-lyrics-NNN.png and scroll-lyrics.json; qa tooling measures
    /// from them how evenly the lines move.
    fn record_scroll(&mut self, app: &mut App, hz: f64) {
        let t = Instant::now();
        loop {
            self.apply(app, Scene::Page(PageKind::Media));
            let _ = app.selftest_capture();
            if (app.selftest_settled() && t.elapsed().as_millis() > 200) || t.elapsed().as_secs_f64() > 2.5 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        let start = clock::mono() + SCROLL_PREROLL_S + 1.0;
        let pre = (SCROLL_PREROLL_S * hz).round() as usize;
        let frames = (SCROLL_FILM_S * hz).round() as usize;
        let mut placed = Vec::with_capacity(frames);
        for i in 0..pre + frames {
            let now = start + (i as f64 - pre as f64) / hz;
            clock::hold(Some(now));
            self.apply(app, Scene::Page(PageKind::Media));
            if let Some(m) = app.st.media.as_mut() {
                // Sampled at `now`: a timeline sampled later doesn't run backwards.
                m.timeline = Timeline::new(SCROLL_FROM_S + (now - start), TRACK_LEN_S, now, true);
            }
            let (w, h, px, _) = app.selftest_capture();
            let Some(k) = i.checked_sub(pre) else { continue };
            // The right half of the page body: the lyrics column.
            let s = self.scale;
            let body = app.island_body_in_capture();
            let x0 = ((body.x + body.w * 0.5) * s).round().clamp(0.0, w as f32) as usize;
            let x1 = (body.right() * s).round().clamp(0.0, w as f32) as usize;
            let y0 = ((body.y + pages::HEADER_H) * s).round().clamp(0.0, h as f32) as usize;
            let y1 = ((body.bottom() - pages::PAD) * s).round().clamp(0.0, h as f32) as usize;
            if x1 <= x0 || y1 <= y0 || px.len() < (w * h * 4) as usize {
                continue;
            }
            let mut gray = Vec::with_capacity((x1 - x0) * (y1 - y0));
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = &px[(y * w as usize + x) * 4..][..4];
                    gray.push(p[0].max(p[1]).max(p[2]));
                }
            }
            placed.push(format!("{{\"frame\": {k}, \"t\": {:.5}, \"w\": {}, \"h\": {}}}", now - start, x1 - x0, y1 - y0));
            let path = self.dir.join(format!("scroll-lyrics-{k:03}.png"));
            let (gw, gh) = ((x1 - x0) as u32, (y1 - y0) as u32);
            if let Err(e) = png::save_gray(&path, gw, gh, &gray) {
                crate::pwarn!("selftest: {e}");
            }
        }
        clock::hold(None);
        let json = format!("{{\"name\": \"lyrics\", \"hz\": {hz}, \"scale\": {}, \"frames\": [\n  {}\n]}}\n", self.scale, placed.join(",\n  "));
        if let Err(e) = fs::write(self.dir.join("scroll-lyrics.json"), json) {
            crate::pwarn!("selftest: writing scroll-lyrics.json failed: {e}");
        }
        crate::pinfo!("selftest: lyrics scroll recorded, {frames} frames at {hz} Hz");
    }

    // ------------------------------------------------------------ scene state

    /// Put the app into `scene`. Idempotent: called on every step and right
    /// before each capture so clocks (track position, countdown) are pinned.
    fn apply(&self, app: &mut App, scene: Scene) {
        let now = clock::mono();
        let st = &mut app.st;
        st.settings.idle_style = match scene {
            Scene::IdleEyes => IdleStyle::Eyes,
            Scene::IdleStatus => IdleStyle::Status,
            _ => IdleStyle::Clock,
        };
        if st.settings.theme != self.theme {
            st.settings.theme = self.theme;
            st.accent = palette::BLUE;
        }
        if let Some(run) = self.glass {
            // What's under the window: drawn again whenever the window grows.
            let (_, _, w, h) = app.selftest_surface();
            if w > 0 && h > 0 && self.glass_size.get() != (w, h) {
                let px = glass_backdrop(run.dark_page(), w, h, app.scale());
                app.selftest_glass_backdrop(px, w, h, run.app_light());
                self.glass_size.set((w, h));
            }
            if self.glass_scene.get() != Some(scene) {
                self.glass_scene.set(Some(scene));
                app.selftest_glass_reseed();
            }
        }
        let st = &mut app.st;
        st.power = fake_power(scene == Scene::ChargingHud);
        if scene == Scene::LowBatteryHud {
            st.power.percent = LOW_BATTERY;
        }
        st.stats = fake_stats();
        st.volume = VolumeState { level: 0.64, muted: false, known: true };
        st.pinned = false;
        st.drag_hover = scene == Scene::ShelfDrop;
        // Start with Windows as Settings shows it once read (it's read off the UI thread).
        st.ui.autostart = Some(true);
        // Hide in full-screen apps (the default), except in the Stay on top scene.
        st.settings.hide_in_fullscreen = scene != Scene::SettingsStayOnTop;
        // Every tab in the row, except in the scene that hides two.
        st.settings.hidden_tabs = if scene == Scene::SettingsTabs { vec!["today".into(), "tasks".into()] } else { Vec::new() };
        // Step aside is off in the app since 0.2.6; the self-test keeps drawing what's left of it.
        st.settings.step_aside = scene != Scene::SettingsAsideStay;
        // A later version offered, only where Settings shows it. A copy from
        // GitHub reads the update file itself, so Settings has Check now.
        let looks = crate::state::UpdateView { looks: true, ..Default::default() };
        st.update = match scene {
            Scene::SettingsUpdate | Scene::UpdateSheet | Scene::UpdateReady => crate::state::UpdateView {
                offer: Some(fake_release()),
                sheet: scene != Scene::SettingsUpdate,
                ready: scene == Scene::UpdateReady,
                ..looks
            },
            _ => looks,
        };
        // A new song's title shows for its few seconds (held here).
        st.ui.peek_until = if scene == Scene::AsidePeek { now + 30.0 } else { 0.0 };
        let empty_shelf = scene == Scene::ShelfEmpty;
        st.store.shelf.items = if empty_shelf { Vec::new() } else { self.shelf_items.clone() };
        if !empty_shelf {
            for (path, thumb) in &self.shelf_thumbs {
                st.shelf_thumbs.entry(path.clone()).or_insert_with(|| thumb.clone());
            }
        }

        if scene.media_playing() {
            let pos = if matches!(scene, Scene::MediaLyric | Scene::AsideLyric | Scene::PrayerOk) { self.lyric_pos } else { TRACK_POS_S };
            let timeline = Timeline::new(pos, TRACK_LEN_S, now, true);
            match st.media.as_mut() {
                Some(m) if m.track_key == self.track_key => m.timeline = timeline,
                _ => st.media = Some(self.media_session(timeline)),
            }
            if st.lyrics.track_key != self.track_key || st.lyrics.status != LyricsStatus::Found {
                st.lyrics = self.lyrics.clone();
            }
        } else {
            st.media = None;
            st.lyrics = LyricsState::default();
        }

        st.pomodoro.phase = PomoPhase::Focus;
        st.pomodoro.cycle_done = 1;
        st.pomodoro.run = if scene.timer_running() { RunState::Running { ends_at: now + TIMER_LEFT_S } } else { RunState::Idle };

        let until = now + 30.0;
        st.hud = match scene {
            Scene::VolumeHud | Scene::AsideHud => Some(Hud { kind: HudKind::Volume { level: 0.64, muted: false }, until }),
            Scene::ChargingHud => Some(Hud { kind: HudKind::Charging { percent: 76, plugged: true }, until }),
            Scene::BluetoothHud => {
                Some(Hud { kind: HudKind::Bluetooth { name: "Sound Buds".into(), connected: true, battery: Some(80), audio: true }, until })
            }
            Scene::LowBatteryHud => Some(Hud { kind: HudKind::LowBattery { percent: LOW_BATTERY }, until }),
            _ => None,
        };
        st.notice = match scene {
            Scene::UpdateNotice => Some(crate::updates::new_version_notice(&fake_release(), st.accent, until)),
            Scene::UpdatedNotice => Some(crate::updates::updated_notice(Some(fake_release().headline), until)),
            Scene::MessageNotice | Scene::MessageHidden => {
                let choices = NotificationSettings { text: scene == Scene::MessageNotice, ..Default::default() };
                crate::pages::notifications::message_notice(&sample_message(), &choices, st.accent, now).map(|n| Notice { until, ..n })
            }
            Scene::AiNotice | Scene::AiNoticeOut => ai_alert(st, scene == Scene::AiNoticeOut).map(|a| crate::state::usage_notice(&a, st.accent, until)),
            _ => None,
        };
        // Add-ons: what the page shows (settings stay as they are, so the worker downloads nothing).
        let addon = match scene {
            Scene::AddonsDownloading => AddonStatus::Downloading { done: 1_300_000, total: PRAYER_TIMES.size() },
            Scene::AddonsOn => AddonStatus::Ready,
            Scene::AddonsOffline => AddonStatus::Waiting,
            Scene::AddonsFailed => AddonStatus::Failed,
            _ => AddonStatus::Off,
        };
        st.addons.insert(PRAYER_TIMES.id, addon);
        // Script reader: on for the Script page's scenes, off (no tab) elsewhere.
        let script = scene.page() == Some(PageKind::Script);
        st.addons.insert(SCRIPT_READER.id, if script { AddonStatus::Ready } else { AddonStatus::Off });
        // System (an add-on, off by default): on for its own pages' scenes only.
        let system = scene.page() == Some(PageKind::System);
        // Notifications (Windows, off by default): on for its page and its notices.
        let notify = scene.page() == Some(PageKind::Notifications) || matches!(scene, Scene::MessageNotice | Scene::MessageHidden);
        for (id, on) in [(SYSTEM.id, system), (NOTIFICATIONS.id, notify)] {
            st.settings.set_addon(id, on);
            st.addons.insert(id, if on { AddonStatus::Ready } else { AddonStatus::Off });
        }
        seed_notifications(st, scene);
        seed_prayer(st, scene, until);
        // The Script page: empty, or partway through the sample script.
        st.script = poni_core::script::Reader::default();
        if matches!(scene, Scene::ScriptReading | Scene::ScriptRecorder) {
            st.store.script.text = SAMPLE_SCRIPT.into();
            st.script.word = 22;
            st.script.active = true;
        } else {
            st.store.script.text.clear();
        }
        st.recorder = scene.recorder();
        st.store.recorder_island = scene.recorder() && scene != Scene::ScriptRecorder;
        st.rec = scene.recording(now);
        st.settings.record = poni_core::settings::RecordSettings::default();
        st.system = fake_system(scene);
        st.ui.system_module = match scene {
            Scene::SystemMemory => Module::Memory,
            Scene::SystemDrives => Module::Drives,
            Scene::SystemNetwork => Module::Network,
            Scene::SystemBattery => Module::Battery,
            Scene::SystemGpu => Module::Gpu,
            Scene::SystemPowerEstimate => Module::Power,
            _ => Module::Cpu,
        };
        for (path, rgb) in SAMPLE_ICONS {
            st.app_icons.entry(path.to_string()).or_insert_with(|| Some(sample_icon(rgb)));
        }
        seed_ai(st, scene, now);
        if let Some(p) = scene.page() {
            st.page = p;
        }
        app.selftest_window_under(scene.window_under());
        app.selftest_hover(None);
        app.selftest_set_expanded(scene.expanded());
    }

    fn media_session(&self, timeline: Timeline) -> MediaSession {
        MediaSession {
            app_id: "Spotify.exe".into(),
            app_name: "Spotify".into(),
            title: TITLE.into(),
            artist: ARTIST.into(),
            album: ALBUM.into(),
            art: self.art.clone(),
            art_color: self.art_color,
            timeline,
            can_prev: true,
            can_next: true,
            can_play_pause: true,
            can_seek: true,
            track_key: self.track_key.clone(),
        }
    }

    fn seed_shelf(&mut self, st: &mut AppState) {
        let home = paths::data_dir();
        let base = std::path::absolute(&home).unwrap_or(home).join("shelf-demo");
        let txt = base.join("Trip notes.txt");
        let img = base.join("Harbor sketch.png");
        let folder = base.join("Project assets");
        let mut errors = Vec::new();
        if let Err(e) = fs::create_dir_all(&folder) {
            errors.push(format!("{}: {e}", folder.display()));
        }
        if let Err(e) = fs::write(folder.join("readme.txt"), "Drafts and exports for the harbor poster.\n") {
            errors.push(format!("readme.txt: {e}"));
        }
        if let Err(e) = fs::write(&txt, NOTES_TXT) {
            errors.push(format!("{}: {e}", txt.display()));
        }
        if let Some(art) = &self.art {
            if let Err(e) = png::save_pbgra(&img, art.width, art.height, &art.pixels) {
                errors.push(e);
            }
        }
        let made = [&folder, &img, &txt].iter().filter(|p| p.exists()).count();
        self.checks.push(check("shelf-files", errors.is_empty() && made == 3, if errors.is_empty() { format!("{made} items in {}", base.display()) } else { errors.join("; ") }));

        // Oldest first: add_paths puts each new path at the front.
        let unix = st.unix;
        for (k, p) in [&folder, &img, &txt].into_iter().enumerate() {
            let s = p.to_string_lossy().into_owned();
            st.store.shelf.add_paths(&[s.as_str()], unix - 3600 + k as i64 * 600);
            // Pre-seeded thumbnails keep the screenshots identical on every machine
            // (the app skips LoadThumb for paths it already has an entry for).
            let thumb = if p == &img { self.art.clone() } else { None };
            self.shelf_thumbs.push((s.clone(), thumb.clone()));
            st.shelf_thumbs.insert(s, thumb);
        }
        self.shelf_items = st.store.shelf.items.clone();
    }

    // ------------------------------------------------------------ capture + checks

    /// The frame budget (ms) and how many tries a scene gets to meet it.
    fn frame_budget(&self, app: &App) -> (f64, usize) {
        if self.glass.is_some() && app.selftest_software_gpu() {
            (GLASS_SOFT_GPU_BUDGET_MS, GLASS_FRAME_SAMPLES)
        } else {
            (FRAME_BUDGET_MS, FRAME_SAMPLES)
        }
    }

    /// Seconds a scene may take to settle (longer on a software GPU).
    fn settle_limit(&self, app: &App) -> f64 {
        if app.selftest_software_gpu() {
            SOFT_GPU_SETTLE_TIMEOUT
        } else {
            SETTLE_TIMEOUT
        }
    }

    fn capture(&mut self, app: &mut App, i: usize, settled: bool, waited: f64) {
        let scene = SCENES[i];
        let name = format!("{:02}-{}", i + 1, scene.name());
        let (budget, tries) = self.frame_budget(app);
        self.budget_ms = budget;
        let mut samples = Vec::with_capacity(tries);
        let mut shot = (0, 0, Vec::new(), (0, 0));
        for _ in 0..tries {
            self.apply(app, scene);
            let t = Instant::now();
            shot = app.selftest_capture();
            let ms = t.elapsed().as_secs_f64() * 1000.0;
            samples.push(round2(ms));
            if ms < budget {
                break;
            }
        }
        let (w, h, px, present) = shot;
        let scale = app.scale();
        let body = app.island_body_in_capture();
        let frame = Frame { w, h, px: &px };
        let best = samples.iter().copied().fold(f64::INFINITY, f64::min);

        let mut checks = vec![check("settled", settled, format!("{:.0} ms (limit {:.0} ms)", waited * 1000.0, self.settle_limit(app) * 1000.0))];
        let mut measured = 0;
        if w <= 0 || h <= 0 || px.len() != (w as usize) * (h as usize) * 4 {
            checks.push(check("pixels", false, format!("{w}x{h} frame with {} bytes", px.len())));
        } else if scene.rests_aside() {
            // Nothing drawn over the window under the island: the strip it
            // rests as is there only for the pointer (alpha 1/255 at most).
            let seen = px.chunks_exact(4).filter(|p| p[3] > 1).count();
            checks.push(check("aside-invisible", seen == 0, format!("{seen} pixels drawn with alpha above 1/255")));
            let (mode, aside) = app.selftest_mode();
            // The Hidden look's strip: 150 wide, as short as the island gets (8).
            checks.push(check(
                "aside-strip",
                mode == Mode::Aside && aside && (body.w - 150.0).abs() < 0.5 && body.h <= 8.5,
                format!("{mode:?} (stepped aside: {aside}), body {:.1}x{:.1} DIP, want the Hidden look's 150x8 strip", body.w, body.h),
            ));
        } else {
            let cx = ((body.x + body.w / 2.0) * scale).floor() as i32;
            // 3 DIPs inside the body's top edge: the window's top rows where
            // the island hangs from the edge, below the gap where it floats.
            let pill = crate::pill::on(self.theme);
            let edge = if pill { body.y } else { 0.0 };
            let top = ((edge + 3.0) * scale).round() as i32;
            let [b, g, r, a] = frame.at(cx, top);
            if self.glass.is_some() {
                // Glass shows what's under it, so no one ground colour: opaque is
                // the rule, and smoked glass (no backdrop) lets the screen through.
                let copy = app.selftest_glass_state().1;
                let min = if copy { 254 } else { (poni_core::glass::SMOKED_OPACITY * 255.0) as u8 - 3 };
                checks.push(check("glass-opaque", a >= min, format!("pixel ({cx}, {top}) = rgba({r}, {g}, {b}, {a}), want alpha >= {min}")));
            } else {
                let ground = rgb8(poni_core::look::of(self.theme).ground);
                let off = [r, g, b].iter().zip(ground).map(|(&p, q)| p.abs_diff(q)).max().unwrap_or(0);
                let inside = if pill { ", 3 DIPs inside the floating body" } else { "" };
                checks.push(check(
                    "body-opaque-ground",
                    a >= 254 && off <= 6,
                    format!("pixel ({cx}, {top}) = rgba({r}, {g}, {b}, {a}), ground rgb{ground:?}{inside}"),
                ));
            }

            let corners = [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)].map(|(x, y)| frame.at(x, y)[3]);
            checks.push(check("corners-transparent", corners.iter().all(|&a| a <= 1), format!("corner alpha {corners:?}")));

            let mid = ((body.y + body.h / 2.0) * scale).floor() as i32;
            measured = frame.opaque_run(cx, mid);
            let want = body.w * scale;
            checks.push(check(
                "body-width",
                (measured as f32 - want).abs() <= 2.0,
                format!("{measured} px opaque at row {mid}, geometry {want:.1} px"),
            ));
            if scene.expanded() {
                let ew = app.st.settings.expanded_width * scale;
                checks.push(check(
                    "expanded-width",
                    (measured as f32 - ew).abs() <= 2.0,
                    format!("{measured} px vs settings.expanded_width {:.0} x {scale:.2} = {ew:.1} px", app.st.settings.expanded_width),
                ));
            }

            let look = if self.glass.is_some() { app.selftest_glass_state().2 } else { poni_core::look::of(self.theme) };
            // Glass: away from the bright rim, which isn't content.
            let area = if self.glass.is_some() { Rect::new(body.x + 10.0, body.y, (body.w - 20.0).max(1.0), (body.h - 10.0).max(1.0)) } else { body };
            let lit = frame.content_pixels(area, scale, rgb8(look.ground));
            checks.push(check("content-drawn", lit >= 20, format!("{lit} content pixels inside the island")));
            if let Some(run) = self.glass {
                self.glass_checks(app, scene, run, &px, w, h, body, scale, &mut checks);
            }
            if pill {
                pill_checks(&frame, body, scale, &mut checks);
            }
        }
        // Frame time counts on a real GPU only (see FRAME_BUDGET_MS).
        let soft_gpu = app.selftest_software_gpu();
        let why = if budget > FRAME_BUDGET_MS { ", Glass on a software GPU" } else { "" };
        let timing = format!("best {best:.2} ms of {samples:?} (budget {budget} ms{why})");
        let slow = soft_gpu && best >= budget;
        if !soft_gpu {
            checks.push(check("frame-time", best < budget, timing.clone()));
        }
        checks.push(window_check(app.hwnd(), app.selftest_surface(), present, w, h));
        if let Some(want) = scene.page() {
            let page = app.st.page;
            checks.push(check("page", page == want, format!("showing '{}', expected '{}'", page.key(), want.key())));
        }
        if scene == Scene::MediaLyric {
            checks.push(lyric_line_check(&app.st));
        }
        if scene == Scene::PrayerOk {
            // After OK: the song and its sung line until Ashar, as before the countdown.
            let (mode, _) = app.selftest_mode();
            let want = Mode::Activity(crate::activities::Activity::Media);
            checks.push(check("prayer-ok-song", mode == want, format!("{mode:?}, want {want:?}")));
            checks.push(lyric_line_check(&app.st));
        }
        if matches!(scene, Scene::AsidePeek | Scene::AsideHud) {
            // Stepped aside, and still what just happened shows over the window.
            let (mode, aside) = app.selftest_mode();
            let want = if scene == Scene::AsideHud { Mode::Hud(HudTag::Volume) } else { Mode::Activity(crate::activities::Activity::Media) };
            checks.push(check("aside-still-shows", aside && mode == want, format!("{mode:?} (stepped aside: {aside}), want {want:?}")));
        }
        if scene.page() == Some(PageKind::Script) {
            // The script stays out of screen shares and recordings: ask Windows.
            let build = crate::window::windows_build();
            let can = build.is_some_and(|b| b >= crate::glass::EXCLUDE_FROM_CAPTURE_BUILD);
            let hidden = crate::window::capture_excluded(app.hwnd());
            let said = if hidden { "left out of capture" } else { "in capture" };
            // Wine says Windows 10 but can't leave a window out of capture.
            let wine = under_wine();
            let note = if wine { " (Wine, which can't leave a window out)" } else { "" };
            checks.push(check("script-hidden-from-capture", hidden == can || (wine && !hidden), format!("display affinity: {said}, Windows build {build:?}{note}")));
        }

        if matches!(scene, Scene::RecordActivity | Scene::RecordPaused) {
            // The recording rests on the closed island, ahead of everything else.
            let (mode, _) = app.selftest_mode();
            let want = Mode::Activity(crate::activities::Activity::Recording);
            checks.push(check("recording-pill", mode == want, format!("{mode:?}, want {want:?}")));
        }
        if matches!(scene, Scene::RecordLive | Scene::RecordActivity | Scene::RecordPaused) {
            // The island stays out of the recorder's video: ask Windows.
            let build = crate::window::windows_build();
            let can = build.is_some_and(|b| b >= crate::glass::EXCLUDE_FROM_CAPTURE_BUILD);
            let hidden = crate::window::capture_excluded(app.hwnd());
            let wine = under_wine();
            checks.push(check("recording-hidden-from-capture", hidden == can || (wine && !hidden), format!("display affinity: {}, Windows build {build:?}", if hidden { "left out of capture" } else { "in capture" })));
        }
        let ok = checks.iter().all(|c| c.ok);
        crate::pinfo!(
            "selftest: [{:02}/{}] {}: {} (settle {:.0} ms, frame {best:.2} ms, {w}x{h} px)",
            i + 1,
            SCENES.len(),
            scene.name(),
            if ok { "ok" } else { "FAILED" },
            waited * 1000.0
        );
        for c in checks.iter().filter(|c| !c.ok) {
            crate::pwarn!("selftest:   {} failed: {}", c.name, c.detail);
        }
        if slow {
            crate::pwarn!("selftest:   slow frame on a software GPU, not counted: {timing}");
            self.slow_frames.push(format!("{}: {timing}", scene.name()));
        }

        if w > 0 && h > 0 && px.len() == (w as usize) * (h as usize) * 4 {
            let back = self.glass.map(|run| {
                let r = app.selftest_capture_rect();
                let (_, _, sw, _) = app.selftest_surface();
                Backdrop { dark: run.dark_page(), scale, left: r.left, top: r.top, cx_dip: sw as f32 / 2.0 / scale }
            });
            self.queue_write(&name, w, h, px, scale, back);
        }
        self.states.push(StateReport {
            index: i + 1,
            name: scene.name(),
            expanded: scene.expanded(),
            page: scene.page().map(|p| p.key()),
            image: format!("{name}.png"),
            alpha_image: format!("{name}-alpha.png"),
            ok,
            settle_ms: (waited * 1000.0).round().max(0.0) as u64,
            frame_ms: round2(best),
            frame_samples_ms: samples,
            canvas_px: [w, h],
            window_pos_px: [present.0, present.1],
            body_dip: [body.x, body.y, body.w, body.h],
            measured_width_px: measured,
            checks,
        });
    }

    /// The Glass theme's own checks for one scene: the glass is drawn by the
    /// shader on the GPU, its rim bends the page (lens off changes it), it
    /// shows the page (smoked glass looks different), text reads at 4.5:1 on
    /// it, and in the flip run small glass is dark and the open island light.
    #[allow(clippy::too_many_arguments)]
    fn glass_checks(&self, app: &mut App, scene: Scene, run: GlassRun, px: &[u8], w: i32, h: i32, body: Rect, scale: f32, checks: &mut Vec<Check>) {
        let (shader, copy, look) = app.selftest_glass_state();
        let frame = Frame { w, h, px };
        let on_gpu = self.renderer != "software";
        if on_gpu {
            checks.push(check("glass-shader", shader && copy, format!("shader drew the glass: {shader}, backdrop on the GPU: {copy}")));
            if shader && copy {
                let mut again = |lens: f32, use_copy: bool| {
                    app.selftest_glass_switches(lens, use_copy);
                    self.apply(app, scene);
                    let (w2, h2, px2, _) = app.selftest_capture();
                    app.selftest_glass_switches(1.0, true);
                    (w2 == w && h2 == h).then_some(px2)
                };
                let flat = again(0.0, true);
                let smoked = again(1.0, false);
                self.apply(app, scene);
                let area = (body.w * body.h * scale * scale).max(1.0);
                match flat {
                    Some(f) => {
                        // A pill over flat browser chrome bends only a few pixels;
                        // a lens that doesn't run moves none.
                        let n = frame.changed_in(&f, body, scale, 6);
                        checks.push(check("glass-lens", n >= 10, format!("{n} pixels move when the lens is off")));
                    }
                    None => checks.push(check("glass-lens", false, "the frame changed size between captures")),
                }
                match smoked {
                    Some(f) => {
                        let n = frame.changed_in(&f, body, scale, 4);
                        let share = n as f32 / area;
                        checks.push(check("glass-shows-backdrop", share >= 0.2, format!("{:.0} % of the island differs from smoked glass", share * 100.0)));
                    }
                    None => checks.push(check("glass-shows-backdrop", false, "the frame changed size between captures")),
                }
            }
        }
        // Text against the glass: the island's typical colour (its median,
        // which text doesn't move) must give the ink 4.5:1. Smoked glass lets
        // the page through, so it's judged over the page, as people see it.
        let r = app.selftest_capture_rect();
        let cx_dip = app.selftest_surface().2 as f32 / 2.0 / scale;
        let mut seen = px.to_vec();
        for (i, p) in seen.chunks_exact_mut(4).enumerate() {
            let (x, y) = ((i as i32 % w) + r.left, (i as i32 / w) + r.top);
            let bg = backdrop_dip(run.dark_page(), (x as f32 + 0.5) / scale - cx_dip, (y as f32 + 0.5) / scale);
            let inv = 1.0 - p[3] as f32 / 255.0;
            for (k, c) in [2, 1, 0].into_iter().enumerate() {
                p[k] = (p[k] as f32 + bg[c] as f32 * inv).round().min(255.0) as u8;
            }
            p[3] = 255;
        }
        let median = Frame { w, h, px: &seen }.median_in(body, scale);
        let ink = [look.ink.r, look.ink.g, look.ink.b];
        let c = poni_core::glass::contrast(median, ink);
        checks.push(check("glass-ink-contrast", c >= 4.5, format!("{c:.2}:1 for ink rgb{:?} on glass rgb{:?}", rgb8(look.ink), median.map(|v| (v * 255.0).round() as u8))));
        if run == GlassRun::Flip {
            let want_light = scene.expanded();
            checks.push(check(
                "glass-flip",
                look.light == want_light,
                format!("{} glass over the dark page in light mode (want {})", if look.light { "light" } else { "dark" }, if want_light { "light: the open island follows the mode" } else { "dark: small glass flips" }),
            ));
        }
    }

    /// Composite + alpha PNGs, encoded off the UI thread.
    fn queue_write(&mut self, name: &str, w: i32, h: i32, px: Vec<u8>, scale: f32, back: Option<Backdrop>) {
        let dir = self.dir.clone();
        let base = name.to_string();
        let job = move || {
            let mut errors = Vec::new();
            let (cw, ch, comp) = composite(w as usize, h as usize, &px, scale, back);
            if let Err(e) = png::save_rgba(dir.join(format!("{base}.png")), cw, ch, &comp) {
                errors.push(e);
            }
            let alpha: Vec<u8> = px.as_chunks::<4>().0.iter().map(|p| p[3]).collect();
            if let Err(e) = png::save_gray(dir.join(format!("{base}-alpha.png")), w as u32, h as u32, &alpha) {
                errors.push(e);
            }
            errors
        };
        match std::thread::Builder::new().name("poni-selftest-png".into()).spawn(job) {
            Ok(handle) => self.writes.push((name.to_string(), handle)),
            Err(e) => self.checks.push(check("png-writer", false, format!("{name}: could not start a writer thread: {e}"))),
        }
    }

    fn finish(&mut self, writers_done: bool, waited: f64) -> i32 {
        let mut write_errors = Vec::new();
        let mut written = 0;
        for (name, handle) in std::mem::take(&mut self.writes) {
            if !handle.is_finished() {
                write_errors.push(format!("{name}: still encoding"));
                continue; // detached; the process exits soon
            }
            match handle.join() {
                Ok(errs) if errs.is_empty() => written += 1,
                Ok(errs) => write_errors.extend(errs),
                Err(_) => write_errors.push(format!("{name}: writer thread panicked")),
            }
        }
        let detail = if write_errors.is_empty() {
            format!("{written} screenshot pairs written in {:.0} ms after the last capture", waited * 1000.0)
        } else {
            write_errors.join("; ")
        };
        self.checks.push(check("png-files", writers_done && write_errors.is_empty(), detail));
        if let Some(rx) = self.addon_fetch.take() {
            let (ok, detail) = rx.recv_timeout(std::time::Duration::from_secs(60)).unwrap_or((false, "still downloading 60 s after the scenes".into()));
            self.checks.push(check("addon-download", ok, detail));
        }
        if self.states.len() < SCENES.len() {
            self.checks.push(check("states-visited", false, format!("{} of {} states captured", self.states.len(), SCENES.len())));
        }

        let mut failed: Vec<String> = self.checks.iter().filter(|c| !c.ok).map(|c| c.name.clone()).collect();
        for s in &self.states {
            failed.extend(s.checks.iter().filter(|c| !c.ok).map(|c| format!("{}: {}", s.name, c.name)));
        }
        let ok = failed.is_empty();
        let mut code = if ok { 0 } else { 1 };
        let total_ms = self.t0.elapsed().as_millis() as u64;
        let report = Report {
            app: "Poni",
            version: env!("CARGO_PKG_VERSION"),
            ok,
            exit_code: code,
            total_ms,
            scale: self.scale,
            expanded_width_dip: self.expanded_width,
            renderer: self.renderer.to_string(),
            frame_budget_ms: self.budget_ms,
            states_passed: self.states.iter().filter(|s| s.ok).count(),
            states_total: SCENES.len(),
            failed_checks: failed.clone(),
            slow_frames: std::mem::take(&mut self.slow_frames),
            checks: std::mem::take(&mut self.checks),
            states: std::mem::take(&mut self.states),
        };
        let path = self.dir.join("report.json");
        match serde_json::to_vec_pretty(&report).map_err(|e| e.to_string()).and_then(|j| fs::write(&path, j).map_err(|e| e.to_string())) {
            Ok(()) => crate::pinfo!("selftest: report written to {}", path.display()),
            Err(e) => {
                crate::pwarn!("selftest: could not write {}: {e}", path.display());
                code = 1;
            }
        }
        if !report.slow_frames.is_empty() {
            crate::pinfo!("selftest: {} slow frame(s) on a software GPU, reported and not counted", report.slow_frames.len());
        }
        if ok {
            crate::pinfo!("selftest: all {} states passed in {total_ms} ms", report.states_total);
        } else {
            crate::pwarn!("selftest: {} check(s) failed in {total_ms} ms: {}", failed.len(), failed.join(", "));
        }
        code
    }

    /// Remove screenshots and the report from an earlier run (only our own file names).
    fn clear_old_outputs(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else { return };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let b = name.as_bytes();
            let ours = name == "report.json"
                || (name.ends_with(".png") && b.len() > 3 && b[0].is_ascii_digit() && b[1].is_ascii_digit() && b[2] == b'-');
            if ours && e.path().is_file() {
                if let Err(err) = fs::remove_file(e.path()) {
                    crate::pwarn!("selftest: could not remove old {name}: {err}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------- fixtures

fn seed_tasks(st: &mut AppState) {
    let unix = st.unix;
    let day_start = st.day_start_unix();
    let tasks = &mut st.store.tasks;
    let done = tasks.add("Water the balcony plants", unix - 5 * 3600);
    tasks.add("Sketch the onboarding screens", unix - 3 * 3600);
    tasks.add("Book the ferry for Saturday", unix - 2 * 3600);
    if let Some(id) = done {
        tasks.toggle(id, (unix - 1800).max(day_start + 60));
    }
}

/// Where the media-lyric scene holds the song: 1.5 s into the first sung line
/// after the one the other music scenes hold at (so the two show different
/// lines), else into any sung line. `None` without synced lines.
fn lyric_scene_pos(l: &Lyrics) -> Option<f64> {
    if !l.synced {
        return None;
    }
    // A line with words that lasts a second or more, inside the track.
    let held = |k: usize| {
        let start = l.lines[k].time_s;
        let len = l.lines.get(k + 1).map_or(4.0, |n| n.time_s - start);
        let pos = start + (len / 2.0).min(1.5);
        (len >= 1.0 && pos < TRACK_LEN_S && !l.lines[k].text.trim().is_empty()).then_some(pos)
    };
    let after = l.line_index_at(TRACK_POS_S).map_or(0, |i| i + 1);
    (after..l.lines.len()).chain(0..after).find_map(held)
}

/// Procedural album art: a harbor sunset with a paper boat (premultiplied BGRA, opaque).
fn make_art(size: u32) -> Vec<u8> {
    const SS: usize = 3; // 3x3 supersampling for smooth edges
    let n = size.max(1) as usize;
    let mut px = vec![0u8; n * n * 4];
    for y in 0..n {
        for x in 0..n {
            let mut acc = [0.0f32; 3];
            for sy in 0..SS {
                for sx in 0..SS {
                    let u = (x as f32 + (sx as f32 + 0.5) / SS as f32) / n as f32;
                    let v = (y as f32 + (sy as f32 + 0.5) / SS as f32) / n as f32;
                    for (a, c) in acc.iter_mut().zip(art_sample(u, v)) {
                        *a += c;
                    }
                }
            }
            let o = (y * n + x) * 4;
            let to8 = |c: f32| ((c / (SS * SS) as f32).clamp(0.0, 1.0) * 255.0).round() as u8;
            px[o] = to8(acc[2]);
            px[o + 1] = to8(acc[1]);
            px[o + 2] = to8(acc[0]);
            px[o + 3] = 255;
        }
    }
    px
}

fn rgb(hex: u32) -> [f32; 3] {
    [((hex >> 16) & 0xff) as f32 / 255.0, ((hex >> 8) & 0xff) as f32 / 255.0, (hex & 0xff) as f32 / 255.0]
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn in_triangle(p: (f32, f32), a: (f32, f32), b: (f32, f32), c: (f32, f32)) -> bool {
    let edge = |p0: (f32, f32), p1: (f32, f32)| (p1.0 - p0.0) * (p.1 - p0.1) - (p1.1 - p0.1) * (p.0 - p0.0);
    let (d1, d2, d3) = (edge(a, b), edge(b, c), edge(c, a));
    let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(neg && pos)
}

fn art_sample(u: f32, v: f32) -> [f32; 3] {
    const HORIZON: f32 = 0.64;
    let (sun_x, sun_y, sun_r) = (0.66, HORIZON - 0.03, 0.15);
    let mut c = if v < HORIZON {
        mix(rgb(0xFFB37A), rgb(0xE0607E), v / HORIZON)
    } else {
        mix(rgb(0x2F6F8F), rgb(0x13233F), (v - HORIZON) / (1.0 - HORIZON))
    };
    let d = ((u - sun_x).powi(2) + (v - sun_y).powi(2)).sqrt();
    if v < HORIZON && d < sun_r {
        c = mix(c, rgb(0xFFE3A3), 0.92);
    }
    // Glitter: short streaks on the water under the sun, narrowing with depth.
    if v > HORIZON + 0.015 && v < HORIZON + 0.24 {
        let depth = (v - HORIZON) / 0.24;
        let band = ((v - HORIZON) * 42.0).fract();
        if band < 0.4 && (u - sun_x).abs() < 0.12 * (1.0 - depth) {
            c = mix(c, rgb(0xFFD08A), 0.6 * (1.0 - depth));
        }
    }
    // Paper boat: a hull trapezoid and a folded sail.
    let paper = rgb(0xF7F3EA);
    let hull = v > 0.70 && v < 0.78 && {
        let t = (v - 0.70) / 0.08;
        u > 0.17 + 0.06 * t && u < 0.51 - 0.06 * t
    };
    let sail = in_triangle((u, v), (0.34, 0.46), (0.45, 0.70), (0.24, 0.70));
    if hull {
        c = mix(paper, rgb(0xD8D0C2), ((v - 0.70) / 0.08) * 0.5);
    }
    if sail {
        c = if u > 0.34 { mix(paper, rgb(0xD9D2C4), 0.55) } else { paper };
    }
    c
}

// ---------------------------------------------------------------- pixels

/// The rows above the Pill body: the largest alpha, the largest on the top
/// row (the screen's edge), how many pixels have colour (anything but a
/// black shadow or a hit pixel) and how many are the invisible hit pixels
/// (alpha 1).
struct GapScan {
    most: u8,
    edge: u8,
    coloured: usize,
    hit: usize,
}

/// Premultiplied BGRA frame with bounds-checked reads.
struct Frame<'a> {
    w: i32,
    h: i32,
    px: &'a [u8],
}

impl Frame<'_> {
    /// `[b, g, r, a]`, or transparent outside the frame.
    fn at(&self, x: i32, y: i32) -> [u8; 4] {
        if x < 0 || y < 0 || x >= self.w || y >= self.h {
            return [0; 4];
        }
        let o = (y as usize * self.w as usize + x as usize) * 4;
        match self.px.get(o..o + 4) {
            Some(p) => [p[0], p[1], p[2], p[3]],
            None => [0; 4],
        }
    }

    /// Width of the run of mostly-opaque pixels through (`x`, `y`).
    fn opaque_run(&self, x: i32, y: i32) -> i32 {
        if self.at(x, y)[3] < 128 {
            return 0;
        }
        let (mut l, mut r) = (x, x);
        while l > 0 && self.at(l - 1, y)[3] >= 128 {
            l -= 1;
        }
        while r + 1 < self.w && self.at(r + 1, y)[3] >= 128 {
            r += 1;
        }
        r - l + 1
    }

    /// Opaque, bright pixels inside the island body (text, icons, art).
    /// Opaque pixels inside the island that stand clearly apart from its
    /// ground colour (text, icons, art).
    fn content_pixels(&self, body: Rect, scale: f32, ground: [u8; 3]) -> usize {
        let x0 = (body.x * scale).floor().max(0.0) as i32;
        let x1 = ((body.x + body.w) * scale).ceil() as i32;
        let y1 = ((body.y + body.h) * scale).ceil() as i32;
        let mut n = 0;
        for y in (body.y * scale).floor().max(0.0) as i32..y1.min(self.h) {
            for x in x0..x1.min(self.w) {
                let [b, g, r, a] = self.at(x, y);
                let off = [r, g, b].iter().zip(ground).map(|(&p, q)| p.abs_diff(q)).max().unwrap_or(0);
                if a >= 200 && off >= 140 {
                    n += 1;
                }
            }
        }
        n
    }
}

impl Frame<'_> {
    /// Pixels inside `body` where `other` differs by at least `min` in some channel.
    fn changed_in(&self, other: &[u8], body: Rect, scale: f32, min: u8) -> usize {
        let (x0, y0) = ((body.x * scale).floor().max(0.0) as i32, (body.y * scale).floor().max(0.0) as i32);
        let (x1, y1) = (((body.x + body.w) * scale).ceil() as i32, ((body.y + body.h) * scale).ceil() as i32);
        let mut n = 0;
        for y in y0..y1.min(self.h) {
            for x in x0..x1.min(self.w) {
                let o = (y as usize * self.w as usize + x as usize) * 4;
                if let (Some(a), Some(b)) = (self.px.get(o..o + 3), other.get(o..o + 3)) {
                    if a.iter().zip(b).any(|(p, q)| p.abs_diff(*q) >= min) {
                        n += 1;
                    }
                }
            }
        }
        n
    }

    /// What rows `0..rows` hold (see [`GapScan`]).
    fn gap_scan(&self, rows: i32) -> GapScan {
        let mut g = GapScan { most: 0, edge: 0, coloured: 0, hit: 0 };
        for y in 0..rows.min(self.h) {
            for x in 0..self.w {
                let [b, gr, r, a] = self.at(x, y);
                g.most = g.most.max(a);
                if y == 0 {
                    g.edge = g.edge.max(a);
                }
                // Premultiplied: black at any alpha stays (0, 0, 0); 2 allows rounding.
                g.coloured += usize::from(r.max(gr).max(b) > 2);
                g.hit += usize::from(a == 1);
            }
        }
        g
    }

    /// The largest alpha along each edge: top, bottom, left, right.
    fn edge_alpha(&self) -> [u8; 4] {
        let row = |y: i32| (0..self.w).map(|x| self.at(x, y)[3]).max().unwrap_or(0);
        let col = |x: i32| (0..self.h).map(|y| self.at(x, y)[3]).max().unwrap_or(0);
        [row(0), row(self.h - 1), col(0), col(self.w - 1)]
    }

    /// The smallest pixel rectangle (x0, y0, x1, y1, the far edges exclusive)
    /// holding every pixel with at least `min` alpha.
    fn drawn_bounds(&self, min: u8) -> Option<(i32, i32, i32, i32)> {
        let mut b: Option<(i32, i32, i32, i32)> = None;
        for y in 0..self.h {
            for x in 0..self.w {
                if self.at(x, y)[3] >= min {
                    b = Some(b.map_or((x, y, x + 1, y + 1), |(x0, y0, x1, y1)| (x0.min(x), y0.min(y), x1.max(x + 1), y1.max(y + 1))));
                }
            }
        }
        b
    }

    /// The median colour (sRGB, 0..=1, per channel) inside `body`, sampled
    /// every other pixel.
    fn median_in(&self, body: Rect, scale: f32) -> [f32; 3] {
        let (x0, y0) = ((body.x * scale).floor().max(0.0) as i32, (body.y * scale).floor().max(0.0) as i32);
        let (x1, y1) = (((body.x + body.w) * scale).ceil() as i32, ((body.y + body.h) * scale).ceil() as i32);
        let mut ch: [Vec<u8>; 3] = [Vec::new(), Vec::new(), Vec::new()];
        for y in (y0..y1.min(self.h)).step_by(2) {
            for x in (x0..x1.min(self.w)).step_by(2) {
                let [b, g, r, a] = self.at(x, y);
                if a >= 250 {
                    ch[0].push(r);
                    ch[1].push(g);
                    ch[2].push(b);
                }
            }
        }
        ch.map(|mut v| {
            if v.is_empty() {
                return 0.0;
            }
            v.sort_unstable();
            v[v.len() / 2] as f32 / 255.0
        })
    }
}

/// The island window must be a visible, top-most, non-activating layered
/// tool window, laid out where the app placed it, with the frame inside it.
fn window_check(hwnd: HWND, surface: (i32, i32, i32, i32), present: (i32, i32), w: i32, h: i32) -> Check {
    let need = WS_EX_LAYERED.0 | WS_EX_TOPMOST.0 | WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0;
    let (visible, ex, rect) = unsafe {
        let visible = IsWindowVisible(hwnd).as_bool();
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let mut rc = RECT::default();
        let rect = GetWindowRect(hwnd, &mut rc).ok().map(|_| rc);
        (visible, ex, rect)
    };
    let styles_ok = ex & need == need;
    let (sx, sy, sw, sh) = surface;
    let rect_ok = rect.is_some_and(|r| {
        r.left == sx && r.top == sy && r.right - r.left == sw && r.bottom - r.top == sh
            && present.0 >= r.left && present.1 >= r.top && present.0 + w <= r.right && present.1 + h <= r.bottom
    });
    let rect_s = rect.map(|r| format!("({}, {}) {}x{}", r.left, r.top, r.right - r.left, r.bottom - r.top)).unwrap_or_else(|| "unavailable".into());
    check(
        "window",
        visible && styles_ok && rect_ok,
        format!("visible {visible}, ex-style {ex:#x} (needs {need:#x}), rect {rect_s} (laid out ({sx}, {sy}) {sw}x{sh}), frame at ({}, {}) {w}x{h}", present.0, present.1),
    )
}

/// Pill: what the gap above the floating body may hold besides the invisible
/// hit pixels. The shadow's soft upper edge reaches into it (spec 6.2: about
/// 21/255 just above the body, 3/255 at the screen's edge); it is black, at
/// most the first alpha anywhere in the gap and the second on the screen's
/// edge, so the island still reads as floating. Both at 1 = nothing but the
/// hit pixels.
const PILL_GAP_SHADOW_MAX: u8 = 64;
const PILL_GAP_EDGE_MAX: u8 = 8;

/// The Pill theme's own checks. The island floats, so the gap above the body
/// shows the screen: nothing in it but the invisible hit pixels (alpha 1)
/// that take the mouse and the shadow's soft upper edge, and the body's top
/// edge where its geometry says. Its shadow fades out inside the room the
/// window keeps beside and below the body: a shadow that reaches the window's
/// side or bottom edge is cut off there, a hard line on a light wallpaper.
/// (The top edge is the screen's, and the gap check reads it.)
fn pill_checks(frame: &Frame, body: Rect, scale: f32, checks: &mut Vec<Check>) {
    // Rows wholly above the body.
    let gap = (body.y * scale).floor() as i32;
    let g = frame.gap_scan(gap);
    let cx = ((body.x + body.w / 2.0) * scale).floor() as i32;
    let first = (0..frame.h).find(|&y| frame.at(cx, y)[3] >= 128);
    let want = body.y * scale;
    let starts = first.is_some_and(|y| (y as f32 - want).abs() <= 1.0);
    checks.push(check(
        "pill-gap",
        gap > 0 && g.most <= PILL_GAP_SHADOW_MAX && g.edge <= PILL_GAP_EDGE_MAX && g.coloured == 0 && starts,
        format!(
            "rows 0-{} above the body: max alpha {} (want <= {PILL_GAP_SHADOW_MAX}), {} on the screen's edge (want <= {PILL_GAP_EDGE_MAX}), \
             {} coloured pixels (want 0: shadow and hit pixels are black), {} hit pixels at alpha 1; the body starts at row {} (geometry {want:.1})",
            gap - 1,
            g.most,
            g.edge,
            g.coloured,
            g.hit,
            first.map_or("-".to_string(), |y| y.to_string())
        ),
    ));
    let [_, bottom, left, right] = frame.edge_alpha();
    // How far past the body anything but the hit pixels is drawn, against the room there.
    let (w, h) = (frame.w as f32 / scale, frame.h as f32 / scale);
    let reach = frame.drawn_bounds(2).map_or("nothing drawn".to_string(), |(x0, _, x1, y1)| {
        format!(
            "drawn up to {:.1} DIP left, {:.1} right and {:.1} below the body (room {:.1}, {:.1}, {:.1})",
            body.x - x0 as f32 / scale,
            x1 as f32 / scale - body.right(),
            y1 as f32 / scale - body.bottom(),
            body.x,
            w - body.right(),
            h - body.bottom()
        )
    });
    checks.push(check(
        "pill-shadow-inside",
        [left, right, bottom].iter().all(|&a| a <= 1),
        format!("edge alpha left {left}, right {right}, bottom {bottom} (want <= 1); {reach}"),
    ));
}

/// The media-lyric scene's premise: music plays and the song is inside a
/// synced line that has words.
fn lyric_line_check(st: &AppState) -> Check {
    let Some(m) = st.media.as_ref().filter(|m| m.is_playing()) else {
        return check("lyric-line", false, "no music playing");
    };
    let pos = m.timeline.position_at(st.now);
    let lyrics = st.lyrics.lyrics.as_ref().filter(|_| st.lyrics.track_key == m.track_key);
    match lyrics.and_then(|l| l.line_index_at(pos).map(|k| (k, l.lines[k].text.trim()))) {
        Some((k, text)) if !text.is_empty() => {
            check("lyric-line", true, format!("line {} \"{text}\" sung at {pos:.1} s", k + 1))
        }
        Some((k, _)) => check("lyric-line", false, format!("line {} at {pos:.1} s has no words", k + 1)),
        None => check("lyric-line", false, format!("no synced line at {pos:.1} s")),
    }
}

/// Centre of the first header tab. Mirrors `pages::draw_header`: tabs start at
/// the header's left edge and are 30 DIPs wide while inactive.
/// A Settings window frame must draw well inside a 60 Hz refresh even on
/// CI's small runners (the window glides at the display's rate; 144 Hz
/// leaves 6.94 ms).
const SETTINGS_FRAME_BUDGET_MS: f64 = 12.0;

fn first_tab_point(body: Rect) -> Point {
    let header = crate::pages::header_rect(body);
    Point::new(header.x + 15.0, header.center().y)
}

fn mid_blink() -> bool {
    let phase = clock::mono() % BLINK_PERIOD;
    phase < BLINK_GUARD.0 || phase > BLINK_PERIOD - BLINK_GUARD.1
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

/// Where a capture sits over the Glass self-test's page: the page's kind,
/// the scale, the capture's top-left in window pixels and the window's
/// centre in DIPs.
#[derive(Clone, Copy, Debug)]
struct Backdrop {
    dark: bool,
    scale: f32,
    left: i32,
    top: i32,
    cx_dip: f32,
}

/// Frame over a soft wallpaper-like gradient (or, in the Glass theme, the
/// page under the island), with room around it, the way the island hangs from
/// the top of a desktop. Returns straight RGBA.
fn composite(w: usize, h: usize, pbgra: &[u8], scale: f32, back: Option<Backdrop>) -> (u32, u32, Vec<u8>) {
    let pad_x = (48.0 * scale).round().max(0.0) as usize;
    let pad_b = (40.0 * scale).round().max(0.0) as usize;
    let (ow, oh) = (w + 2 * pad_x, h + pad_b);
    let (tl, tr, bl, br) = (rgb(0x5D6B84), rgb(0x837C98), rgb(0x8E8C96), rgb(0xB9A596));
    let fx = 1.0 / (ow.max(2) - 1) as f32;
    let fy = 1.0 / (oh.max(2) - 1) as f32;
    let mut out = vec![0u8; ow * oh * 4];
    for y in 0..oh {
        let v = y as f32 * fy;
        let (left, right) = (mix(tl, bl, v), mix(tr, br, v));
        for x in 0..ow {
            let bg = match back {
                Some(b) => {
                    let wx = (b.left + x as i32 - pad_x as i32) as f32 + 0.5;
                    let wy = (b.top + y as i32) as f32 + 0.5;
                    backdrop_dip(b.dark, wx / b.scale - b.cx_dip, wy / b.scale).map(|c| c as f32 / 255.0)
                }
                None => mix(left, right, x as f32 * fx),
            };
            let mut c = [bg[0] * 255.0, bg[1] * 255.0, bg[2] * 255.0];
            if y < h && x >= pad_x && x < pad_x + w {
                let o = (y * w + (x - pad_x)) * 4;
                if let Some(p) = pbgra.get(o..o + 4) {
                    let inv = 1.0 - p[3] as f32 / 255.0;
                    c = [p[2] as f32 + c[0] * inv, p[1] as f32 + c[1] * inv, p[0] as f32 + c[2] * inv];
                }
            }
            let o = (y * ow + x) * 4;
            out[o] = c[0].round().clamp(0.0, 255.0) as u8;
            out[o + 1] = c[1].round().clamp(0.0, 255.0) as u8;
            out[o + 2] = c[2].round().clamp(0.0, 255.0) as u8;
            out[o + 3] = 255;
        }
    }
    (ow as u32, oh as u32, out)
}

/// A colour as 8-bit (r, g, b).
fn rgb8(c: Color) -> [u8; 3] {
    [c.r, c.g, c.b].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

// ---------------------------------------------------------------- Glass backdrops

/// What the Glass self-test puts under the island (PONI_SELFTEST_GLASS).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GlassRun {
    /// A light browser page, Windows in light mode.
    Light,
    /// A dark video page, Windows in dark mode.
    Dark,
    /// The dark page with Windows in light mode: small glass flips dark, the
    /// open island stays light.
    Flip,
}

impl GlassRun {
    fn from_env() -> Self {
        match std::env::var("PONI_SELFTEST_GLASS").map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            Ok("dark") => GlassRun::Dark,
            Ok("flip") => GlassRun::Flip,
            _ => GlassRun::Light,
        }
    }

    fn dark_page(self) -> bool {
        self != GlassRun::Light
    }

    fn app_light(self) -> bool {
        self != GlassRun::Dark
    }
}

/// The page under a `w`×`h` window at `scale`: BGRA, opaque.
fn glass_backdrop(dark: bool, w: i32, h: i32, scale: f32) -> Vec<u8> {
    let cx = w as f32 / 2.0 / scale;
    let mut px = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let c = backdrop_dip(dark, (x as f32 + 0.5) / scale - cx, (y as f32 + 0.5) / scale);
            let o = ((y * w + x) * 4) as usize;
            px[o..o + 4].copy_from_slice(&[c[2], c[1], c[0], 255]);
        }
    }
    px
}

fn hash(i: i32) -> u32 {
    let mut h = (i as u32) ^ 0x9E37_79B9;
    h = h.wrapping_mul(0x85EB_CA6B);
    h ^= h >> 13;
    h = h.wrapping_mul(0xC2B2_AE35);
    h ^ (h >> 16)
}

/// Text-like word bars between `x0` and `x1`: is `x` on a word?
fn on_word(x: f32, x0: f32, x1: f32, seed: i32) -> bool {
    if x < x0 || x >= x1 {
        return false;
    }
    let mut at = x0;
    let mut k = 0;
    while at < x1 {
        let w = 14.0 + (hash(seed.wrapping_mul(31).wrapping_add(k)) % 33) as f32;
        if x >= at && x < (at + w).min(x1) {
            return true;
        }
        at += w + 5.0;
        k += 1;
    }
    false
}

fn mix8(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (a[i] as f32 + (b[i] as f32 - a[i] as f32) * t).round() as u8)
}

const FAVICONS: [[u8; 3]; 6] = [[232, 69, 60], [240, 137, 46], [26, 115, 232], [24, 128, 56], [147, 52, 230], [95, 99, 104]];

/// A browser's tab strip and toolbar (light or dark), `y` < 85 DIPs. Tabs
/// are 228 DIPs wide, the active one just left of the island's centre.
fn browser_chrome(dark: bool, x: f32, y: f32) -> [u8; 3] {
    let (strip, active_bg, sep, text, text_dim, bar, pill, url, url_dim) = if dark {
        ([0x20, 0x21, 0x24], [0x35, 0x36, 0x3A], [0x5F, 0x63, 0x68], [0xE8, 0xEA, 0xED], [0xBD, 0xC1, 0xC6], [0x35, 0x36, 0x3A], [0x20, 0x21, 0x24], [0xE8, 0xEA, 0xED], [0x9A, 0xA0, 0xA6])
    } else {
        ([0xDF, 0xE3, 0xE8], [0xFF, 0xFF, 0xFF], [0xA9, 0xAD, 0xB3], [0x20, 0x21, 0x24], [0x3C, 0x40, 0x43], [0xFF, 0xFF, 0xFF], [0xF1, 0xF3, 0xF4], [0x20, 0x21, 0x24], [0x5F, 0x63, 0x68])
    };
    if y < 41.0 {
        let k = ((x + 150.0) / 228.0).floor() as i32;
        let lx = x - (-150.0 + 228.0 * k as f32);
        let active = k == 0;
        let mut c = if active && y >= 7.0 { active_bg } else { strip };
        if k != 0 && k != 1 && lx < 1.0 && (9.0..25.0).contains(&y) {
            c = sep;
        }
        if (13.0..29.0).contains(&y) && (12.0..28.0).contains(&lx) {
            c = FAVICONS[k.rem_euclid(6) as usize];
        }
        if (17.0..24.0).contains(&y) && on_word(lx, 36.0, 190.0, k + 7) {
            c = if active { text } else { text_dim };
        }
        return c;
    }
    if !dark && y >= 84.0 {
        return [0xE3, 0xE3, 0xE3];
    }
    let in_pill = (47.0..79.0).contains(&y) && (x.abs() < 544.0 || (x.abs() - 544.0).powi(2) + (y - 63.0).powi(2) < 256.0);
    if in_pill {
        if (59.0..67.0).contains(&y) && on_word(x, -500.0, -60.0, 77) {
            return url;
        }
        if (59.0..67.0).contains(&y) && on_word(x, -52.0, 240.0, 91) {
            return url_dim;
        }
        return pill;
    }
    bar
}

/// One pixel of the Glass self-test's page at (`x`, `y`) DIPs from the
/// island's top centre. Light: a browser over a warm morning sky with a big
/// white heading. Dark: a dark browser playing a sunset over a skyline. Text
/// and edges cross the island's rim so the lens has something to bend.
fn backdrop_dip(dark: bool, x: f32, y: f32) -> [u8; 3] {
    if y < 85.0 {
        return browser_chrome(dark, x, y);
    }
    if !dark {
        if y < 137.0 {
            let ink = [0x20, 0x21, 0x24];
            if ((104.0..118.0).contains(&y) && on_word(x, -672.0, -570.0, 5)) || ((107.0..115.0).contains(&y) && on_word(x, -500.0, 120.0, 13)) {
                return ink;
            }
            if (131.0..133.0).contains(&y) && (-500.0..-452.0).contains(&x) {
                return ink;
            }
            return [0xFF, 0xFF, 0xFF];
        }
        let t = (y - 137.0) / 300.0;
        let u = (x + 720.0) / 1440.0;
        let top = mix8([250, 196, 140], [236, 170, 190], u);
        let bottom = mix8([246, 150, 120], [200, 140, 175], u);
        let mut c = mix8(top, bottom, t);
        let cloud = ((x * 0.013 + y * 0.05).sin() * (x * 0.004 - y * 0.02).cos()).max(0.0) * 0.35;
        c = mix8(c, [255, 236, 220], cloud);
        // A heading of chunky letters, its lower half under the open island's rim.
        if (171.0..209.0).contains(&y) && on_word(x, -364.0, -20.0, 3) {
            let k = ((x + 364.0) / 9.0) as i32;
            if !((186.0..193.0).contains(&y) && hash(k) % 3 == 0) {
                c = [255, 255, 255];
            }
        }
        if (231.0..241.0).contains(&y) && on_word(x, -360.0, 130.0, 21) {
            c = mix8(c, [255, 255, 255], 0.92);
        }
        return c;
    }
    if y < 141.0 {
        let mut c = [0x0F, 0x0F, 0x0F];
        if (104.0..122.0).contains(&y) && (-696.0..-670.0).contains(&x) {
            c = [0xE8, 0x45, 0x3C];
        }
        if (106.0..120.0).contains(&y) && (-662.0..-616.0).contains(&x) {
            c = [0xF1, 0xF1, 0xF1];
        }
        if (94.0..132.0).contains(&y) && x.abs() < 270.0 {
            let ring = y < 95.0 || y >= 131.0 || x.abs() >= 269.0;
            c = if ring { [0x30, 0x30, 0x30] } else { [0x12, 0x12, 0x12] };
            if (109.0..117.0).contains(&y) && on_word(x, -250.0, -180.0, 41) {
                c = [0x88, 0x88, 0x88];
            }
        }
        return c;
    }
    // The video: a sunset sky, hills, water and a skyline.
    let t = (y - 141.0) / 90.0;
    let u = (x + 720.0) / 1440.0;
    let mut c = mix8(mix8([255, 172, 120], [232, 160, 150], u), [150, 140, 175], t);
    let hills = 205.0 + 12.0 * (x * 0.011).sin() + 7.0 * (x * 0.027 + 1.3).sin();
    if y > hills && y < 236.0 {
        c = mix8([96, 108, 140], [120, 128, 160], (y - hills) / 30.0);
    }
    if (236.0..248.0).contains(&y) {
        c = mix8([196, 170, 176], [228, 196, 186], ((x * 0.09).sin() * 0.5 + 0.5) * 0.6);
    }
    let b = ((x + 2000.0) / 14.0).floor() as i32;
    let bx = (x + 2000.0).rem_euclid(14.0);
    let top = 262.0 - (18 + hash(b) % 46) as f32;
    if y > top && y < 262.0 && bx < 10.0 {
        c = [150, 158, 178];
        let (wx, wy) = ((x * 2.0) as i32, (y * 1.0) as i32);
        if wy % 6 < 2 && wx % 4 < 2 && hash(b.wrapping_mul(97).wrapping_add(wy)) % 5 == 0 {
            c = [255, 214, 150];
        }
    }
    if y >= 262.0 {
        let n = ((x * 0.21).sin() * (y * 0.33).cos() * 0.5 + 0.5) * 0.5;
        c = mix8([34, 70, 46], [58, 96, 60], n);
    }
    c
}

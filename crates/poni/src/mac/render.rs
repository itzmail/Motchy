//! `poni --render <dir> [--theme classic|glass|pill] [--notch]`: draw the island's
//! states with sample data into PNGs, offscreen, through the Mac renderer.
//! CI runs it on a Mac with no notch and no person at the screen, so this is
//! how the Mac drawing gets looked at: every page and activity, each theme,
//! with and without a (simulated) MacBook notch.
//!
//! Each picture is the island over a desktop: a soft wallpaper gradient and a
//! light menu bar. With `--notch` the camera notch is painted on top, black,
//! the way it hides that part of the island on a real MacBook. Glass is
//! macOS's own view on the real window, so here a smoked stand-in takes its
//! place (the smoke run's screenshots show the real one). Pill floats below
//! the top edge, or around a notch rests inside it and floats below the menu
//! bar for everything else (notch.rs). Around a
//! notch, the last picture is the Eyes look's hide and seek with the camera
//! as it moves, a frame a row (`render_peek`).
//!
//! The music scenes use made-up data to test the drawing; the Mac app shows
//! music once the media slice is built. The System page's scenes show a
//! made-up minute of a MacBook too, with real app icons from Finder.

use crate::activities::{self, Mode};
use crate::gfx::{Bitmap, Gfx, ImageData};
use crate::icons::Icon;
use crate::island::{Island, SHOULDER};
use crate::notch::{self, Notch};
use crate::pages::{self, PageKind};
use crate::state::{
    AppState, CityFile, Hud, HudKind, InlineAction, Locate, LyricsState, LyricsStatus, MediaSession, NoticeAction, PowerState, SysStats, VolumeState,
};
use crate::ui::{Hit, Input};
use poni_core::addons::{Status as AddonStatus, PRAYER_TIMES};
use poni_core::datetime::LocalDateTime;
use poni_core::geom::{palette, Color, Point, Rect};
use poni_core::lrc::parse_lrc;
use poni_core::pomodoro::{Phase, RunState};
use poni_core::power::PowerReading;
use poni_core::prayer::{PlaceList, Prayer, World};
use poni_core::settings::{IdleStyle, Settings, Theme};
use poni_core::store::Store;
use poni_core::sysmon::{
    level_pressure, AppUse, BatteryReading, CoreKind, CoreLoad, CpuReading, DiskActivity, Drive, GpuReading, MemoryReading, Module, Monitor, NetReading,
    PressureLevel, Reading, HISTORY_LEN,
};
use poni_core::timeline::Timeline;
use std::path::{Path, PathBuf};

/// Pixels per DIP (a Retina screen).
const SCALE: f32 = 2.0;
/// Room around the island in each picture (DIPs).
const MARGIN_X: f32 = 36.0;
const MARGIN_BOTTOM: f32 = 40.0;
/// The menu bar of a Mac without a notch (DIPs).
const MENU_BAR: f32 = 24.0;
/// A fixed moment for every picture: 24 Oct 2026, 09:41.
const NOW_MONO: f64 = 1000.0;
const NOW_UNIX: i64 = 1_792_834_860;

const TITLE: &str = "Paper Boats";
const ARTIST: &str = "The Lanterns";
const ALBUM: &str = "Harbor Lights";
const LYRICS_LRC: &str = "\
[00:38.00]Folded up the morning into something small
[00:43.50]Set it on the water by the harbor wall
[00:49.00]Ink along the edges of a name I used to know
[00:54.50]Watched it find the current, watched it go
[01:00.00]Paper boats, carry what I couldn't say
[01:05.50]Over the grey and out across the bay
[01:11.00]If the tide is gentle they will find the light
";
#[derive(Clone, Copy, Debug, PartialEq)]
enum Scene {
    IdleClock,
    IdleEyes,
    Hover,
    Timer,
    Media,
    FocusNotice,
    VolumeHud,
    ChargingHud,
    LowBatteryHud,
    Page(PageKind),
    ShelfItems,
    /// The System page on one module.
    System(Module),
    /// Pill's status bar look at rest.
    IdleStatus,
    /// Music with no line to sing (lyrics off): the song in Pill's pill,
    /// floating below a notch too.
    MediaNoLine,
    /// The Add-ons tab, Prayer times at this status.
    Addons(AddonStatus),
    /// Prayer times on, in Kota Jakarta, 8 minutes before Ashar: the page,
    /// its place picker, the resting island's countdown, and the notice at
    /// the time with the adzan playing.
    Prayer,
    PrayerPicker,
    PrayerCountdown,
    PrayerNotice,
    /// Prayer times abroad: the place picker with "edin" typed, and the page
    /// for Edinburgh picked by hand (its method beside its name).
    PrayerPickerWorld,
    PrayerWorld,
    /// Bluetooth headphones connected (the headphone pop-up).
    HeadphonesHud,
    /// The Update sheet for a later version: while it downloads (or where
    /// this copy can't replace itself), with the DMG to download, and once
    /// its app is out of the DMG and checked, Install now and Install on
    /// next start.
    UpdateSheet,
    UpdateReady,
    /// The AI tab with made-up readings (`pages::usage::picture`): plans
    /// and limits, a trouble mix, nothing found, the first look, the
    /// Services list.
    Ai(crate::pages::usage::Picture),
    /// The Coding page in its four states: tracking off, the editors not
    /// pointed here yet, waiting for the first heartbeats, and a month of days.
    Coding(u8),
}

const SCENES: [Scene; 15] = [
    Scene::IdleClock,
    Scene::IdleEyes,
    Scene::Hover,
    Scene::Timer,
    Scene::Media,
    Scene::FocusNotice,
    Scene::VolumeHud,
    Scene::Page(PageKind::Focus),
    Scene::Page(PageKind::Today),
    Scene::Page(PageKind::Tasks),
    Scene::Page(PageKind::Media),
    Scene::Page(PageKind::Shelf),
    // Slice 3: the pop-ups and the Shelf with real files.
    Scene::ChargingHud,
    Scene::LowBatteryHud,
    Scene::ShelfItems,
];

/// The System page's modules (after the scenes above, so their numbers stay).
const SYSTEM_SCENES: [Scene; 6] = [
    Scene::System(Module::Cpu),
    Scene::System(Module::Gpu),
    Scene::System(Module::Memory),
    Scene::System(Module::Drives),
    Scene::System(Module::Network),
    Scene::System(Module::Battery),
];

/// Pill's own looks, after its frames strip (number 26), so every other
/// picture keeps its number.
const PILL_SCENES: [Scene; 2] = [Scene::IdleStatus, Scene::MediaNoLine];

/// The Add-ons tab, after Pill's pictures.
const ADDON_SCENES: [Scene; 3] = [
    Scene::Addons(AddonStatus::Off),
    Scene::Addons(AddonStatus::Downloading { done: 1_700_000, total: 0 }),
    Scene::Addons(AddonStatus::Ready),
];

/// Prayer times, after the Add-ons tab.
const PRAYER_SCENES: [Scene; 4] = [Scene::Prayer, Scene::PrayerPicker, Scene::PrayerCountdown, Scene::PrayerNotice];
/// Prayer times abroad, after Today (so every other picture keeps its number).
const PRAYER_WORLD_SCENES: [Scene; 2] = [Scene::PrayerPickerWorld, Scene::PrayerWorld];
/// The AI tab in each of its sample states (the page's own list), last of all.
fn ai_scenes() -> impl Iterator<Item = Scene> {
    crate::pages::usage::Picture::ALL.into_iter().map(Scene::Ai)
}

/// The Settings page (behind the gear), after Prayer times.
const SETTINGS_SCENES: [Scene; 1] = [Scene::Page(PageKind::Settings)];
const CODING_SCENES: [Scene; 4] = [Scene::Coding(0), Scene::Coding(1), Scene::Coding(2), Scene::Coding(3)];
/// The headphone pop-up, after Settings.
const HEADPHONE_SCENES: [Scene; 1] = [Scene::HeadphonesHud];
/// The System page's Power module on a Mac without a battery, after headphones.
const POWER_SCENES: [Scene; 1] = [Scene::System(Module::Power)];
/// The Update sheet, after Prayer times abroad.
const UPDATE_SCENES: [Scene; 2] = [Scene::UpdateSheet, Scene::UpdateReady];
impl Scene {
    fn name(self) -> String {
        match self {
            Scene::IdleClock => "idle-clock".into(),
            Scene::IdleEyes => "idle-eyes".into(),
            Scene::Hover => "idle-hover".into(),
            Scene::Timer => "timer-activity".into(),
            Scene::Media => "media-activity".into(),
            Scene::FocusNotice => "focus-notice".into(),
            Scene::VolumeHud => "volume-hud".into(),
            Scene::ChargingHud => "charging-hud".into(),
            Scene::LowBatteryHud => "low-battery-hud".into(),
            Scene::ShelfItems => "page-shelf-items".into(),
            Scene::Page(p) => format!("page-{}", p.key()),
            Scene::System(Module::Cpu) => "page-system".into(),
            Scene::System(m) => format!("page-system-{}", m.title().to_lowercase()),
            Scene::IdleStatus => "idle-status".into(),
            Scene::MediaNoLine => "media-no-line".into(),
            Scene::Addons(AddonStatus::Off) => "page-addons".into(),
            Scene::Addons(AddonStatus::Downloading { .. }) => "page-addons-getting".into(),
            Scene::Addons(s) => format!("page-addons-{}", format!("{s:?}").to_lowercase()),
            Scene::Prayer => "page-prayer".into(),
            Scene::PrayerPicker => "page-prayer-picker".into(),
            Scene::PrayerCountdown => "prayer-countdown".into(),
            Scene::PrayerNotice => "prayer-notice".into(),
            Scene::PrayerPickerWorld => "page-prayer-picker-world".into(),
            Scene::PrayerWorld => "page-prayer-world".into(),
            Scene::HeadphonesHud => "headphones-hud".into(),
            Scene::UpdateSheet => "page-update-sheet".into(),
            Scene::UpdateReady => "page-update-ready".into(),
            Scene::Ai(p) => p.name().into(),
            Scene::Coding(n) => format!("page-coding-{n}"),
        }
    }
}

pub fn run(dir: &str, args: &[String]) -> i32 {
    let out = PathBuf::from(dir);
    if let Err(e) = std::fs::create_dir_all(&out) {
        eprintln!("render: {}: {e}", out.display());
        return 2;
    }
    let theme = match args.iter().position(|a| a == "--theme").and_then(|i| args.get(i + 1)).map(|s| s.as_str()) {
        Some("glass") => Theme::Glass,
        Some("pill") => Theme::Pill,
        _ => Theme::Classic,
    };
    let notch = args.iter().any(|a| a == "--notch").then_some(Notch::MACBOOK);
    let name = match theme {
        Theme::Classic => "",
        Theme::Glass => "-glass",
        Theme::Pill => "-pill",
    };
    let suffix = format!("{name}{}", if notch.is_some() { "-notch" } else { "" });
    // A home of its own, so add-ons on this Mac don't change the pictures.
    let home = std::env::temp_dir().join("poni-render-home");
    let _ = std::fs::remove_dir_all(&home);
    std::env::set_var("PONI_HOME", &home);
    let mut failed = 0;
    let mut gfx = Gfx::new(SCALE);
    for (i, scene) in SCENES.into_iter().chain(SYSTEM_SCENES).enumerate() {
        let path = out.join(format!("{:02}-{}{suffix}.png", i + 1, scene.name()));
        match render_scene(&mut gfx, scene, theme, notch, &path) {
            Ok(()) => println!("render: {}", path.display()),
            Err(e) => {
                eprintln!("render: {}: {e}", path.display());
                failed += 1;
            }
        }
    }
    // Pill: how the floating outline moves (stills can't show it), then its own looks.
    if crate::pill::on(theme) {
        let n = SCENES.len() + SYSTEM_SCENES.len() + 1;
        let path = out.join(format!("{n:02}-open-close-frames{suffix}.png"));
        match render_morph(&mut gfx, theme, notch, &path) {
            Ok(()) => println!("render: {}", path.display()),
            Err(e) => {
                eprintln!("render: {}: {e}", path.display());
                failed += 1;
            }
        }
        for (k, scene) in PILL_SCENES.into_iter().enumerate() {
            let path = out.join(format!("{:02}-{}{suffix}.png", n + 1 + k, scene.name()));
            match render_scene(&mut gfx, scene, theme, notch, &path) {
                Ok(()) => println!("render: {}", path.display()),
                Err(e) => {
                    eprintln!("render: {}: {e}", path.display());
                    failed += 1;
                }
            }
        }
    }
    // The Add-ons tab, Prayer times, Settings, the headphone pop-up, Power, Prayer times abroad, the Update sheet, Record and the AI tab in every theme, after Pill's.
    let first = SCENES.len() + SYSTEM_SCENES.len() + 2 + PILL_SCENES.len();
    let mut next = first;
    let later = SETTINGS_SCENES.into_iter().chain(HEADPHONE_SCENES).chain(POWER_SCENES).chain(PRAYER_WORLD_SCENES).chain(UPDATE_SCENES).chain(CODING_SCENES).chain(ai_scenes());
    for (k, scene) in ADDON_SCENES.into_iter().chain(PRAYER_SCENES).chain(later).enumerate() {
        let path = out.join(format!("{:02}-{}{suffix}.png", first + k, scene.name()));
        match render_scene(&mut gfx, scene, theme, notch, &path) {
            Ok(()) => println!("render: {}", path.display()),
            Err(e) => {
                eprintln!("render: {}: {e}", path.display());
                failed += 1;
            }
        }
        next = first + k + 1;
    }
    // The Settings window's sections, dark, two of them light, and Island
    // with Stay on top picked.
    for (k, (section, light, on_top)) in settings_shots().into_iter().enumerate() {
        let mode = if light { "-light" } else { "" };
        let pick = if on_top { "-on-top" } else { "" };
        let path = out.join(format!("{:02}-settings-window-{}{pick}{mode}{suffix}.png", next + k, section.key()));
        match render_settings(&mut gfx, section, theme, light, on_top, &path) {
            Ok(()) => println!("render: {}", path.display()),
            Err(e) => {
                eprintln!("render: {}: {e}", path.display());
                failed += 1;
            }
        }
    }
    let next = next + settings_shots().len();
    // Around a notch, last: the Eyes look's hide and seek with the camera.
    if let Some(n) = notch {
        let path = out.join(format!("{next:02}-peek-frames{suffix}.png"));
        match render_peek(&mut gfx, theme, n, &path) {
            Ok(()) => println!("render: {}", path.display()),
            Err(e) => {
                eprintln!("render: {}: {e}", path.display());
                failed += 1;
            }
        }
    }
    let _ = std::fs::remove_dir_all(&home);
    if failed > 0 {
        1
    } else {
        0
    }
}

/// The Settings window's pictures: every section dark, General and
/// Appearance light too, and Island with In full-screen apps set to Stay on
/// top (the website's full-screen chapter). Each is (section, light, Stay on top).
fn settings_shots() -> Vec<(crate::settings_window::Section, bool, bool)> {
    use crate::settings_window::Section;
    let mut v: Vec<_> = Section::ALL.into_iter().filter(|s| *s != Section::Prayer).map(|s| (s, false, false)).collect();
    v.extend([(Section::General, true, false), (Section::Appearance, true, false), (Section::Island, false, true)]);
    v
}

/// One section of the Settings window as the Mac shows it, light or dark,
/// with the system's window buttons where macOS puts them (the real window
/// draws those itself; the smoke test pictures that one). `on_top`: In
/// full-screen apps set to Stay on top instead of Hide.
fn render_settings(gfx: &mut Gfx, section: crate::settings_window::Section, theme: Theme, light: bool, on_top: bool, path: &Path) -> Result<(), String> {
    use crate::settings_host::{HEIGHT, TITLEBAR, WIDTH};
    let (w, h) = (WIDTH as f32, HEIGHT as f32);
    let (w_px, h_px) = ((w * SCALE) as usize, (h * SCALE) as usize);
    let bmp = Bitmap::new(w_px, h_px, SCALE).ok_or("no bitmap")?;
    let mut st = AppState::new(Settings { theme, hide_in_fullscreen: !on_top, ..Settings::default() }, Store::default());
    st.accent = crate::app::theme_accent();
    st.ui.autostart = Some(true);
    let mut view = crate::settings_window::View::new(section);
    let frame = crate::settings_window::Frame { card: Rect::new(0.0, 0.0, w, h), own_chrome: false, top: TITLEBAR, still: true };
    gfx.set_look(poni_core::look::glass(light));
    {
        let mut c = gfx.canvas(bmp.ctx());
        let _ = view.draw(&mut c, &mut st, NOW_MONO, frame, light);
        // Close, minimise and zoom, 12 points across and 20 apart from 20 in.
        c.unthemed(|c| {
            for (i, rgb) in [0xFF5F57u32, 0xFEBC2E, 0x28C840].into_iter().enumerate() {
                c.fill_circle(Point::new(20.0 + 6.0 + i as f32 * 20.0, TITLEBAR / 2.0), 6.0, Color::hex(rgb));
            }
        });
    }
    let px = bmp.pixels();
    crate::png::save_pbgra(path, w_px as u32, h_px as u32, &px)
}

/// The island opening from rest onto the Today page (left column) and
/// closing again (right), a frame every few sixtieths of a second, stepped on
/// the app's own springs: for the Pill theme, how the floating outline moves
/// and, around a notch, how it lets go of the edge and comes back.
fn render_morph(gfx: &mut Gfx, theme: Theme, notch: Option<Notch>, path: &Path) -> Result<(), String> {
    /// Sixtieths of a second after the change, one row each.
    const FRAMES: [usize; 12] = [0, 2, 4, 6, 8, 10, 13, 16, 20, 25, 30, 45];
    const DT: f32 = 1.0 / 60.0;
    /// The strip is drawn at one pixel per DIP (it is long).
    const STRIP_SCALE: f32 = 1.0;
    let mut st = base_state(theme);
    st.page = PageKind::Today;
    let (rest, open) = (Mode::Idle, Mode::Expanded(PageKind::Today));
    let size = |m: &Mode, st: &AppState| {
        let g = activities::geom(m, st);
        notch.map_or(g, |n| notch::geom(m, st, n, g))
    };
    let gap_at = |m: &Mode, st: &AppState| notch.map_or(Some(crate::pill::TOP_GAP), |n| notch::pill_gap(m, st, n));
    let ends = [(rest, size(&rest, &st), gap_at(&rest, &st)), (open, size(&open, &st), gap_at(&open, &st))];
    let menu_h = notch.map_or(MENU_BAR, |n| n.h);
    let cell_w = st.settings.expanded_width + 2.0 * MARGIN_X;
    let cell_h = (notch.map_or(0.0, |n| n.h) + crate::pill::TOP_GAP + ends[1].1.h * 1.06 + 28.0).ceil();
    let (w_px, h_px) = ((2.0 * cell_w * STRIP_SCALE).ceil() as usize, (FRAMES.len() as f32 * cell_h * STRIP_SCALE).ceil() as usize);
    let bmp = Bitmap::new(w_px, h_px, STRIP_SCALE).ok_or("no bitmap context")?;
    let look = poni_core::look::of(theme);
    for (col, (from, to)) in [(0usize, 1usize), (1, 0)].into_iter().enumerate() {
        let (_, g_from, gap_from) = ends[from];
        let (to_mode, g_to, gap_to) = ends[to];
        let mut island = Island::new(g_from);
        island.set_floating(gap_from);
        island.snap(g_from);
        // A first (still) frame, so the new gap below slides instead of snapping.
        island.step(0.0, NOW_MONO);
        island.set_target(g_to, NOW_MONO, true);
        island.set_floating(gap_to);
        let (mut k, mut now) = (0usize, NOW_MONO);
        for (row, &f) in FRAMES.iter().enumerate() {
            while k < f {
                island.step(DT, now);
                now += DT as f64;
                k += 1;
            }
            let (x, y) = (col as f32 * cell_w, row as f32 * cell_h);
            let cx = cell_w / 2.0;
            let body = island.body_rect_at(cx);
            let geom = island.current();
            let shoulder = notch.map_or(SHOULDER, |n| notch::shoulder(n, geom.w));
            let detach = notch::detach(island.top(), notch);
            gfx.set_look(&poni_core::look::CLASSIC);
            {
                let mut c = gfx.canvas(bmp.ctx());
                c.translate(x, y);
                c.clip(Rect::new(0.0, 0.0, cell_w, cell_h));
                let steps = 24;
                for i in 0..steps {
                    let t = i as f32 / (steps - 1) as f32;
                    let shade = Color::lerp(Color::hex(0x2B4F86), Color::hex(0xF0708A), t);
                    c.fill_rect(Rect::new(0.0, cell_h * i as f32 / steps as f32, cell_w, cell_h / steps as f32 + 0.5), shade);
                }
                c.fill_rect(Rect::new(0.0, 0.0, cell_w, menu_h), Color::rgba(0.96, 0.96, 0.97, 0.78));
                c.unclip();
                c.reset_transform();
            }
            gfx.set_look(look);
            {
                let mut c = gfx.canvas(bmp.ctx());
                c.translate(x, y);
                // Each frame keeps to its cell (the notch is drawn from above it).
                c.clip(Rect::new(0.0, 0.0, cell_w, cell_h));
                let border = Some(c.look().border);
                if island.floating() {
                    notch::draw_pill_body(&mut c, body, geom.r, shoulder, detach, palette::ISLAND, border);
                    c.push_outline_mask(&notch::pill_outline(body, geom.r, shoulder, detach));
                } else {
                    c.notch(body, geom.r, shoulder, palette::ISLAND, border);
                    c.push_shape_mask(body, geom.r, shoulder);
                }
                // The incoming content as it fades in (the app also fades the old one out).
                let lay = match to_mode {
                    Mode::Expanded(_) => Rect::new(body.center().x - g_to.w / 2.0, body.y, g_to.w, g_to.h),
                    _ => body,
                };
                let (mut input, mut hits) = (Input::default(), Vec::<Hit>::new());
                let alpha = island.content_alpha();
                c.with_alpha(alpha, |c| crate::app::draw_content(c, &mut input, &mut hits, &mut st, to_mode, lay, body, None, now, notch));
                c.pop_shape_mask();
                if let Some(n) = notch {
                    c.unthemed(|c| c.fill_rrect(Rect::new(cx - n.w / 2.0, -12.0, n.w, n.h + 12.0), 10.0, Color::black(1.0)));
                }
                let label = format!("{} {:.0} ms", if to == 1 { "open" } else { "close" }, f as f32 * 1000.0 / 60.0);
                c.unthemed(|c| c.text(&label, Rect::new(10.0, cell_h - 22.0, 160.0, 16.0), &crate::gfx::TextStyle::caption().color(Color::white(0.92))));
                c.unclip();
                c.reset_transform();
            }
        }
    }
    let px = bmp.pixels();
    crate::png::save_pbgra(path, w_px as u32, h_px as u32, &px)
}

/// The Eyes look around notch `n` as it moves (stills can't show it), a
/// frame a row, a fifteenth of a second apart: the pointer (drawn, so each
/// frame says what the eyes follow) wanders right of the camera and crosses
/// over; the eyes duck behind the camera and come out
/// on its left, follow it up into the menu bar and back under the camera.
fn render_peek(gfx: &mut Gfx, theme: Theme, n: Notch, path: &Path) -> Result<(), String> {
    /// The pointer from the island's top centre (DIPs) at each time
    /// (seconds), in a straight line from one to the next.
    const WAY: [(f64, f32, f32); 7] = [
        (0.0, 250.0, 118.0),
        (0.6, 170.0, 70.0),
        (1.2, -60.0, 95.0),
        (1.5, -130.0, 105.0),
        (2.4, -235.0, 80.0),
        (3.0, -215.0, 14.0),
        (3.6, -40.0, 118.0),
    ];
    const FPS: f64 = 15.0;
    const CELL: (f32, f32) = (560.0, 140.0);
    let mut st = base_state(theme);
    st.settings.idle_style = IdleStyle::Eyes;
    let mode = Mode::Idle;
    let g = notch::geom(&mode, &st, n, activities::geom(&mode, &st));
    let frames = (WAY[WAY.len() - 1].0 * FPS).round() as usize + 1;
    let (w_px, h_px) = ((CELL.0 * SCALE).ceil() as usize, (frames as f32 * CELL.1 * SCALE).ceil() as usize);
    let bmp = Bitmap::new(w_px, h_px, SCALE).ok_or("no bitmap context")?;
    let look = poni_core::look::of(theme);
    let at = |t: f64| {
        let i = WAY.iter().rposition(|w| w.0 <= t).unwrap_or(0).min(WAY.len() - 2);
        let (a, b) = (WAY[i], WAY[i + 1]);
        let k = ((t - a.0) / (b.0 - a.0)).clamp(0.0, 1.0) as f32;
        Point::new(a.1 + (b.1 - a.1) * k, a.2 + (b.2 - a.2) * k)
    };
    for f in 0..frames {
        let t = f as f64 / FPS;
        st.now = NOW_MONO + t;
        let p = at(t);
        st.ui.peek.point(p, st.now, n.w / 2.0);
        let (x, y) = (0.0, f as f32 * CELL.1);
        let cx = CELL.0 / 2.0;
        let mut island = Island::new(g);
        island.snap(g);
        let body = island.body_rect_at(cx);
        gfx.set_look(&poni_core::look::CLASSIC);
        {
            let mut c = gfx.canvas(bmp.ctx());
            c.translate(x, y);
            c.clip(Rect::new(0.0, 0.0, CELL.0, CELL.1));
            let steps = 24;
            for i in 0..steps {
                let k = i as f32 / (steps - 1) as f32;
                let shade = Color::lerp(Color::hex(0x2B4F86), Color::hex(0xF0708A), k);
                c.fill_rect(Rect::new(0.0, CELL.1 * i as f32 / steps as f32, CELL.0, CELL.1 / steps as f32 + 0.5), shade);
            }
            c.fill_rect(Rect::new(0.0, 0.0, CELL.0, n.h), Color::rgba(0.96, 0.96, 0.97, 0.78));
            c.unclip();
            c.reset_transform();
        }
        gfx.set_look(look);
        {
            let mut c = gfx.canvas(bmp.ctx());
            c.translate(x, y);
            c.clip(Rect::new(0.0, 0.0, CELL.0, CELL.1));
            let shoulder = notch::shoulder(n, g.w);
            if look.material == poni_core::look::Material::Glass {
                crate::glass::draw_body(&mut c, body, g.r, shoulder, Some(n), false, true);
            } else {
                let border = Some(c.look().border);
                c.notch(body, g.r, shoulder, palette::ISLAND, border);
            }
            let (mut input, mut hits) = (Input::default(), Vec::<Hit>::new());
            let now = st.now;
            crate::app::draw_content(&mut c, &mut input, &mut hits, &mut st, mode, body, body, None, now, Some(n));
            c.unthemed(|c| {
                c.fill_rrect(Rect::new(cx - n.w / 2.0, -12.0, n.w, n.h + 12.0), 10.0, Color::black(1.0));
                pointer_arrow(c, Point::new(cx + p.x, p.y));
            });
            c.unclip();
            c.reset_transform();
        }
    }
    let px = bmp.pixels();
    crate::png::save_pbgra(path, w_px as u32, h_px as u32, &px)
}

/// The Mac's arrow pointer, its tip at `tip`: black, outlined in white.
fn pointer_arrow(c: &mut crate::gfx::Canvas, tip: Point) {
    use poni_core::svgpath::Seg;
    const ARROW: [(f32, f32); 7] = [(0.0, 0.0), (0.0, 16.0), (4.0, 12.3), (6.9, 18.6), (9.6, 17.4), (6.8, 11.3), (12.0, 11.3)];
    let mut segs: Vec<Seg> = ARROW
        .iter()
        .enumerate()
        .map(|(i, (dx, dy))| if i == 0 { Seg::MoveTo(tip.x + dx, tip.y + dy) } else { Seg::LineTo(tip.x + dx, tip.y + dy) })
        .collect();
    segs.push(Seg::Close);
    c.path(&segs, Some(Color::white(1.0)), Some((Color::white(1.0), 2.4)));
    c.path(&segs, Some(Color::black(1.0)), None);
}

/// Prayer times on, in Kota Jakarta (found by the location), its clock 8
/// minutes before today's Ashar; `picking`: the place picker with "band"
/// typed. Returns Jakarta's site and today's Ashar (UTC seconds).
fn seed_prayer(st: &mut AppState, picking: bool) -> Result<(poni_core::prayer::Site, i64), String> {
    st.settings.set_addon(PRAYER_TIMES.id, true);
    st.addons.insert(PRAYER_TIMES.id, AddonStatus::Ready);
    let list = PlaceList::parse(SAMPLE_PLACES)?;
    st.prayer.place = list.find("jakarta").cloned();
    st.prayer.places = Some(std::sync::Arc::new(list));
    st.prayer.suggested = false;
    st.prayer.locate = Locate::Found { km: 2.0 };
    st.prayer.located_at = Some(st.unix);
    st.prayer.picking = picking;
    st.prayer.query = if picking { "band".into() } else { String::new() };
    // The website's "ed" file, as if it had come.
    let ed = World::parse(poni_core::prayer::world::SAMPLE.into())?;
    st.prayer.cities = [("ed".to_string(), CityFile::Here(std::sync::Arc::new(ed)))].into();
    let site = st.prayer.site().ok_or("no site for Kota Jakarta")?;
    let ashar = poni_core::prayer::day_times(&site, site.local_day(st.unix)).ok_or("no times today")?.ashar;
    st.prayer.clock_shift = ashar - 8 * 60 - st.unix;
    st.prayer.handled_until = Some(st.prayer_now());
    Ok((site, ashar))
}

/// Prayer times abroad: the place picker with "edin" typed (`picking`), or
/// the page for Edinburgh, picked by hand, 8 minutes before its Ashar.
fn seed_prayer_world(st: &mut AppState, picking: bool) -> Result<(), String> {
    seed_prayer(st, picking)?;
    if picking {
        st.prayer.query = "edin".into();
        return Ok(());
    }
    st.prayer.place = match st.prayer.cities.get("ed") {
        Some(CityFile::Here(w)) => w.place(0),
        _ => None,
    };
    st.settings.prayer.world_place = st.prayer.place.as_ref().and_then(|p| p.world_lines.clone());
    st.settings.prayer.follow_location = false;
    let site = st.prayer.site().ok_or("no site for Edinburgh")?;
    let ashar = poni_core::prayer::day_times(&site, site.local_day(st.unix)).ok_or("no times in Edinburgh today")?.ashar;
    st.prayer.clock_shift = ashar - 8 * 60 - st.unix;
    st.prayer.handled_until = Some(st.prayer_now());
    Ok(())
}

/// A few places from the add-on's list (as the Windows self-test has them).
const SAMPLE_PLACES: &str = r#"{"places":[
    {"id":"jakarta","label":"Kota Jakarta","kind":"kota","prov":"DKI Jakarta","lat":-6.1833,"lng":106.8333,"tz":"Asia/Jakarta"},
    {"id":"banda-aceh","label":"Kota Banda Aceh","kind":"kota","prov":"Aceh","lat":5.55,"lng":95.3167,"tz":"Asia/Jakarta"},
    {"id":"bandar-lampung","label":"Kota Bandar Lampung","kind":"kota","prov":"Lampung","lat":-5.4167,"lng":105.2667,"tz":"Asia/Jakarta"},
    {"id":"kab-bandung","label":"Kab. Bandung","kind":"kab","prov":"Jawa Barat","lat":-7.0167,"lng":107.5333,"tz":"Asia/Jakarta","h0":-2},
    {"id":"bandung","label":"Kota Bandung","kind":"kota","prov":"Jawa Barat","lat":-6.9,"lng":107.6167,"tz":"Asia/Jakarta","h0":-2},
    {"id":"kab-bandung-barat","label":"Kab. Bandung Barat","kind":"kab","prov":"Jawa Barat","lat":-6.85,"lng":107.5167,"tz":"Asia/Jakarta","h0":-2},
    {"id":"makkah","label":"Makkah","kind":"abroad","prov":"Arab Saudi","lat":21.4225,"lng":39.8261,"tz":"Asia/Riyadh","method":"ummalqura"}
]}"#;

fn base_state(theme: Theme) -> AppState {
    let settings = Settings { theme, sounds: false, show_media_activity: true, lyrics: true, ..Default::default() };
    let mut st = AppState::new(settings, Store::default());
    st.now = NOW_MONO;
    st.unix = NOW_UNIX;
    st.local = LocalDateTime { year: 2026, month: 10, day: 24, hour: 9, minute: 41, second: 0 };
    st.accent = palette::BLUE;
    st.volume = VolumeState { level: 0.64, muted: false, known: true };
    st.power = PowerState { has_battery: true, percent: 76, charging: false, on_ac: false, saver: false };
    st.stats = SysStats { valid: true, cpu: 0.23, mem_used: 9 << 30, mem_total: 16 << 30, disk_free: 212 << 30, disk_total: 476 << 30, uptime_s: 3 * 86_400 + 4 * 3600 };
    seed(&mut st);
    st
}

/// A later version than this build, with its DMG, for the Update sheet.
fn fake_release() -> poni_core::update::Release {
    let mut parts: Vec<u64> = crate::version::VERSION.split('.').filter_map(|p| p.parse().ok()).collect();
    if let Some(last) = parts.last_mut() {
        *last += 1;
    }
    let version = parts.iter().map(u64::to_string).collect::<Vec<_>>().join(".");
    let repo = "https://github.com/lucacadalora/oyenza-studio/releases";
    poni_core::update::Release {
        date: "2026-10-27".into(),
        headline: "Updates install themselves on a Mac".into(),
        notes: format!("{repo}/tag/v{version}"),
        windows: None,
        mac: Some(poni_core::update::Download { url: format!("{repo}/download/v{version}/Oyenza-Studio-{version}-mac.dmg"), size: 4_300_000, sha256: "0".repeat(64) }),
        version,
    }
}



fn seed(st: &mut AppState) {
    let unix = st.unix;
    let day_start = st.day_start_unix();
    let done = st.store.tasks.add("Water the balcony plants", unix - 5 * 3600);
    st.store.tasks.add("Sketch the onboarding screens", unix - 3 * 3600);
    st.store.tasks.add("Book the ferry for Saturday", unix - 2 * 3600);
    if let Some(id) = done {
        st.store.tasks.toggle(id, (unix - 1800).max(day_start + 60));
    }
    st.store.focus_sessions.insert(st.day_key(), 2);
}

fn media(st: &mut AppState) {
    let art = ImageData::new(240, 240, make_art(240));
    let color = art.accent_color();
    st.media = Some(MediaSession {
        app_id: "com.apple.Music".into(),
        app_name: "Music".into(),
        title: TITLE.into(),
        artist: ARTIST.into(),
        album: ALBUM.into(),
        art: Some(art),
        art_color: Some(color),
        timeline: Timeline::new(64.0, 201.0, NOW_MONO, true),
        can_prev: true,
        can_next: true,
        can_play_pause: true,
        can_seek: true,
        track_key: MediaSession::make_track_key(TITLE, ARTIST, ALBUM),
    });
    st.lyrics = LyricsState { track_key: MediaSession::make_track_key(TITLE, ARTIST, ALBUM), status: LyricsStatus::Found, lyrics: Some(parse_lrc(LYRICS_LRC)), source: "Sample".into() };
}

fn render_scene(gfx: &mut Gfx, scene: Scene, theme: Theme, notch: Option<Notch>, path: &Path) -> Result<(), String> {
    let mut st = base_state(theme);
    let mut hovering = false;
    let mode = match scene {
        Scene::IdleClock => Mode::Idle,
        // Around a notch the eyes look at the pointer: down at it in the
        // window under the menu bar (the eyes rest again a while after it
        // moved), and over the notch while it hovers there.
        Scene::IdleEyes => {
            st.settings.idle_style = IdleStyle::Eyes;
            if let Some(n) = notch {
                st.ui.peek.point(Point::new(240.0, 320.0), NOW_MONO - 10.0, n.w / 2.0);
            }
            Mode::Idle
        }
        Scene::Hover => {
            st.settings.idle_style = IdleStyle::Eyes;
            hovering = true;
            if let Some(n) = notch {
                st.ui.peek.point(Point::new(20.0, n.h / 2.0), NOW_MONO - 0.5, n.w / 2.0);
            }
            Mode::Idle
        }
        Scene::Timer => {
            st.pomodoro.phase = Phase::Focus;
            st.pomodoro.run = RunState::Running { ends_at: NOW_MONO + 1104.5 };
            Mode::Activity(activities::Activity::Timer)
        }
        Scene::Media => {
            media(&mut st);
            Mode::Activity(activities::Activity::Media)
        }
        Scene::FocusNotice => {
            st.pomodoro.phase = Phase::ShortBreak;
            st.notify(Icon::Coffee, palette::GREEN, "Focus done", "Time for a 5 minute break", 30.0, None);
            st.set_notice_inline(crate::state::InlineAction::StartFocus);
            Mode::Notice
        }
        Scene::VolumeHud => {
            st.hud = Some(Hud { kind: HudKind::Volume { level: 0.64, muted: false }, until: NOW_MONO + 30.0 });
            st.ui.hud_level = 0.64;
            Mode::Hud(activities::HudTag::Volume)
        }
        Scene::ChargingHud => {
            st.hud = Some(Hud { kind: HudKind::Charging { percent: 76, plugged: true }, until: NOW_MONO + 30.0 });
            Mode::Hud(activities::HudTag::Charging)
        }
        Scene::LowBatteryHud => {
            st.hud = Some(Hud { kind: HudKind::LowBattery { percent: 19 }, until: NOW_MONO + 30.0 });
            Mode::Hud(activities::HudTag::LowBattery)
        }
        Scene::HeadphonesHud => {
            st.hud = Some(Hud { kind: HudKind::Bluetooth { name: "AirPods Pro".into(), connected: true, battery: None, audio: true }, until: NOW_MONO + 30.0 });
            Mode::Hud(activities::HudTag::Bluetooth)
        }
        Scene::ShelfItems => {
            // Real files, with tiles from the Mac's own thumbnail code.
            let dir = std::env::temp_dir().join("poni-render-shelf");
            let folder = dir.join("Project assets");
            let _ = std::fs::create_dir_all(&folder);
            let notes = dir.join("Trip notes.txt");
            let _ = std::fs::write(&notes, "Weekend trip\n");
            let sketch = dir.join("Harbor sketch.png");
            let _ = crate::png::save_pbgra(&sketch, 240, 240, &make_art(240));
            let px = (crate::services::imaging::THUMB_DIP * SCALE).round() as u32;
            for (k, p) in [&folder, &notes, &sketch].into_iter().enumerate() {
                let path = p.to_string_lossy().into_owned();
                st.store.shelf.add_paths(&[path.as_str()], NOW_UNIX - 3600 + k as i64 * 600);
                let tile = crate::services::thumbs::make(&path, px);
                println!("render: shelf tile for {}: {}", p.display(), if tile.is_some() { "drawn" } else { "missing" });
                st.shelf_thumbs.insert(path, tile);
            }
            st.page = PageKind::Shelf;
            Mode::Expanded(PageKind::Shelf)
        }
        Scene::Page(p) => {
            if p == PageKind::Media {
                media(&mut st);
            }
            if p == PageKind::Focus {
                st.pomodoro.phase = Phase::Focus;
                st.pomodoro.run = RunState::Running { ends_at: NOW_MONO + 1104.5 };
            }
            st.page = p;
            Mode::Expanded(p)
        }
        Scene::UpdateSheet | Scene::UpdateReady => {
            st.update = crate::state::UpdateView { offer: Some(fake_release()), sheet: true, ready: scene == Scene::UpdateReady, ..Default::default() };
            st.page = PageKind::Settings;
            Mode::Expanded(PageKind::Settings)
        }

        Scene::Coding(n) => {
            use poni_core::wakatime::{Share, Summary};
            let share = |name: &str, secs| Share { name: name.into(), secs };
            st.settings.coding.track = n > 0;
            st.coding.plugins_ready = n > 1;
            st.coding.listening = n > 0;
            if n == 3 {
                st.coding.summary = Summary {
                    today: 7500,
                    week: [3600, 7200, 0, 5400, 9000, 1800, 7500],
                    week_total: 34_500,
                    month_total: 112_000,
                    ai_today: 2700,
                    ai_week: 9000,
                    editors: vec![share("Zed", 21_000), share("VS Code", 9000), share("Neovim", 4500)],
                    projects: vec![share("motchy", 18_000), share("oyenza-site", 9000), share("dotfiles", 4200)],
                    languages: vec![share("Rust", 21_000), share("Swift", 6000), share("Markdown", 3000)],
                    kept: 41,
                    since: "2026-09-26".into(),
                };
            }
            st.page = PageKind::Coding;
            Mode::Expanded(PageKind::Coding)
        }

        Scene::Ai(shows) => {
            let now = st.now;
            crate::pages::usage::picture(&mut st, shows, now);
            st.page = PageKind::Ai;
            Mode::Expanded(PageKind::Ai)
        }

        Scene::IdleStatus => {
            st.settings.idle_style = IdleStyle::Status;
            Mode::Idle
        }
        Scene::MediaNoLine => {
            // A long title: Pill floats it below a notch (the other themes
            // keep words out from under the camera).
            media(&mut st);
            if let Some(m) = st.media.as_mut() {
                m.title = "Paper Boats (Harbor Lights Session)".into();
            }
            st.settings.lyrics = false;
            st.lyrics = LyricsState::default();
            Mode::Activity(activities::Activity::Media)
        }
        Scene::System(m) => {
            // System is an add-on, off by default: on for its own scenes.
            st.settings.set_addon(poni_core::addons::SYSTEM.id, true);
            st.addons.insert(poni_core::addons::SYSTEM.id, AddonStatus::Ready);
            // Power is a desktop Mac's: no battery, the SMC's whole-Mac watts.
            st.system = sample_system(m == Module::Power);
            st.ui.system_module = m;
            if let Some(r) = st.system.latest.clone() {
                for a in r.top_cpu.iter().chain(&r.top_memory).chain(&r.top_io) {
                    if let Some(p) = &a.path {
                        let icon = app_icon(p);
                        st.app_icons.insert(p.clone(), icon);
                    }
                }
            }
            st.page = PageKind::System;
            Mode::Expanded(PageKind::System)
        }
        Scene::Addons(status) => {
            // Prayer times' row at `status` (a download's total filled in).
            let status = match status {
                AddonStatus::Downloading { done, .. } => AddonStatus::Downloading { done, total: PRAYER_TIMES.size() },
                s => s,
            };
            st.settings.set_addon(PRAYER_TIMES.id, status.is_on());
            st.addons.insert(PRAYER_TIMES.id, status);
            st.page = PageKind::Addons;
            Mode::Expanded(PageKind::Addons)
        }
        Scene::Prayer | Scene::PrayerPicker => {
            seed_prayer(&mut st, scene == Scene::PrayerPicker)?;
            st.page = PageKind::Prayer;
            Mode::Expanded(PageKind::Prayer)
        }
        Scene::PrayerCountdown => {
            seed_prayer(&mut st, false)?;
            Mode::Activity(activities::Activity::Prayer)
        }
        Scene::PrayerPickerWorld | Scene::PrayerWorld => {
            seed_prayer_world(&mut st, scene == Scene::PrayerPickerWorld)?;
            st.page = PageKind::Prayer;
            Mode::Expanded(PageKind::Prayer)
        }
        Scene::PrayerNotice => {
            let (site, ashar) = seed_prayer(&mut st, false)?;
            st.prayer.adzan = true;
            let place = st.prayer.place.as_ref().map_or(String::new(), |p| p.label.clone());
            let title = crate::pages::prayer::moment_title(Prayer::Ashar);
            st.notify(Icon::Moon, st.accent, title, format!("{} \u{b7} {place}", site.hhmm(ashar)), 30.0, Some(NoticeAction::OpenPage(PageKind::Prayer)));
            st.set_notice_inline(InlineAction::StopAdzan);
            Mode::Notice
        }
    };

    // The island at rest at its size for this state (as the app sizes it).
    // The Pill theme floats it below the top edge, where the app puts it.
    let size = |st: &AppState| {
        let mut g = activities::geom(&mode, st);
        if let Some(n) = notch {
            g = notch::geom(&mode, st, n, g);
        }
        let gap = if crate::pill::on(theme) { notch.map_or(Some(crate::pill::TOP_GAP), |n| notch::pill_gap(&mode, st, n)) } else { None };
        if hovering {
            g = if gap.is_some() { crate::pill::motion::hover_lift(g) } else { crate::island::Geom::new(g.w + 12.0, g.h + 4.0, g.r + 2.0) };
        }
        (g, gap)
    };
    // Pill's surfaces size themselves from what they last drew (a measured
    // text width) and ease in over their first frames (the song handing the
    // middle to its line): draw the scene twice, a moment before the
    // picture's, so it shows them as the app does a few frames in.
    if crate::pill::on(theme) {
        for t in [NOW_MONO - 0.4, NOW_MONO - 0.2] {
            st.now = t;
            let (g, gap) = size(&st);
            warm_up(gfx, &mut st, mode, notch, g, gap, hovering)?;
        }
        st.now = NOW_MONO;
    }
    let (g, gap) = size(&st);
    let mut island = Island::new(g);
    island.set_floating(gap);
    island.snap(g);

    let menu_h = notch.map_or(MENU_BAR, |n| n.h);
    let w_dip = (st.settings.expanded_width + 2.0 * MARGIN_X).max(g.w + 2.0 * (SHOULDER + MARGIN_X));
    let h_dip = (island.top() + g.h).max(menu_h) + MARGIN_BOTTOM + if matches!(mode, Mode::Expanded(_)) { 0.0 } else { 60.0 };
    let (w_px, h_px) = ((w_dip * SCALE).ceil() as usize, (h_dip * SCALE).ceil() as usize);
    let bmp = Bitmap::new(w_px, h_px, SCALE).ok_or("no bitmap context")?;
    let cx = w_dip / 2.0;
    let body = island.body_rect_at(cx);

    // The desktop: wallpaper and the menu bar.
    gfx.set_look(&poni_core::look::CLASSIC);
    {
        let mut c = gfx.canvas(bmp.ctx());
        let steps = 48;
        for i in 0..steps {
            let t = i as f32 / (steps - 1) as f32;
            let col = Color::lerp(Color::hex(0x2B4F86), Color::hex(0xF0708A), t);
            let y = h_dip * i as f32 / steps as f32;
            c.fill_rect(Rect::new(0.0, y, w_dip, h_dip / steps as f32 + 0.5), col);
        }
        c.fill_rect(Rect::new(0.0, 0.0, w_dip, menu_h), Color::rgba(0.96, 0.96, 0.97, 0.78));
    }

    // The island, drawn by the app's own content code (around a notch in
    // the look that meets its black).
    let look = poni_core::look::of(theme);
    gfx.set_look(look);
    {
        let mut c = gfx.canvas(bmp.ctx());
        let shoulder = notch.map_or(SHOULDER, |n| notch::shoulder(n, g.w));
        let inside = notch.is_some_and(|_| notch::fit(&mode, &st) == notch::Fit::Inside);
        if look.material == poni_core::look::Material::Glass {
            crate::glass::draw_body(&mut c, body, g.r, shoulder, notch, inside, true);
        } else {
            let border = if inside { None } else { Some(c.look().border) };
            if island.floating() {
                crate::pill::shape::draw_body(&mut c, body, g.r, palette::ISLAND, border);
            } else {
                c.notch(body, g.r, shoulder, palette::ISLAND, border);
            }
        }
        let mut input = Input::default();
        if hovering {
            input.mouse = Some(Point::new(body.center().x + 20.0, body.center().y));
        }
        let mut hits: Vec<Hit> = Vec::new();
        crate::app::draw_content(&mut c, &mut input, &mut hits, &mut st, mode, body, body, None, NOW_MONO, notch);
    }

    // The camera notch hides its part of the island on a real MacBook.
    if let Some(n) = notch {
        let mut c = gfx.canvas(bmp.ctx());
        c.unthemed(|c| c.fill_rrect(Rect::new(cx - n.w / 2.0, -12.0, n.w, n.h + 12.0), 10.0, Color::black(1.0)));
    }

    let px = bmp.pixels();
    crate::png::save_pbgra(path, w_px as u32, h_px as u32, &px)?;
    let _ = pages::HEADER_H;
    Ok(())
}

/// One frame of `mode` at `st.now`, drawn offscreen and thrown away, for the
/// state the drawing leaves behind (see `render_scene`).
#[allow(clippy::too_many_arguments)]
fn warm_up(gfx: &mut Gfx, st: &mut AppState, mode: Mode, notch: Option<Notch>, g: crate::island::Geom, gap: Option<f32>, hovering: bool) -> Result<(), String> {
    let mut island = Island::new(g);
    island.set_floating(gap);
    island.snap(g);
    let w = (st.settings.expanded_width + 2.0 * MARGIN_X).max(g.w + 2.0 * (SHOULDER + MARGIN_X));
    let h = island.top() + g.h + MARGIN_BOTTOM;
    let bmp = Bitmap::new((w * SCALE).ceil() as usize, (h * SCALE).ceil() as usize, SCALE).ok_or("no bitmap context")?;
    let body = island.body_rect_at(w / 2.0);
    let theme = st.settings.theme;
    gfx.set_look(poni_core::look::of(theme));
    let mut c = gfx.canvas(bmp.ctx());
    let mut input = Input::default();
    if hovering {
        input.mouse = Some(Point::new(body.center().x + 20.0, body.center().y));
    }
    let mut hits: Vec<Hit> = Vec::new();
    let now = st.now;
    crate::app::draw_content(&mut c, &mut input, &mut hits, st, mode, body, body, None, now, notch);
    Ok(())
}

// ---------------------------------------------------------------- the System page's sample minute

thread_local! {
    /// Each sample app's icon, drawn once a run.
    static ICONS: std::cell::RefCell<std::collections::HashMap<String, Option<crate::gfx::Image>>> = Default::default();
}

/// An app's icon the way the app gets it (`Command::LoadAppIcon`).
fn app_icon(path: &str) -> Option<crate::gfx::Image> {
    ICONS.with(|c| {
        c.borrow_mut()
            .entry(path.to_string())
            .or_insert_with(|| {
                let px = pages::system::icon_px(SCALE);
                let icon = crate::services::thumbs::app_icon(path, px).map(|i| pages::system::shrink_icon(&i, px));
                println!("render: app icon for {path}: {}", if icon.is_some() { "drawn" } else { "missing" });
                icon
            })
            .clone()
    })
}

/// Apps of the sample minute: name, bundle, CPU share, memory, disk bytes/s.
const SAMPLE_APPS: [(&str, &str, f32, u64, f64); 8] = [
    ("Safari", "/Applications/Safari.app", 0.091, 2_150 << 20, 1.2e6),
    ("Music", "/System/Applications/Music.app", 0.042, 412 << 20, 4.1e5),
    ("Mail", "/System/Applications/Mail.app", 0.018, 280 << 20, 8.2e5),
    ("Finder", "/System/Library/CoreServices/Finder.app", 0.013, 96 << 20, 4.0e4),
    ("Messages", "/System/Applications/Messages.app", 0.009, 210 << 20, 1.2e4),
    ("Xcode", "/Applications/Xcode.app", 0.008, 940 << 20, 9.6e4),
    ("Photos", "/System/Applications/Photos.app", 0.004, 520 << 20, 3.1e6),
    ("Terminal", "/System/Applications/Utilities/Terminal.app", 0.002, 88 << 20, 3.0e3),
];

fn sample_app(i: usize) -> AppUse {
    let (name, path, cpu, memory, io) = SAMPLE_APPS[i];
    AppUse { image: path.to_lowercase(), name: name.into(), path: Some(path.into()), pid: 400 + i as u64, cpu, memory, io, processes: if i == 0 { 9 } else { 1 } }
}

/// The System page's sample minute (all made up): a MacBook Air with an M2,
/// its four efficiency cores numbered before its four performance cores, as
/// Apple numbers them.
fn sample_system(desktop: bool) -> Monitor {
    const MIB: f64 = 1024.0 * 1024.0;
    let mut m = Monitor::default();
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
        let mut r = sample_reading(cpu, gpu, (read, write), (down, up), t);
        if desktop {
            // What the office Mac Studio's SMC read at rest (7.8-8.2 W), rising with the graphics.
            let w = if last { 8.1 } else { 7.4 + 24.0 * (gpu - 0.2).max(0.0) + 3.0 * cpu };
            r.battery = None;
            r.power = Some(PowerReading { total_w: w, cpu_w: None, gpu_w: None, rest_w: w });
        }
        m.push(r);
    }
    m
}

fn sample_reading(cpu: f32, gpu: f32, (read, write): (f64, f64), (down, up): (f64, f64), t: f32) -> Reading {
    const GIB: u64 = 1 << 30;
    let total = 16 * GIB;
    let cores = (0..8)
        .map(|k| {
            let p = k >= 4;
            let wave = 0.5 + 0.5 * (t / 3.0 + k as f32 * 1.7).sin();
            let load = if p { cpu * (0.6 + 1.1 * wave) } else { cpu * (0.3 + 0.5 * wave) };
            CoreLoad { load: load.clamp(0.02, 1.0), kind: if p { CoreKind::Performance } else { CoreKind::Efficiency } }
        })
        .collect();
    let (app, wired, compressed, cached) = (66 * GIB / 10, 21 * GIB / 10, 9 * GIB / 10, 42 * GIB / 10);
    let used = app + wired + compressed;
    let pressure = level_pressure(PressureLevel::Normal, 0.41);
    let apps = |order: [usize; 8]| order.iter().map(|&i| sample_app(i)).collect::<Vec<_>>();
    Reading {
        cpu: Some(CpuReading { load: cpu, user: cpu * 0.62, kernel: cpu * 0.38, cores, logical: 8, uptime_s: 3 * 86_400 + 4 * 3600 + 12 * 60, ..CpuReading::default() }),
        gpu: Some(GpuReading {
            name: "Apple M2".into(),
            util: gpu,
            engine_3d: gpu * 0.94,
            tiler: gpu * 0.41,
            mem_used: 13 * GIB / 10,
            mem_total: total,
            integrated: true,
            ..GpuReading::default()
        }),
        memory: Some(MemoryReading {
            total,
            used,
            committed: (f64::from(pressure) * total as f64).round() as u64,
            commit_limit: total,
            cached,
            wired,
            compressed,
            free: total - used - cached,
            page_used: 3 * GIB / 10,
            page_total: GIB,
        }),
        drives: vec![
            Drive { root: "Macintosh HD".into(), label: String::new(), total: 460 * GIB, free: 212 * GIB, system: true },
            Drive { root: "Backup".into(), label: String::new(), total: 931 * GIB, free: 640 * GIB, system: false },
        ],
        disk: Some(DiskActivity { read_bps: read, write_bps: write, active: None }),
        network: Some(NetReading {
            online: true,
            adapter: "Wi-Fi".into(),
            ipv4: Some("192.168.1.42".into()),
            mac: "3C:22:FB:0A:11:9E".into(),
            link_bps: 866_700_000,
            down_bps: down,
            up_bps: up,
            received: 34 * GIB / 10,
            sent: 410 << 20,
        }),
        battery: Some(BatteryReading {
            percent: 76,
            secs_left: Some(2 * 3600 + 14 * 60),
            design_mwh: Some(52_600),
            full_mwh: Some(48_390),
            cycles: Some(193),
            rate_mw: Some(-8_200),
            ..BatteryReading::default()
        }),
        top_cpu: apps([0, 1, 2, 3, 4, 5, 6, 7]),
        top_memory: apps([0, 5, 6, 1, 2, 4, 3, 7]),
        top_io: apps([6, 0, 2, 1, 5, 3, 4, 7]),
        power: None,
    }
}

// ---------------------------------------------------------------- sample album art

/// Procedural album art: a harbor sunset with a paper boat (premultiplied
/// BGRA, opaque). The same picture the Windows self-test uses.
pub(crate) fn make_art(size: u32) -> Vec<u8> {
    const SS: usize = 3;
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
    let mut c = if v < HORIZON { mix(rgb(0xFFB37A), rgb(0xE0607E), v / HORIZON) } else { mix(rgb(0x2F6F8F), rgb(0x13233F), (v - HORIZON) / (1.0 - HORIZON)) };
    let d = ((u - sun_x).powi(2) + (v - sun_y).powi(2)).sqrt();
    if v < HORIZON && d < sun_r {
        c = mix(c, rgb(0xFFE3A3), 0.92);
    }
    if v > HORIZON + 0.015 && v < HORIZON + 0.24 {
        let depth = (v - HORIZON) / 0.24;
        let band = ((v - HORIZON) * 42.0).fract();
        if band < 0.4 && (u - sun_x).abs() < 0.12 * (1.0 - depth) {
            c = mix(c, rgb(0xFFD08A), 0.6 * (1.0 - depth));
        }
    }
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

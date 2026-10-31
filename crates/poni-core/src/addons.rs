//! Add-ons: features that download only for the people who switch them on.
//!
//! An add-on's code is part of the app and stays asleep while it is off. The
//! switch downloads the add-on's files (data and sounds) from a pre-release
//! on the app's GitHub repository, and switching it off deletes them. Each
//! file's size and SHA-256 are built into the app, so a changed or broken
//! file is refused; new files come with a new app release (a new `tag`).
//! The apps' `services::addons` (one file, the Windows app's, which the Mac
//! app runs too) does the downloading.

use crate::sha256;

/// Where the files live: `<RELEASES>/<tag>/<name>`.
pub const RELEASES: &str = "https://github.com/lucacadalora/oyenza-studio/releases/download";

/// One file an add-on downloads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AddonFile {
    pub name: &'static str,
    pub size: u64,
    /// SHA-256, lowercase hex.
    pub sha256: &'static str,
    /// Where it comes from when that isn't the add-on's own release: a tool's
    /// own release, unchanged. Saved
    /// under `name` all the same.
    pub url: Option<&'static str>,
}

impl AddonFile {
    /// True when `bytes` are exactly this file.
    pub fn matches(&self, bytes: &[u8]) -> bool {
        bytes.len() as u64 == self.size && sha256::hex(&sha256::sha256(bytes)) == self.sha256
    }

    /// The same for a file read from `r` 64 KB at a time, so checking one on
    /// disk never holds all of it in memory (a tool can be 24 MB).
    pub fn matches_read(&self, mut r: impl std::io::Read) -> bool {
        let (mut hash, mut n, mut buf) = (sha256::Sha256::new(), 0u64, vec![0u8; 64 * 1024]);
        loop {
            match r.read(&mut buf) {
                Ok(0) => break,
                Ok(k) => {
                    n += k as u64;
                    if n > self.size {
                        return false;
                    }
                    hash.update(&buf[..k]);
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => return false,
            }
        }
        n == self.size && sha256::hex(&hash.finish()) == self.sha256
    }

    /// Whether this build of the app knows the file: a size and a SHA-256 to
    /// check it against. A placeholder (size 0, no SHA-256) stands for a file
    /// that isn't published yet, and nothing is downloaded for it.
    pub fn pinned(&self) -> bool {
        self.size > 0 && self.sha256.len() == 64 && self.sha256.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Addon {
    /// Stable id: the key in settings.json and the folder's name.
    pub id: &'static str,
    pub title: &'static str,
    /// What it does, in one line, for the Add-ons page.
    pub blurb: &'static str,
    /// The same in the Mac app, where `blurb` gives Windows' reason (None:
    /// `blurb` there too).
    pub mac_blurb: Option<&'static str>,
    /// The release that holds this version of the files.
    pub tag: &'static str,
    pub files: &'static [AddonFile],
    /// Part of Oyenza Studio Pro: it works only for an Oyenza account the
    /// server says has Pro (`GET /island/pro`; `AppState::pro`).
    pub pro: bool,
}

impl Addon {
    /// Every file together, in bytes.
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    pub fn url(&self, file: &AddonFile) -> String {
        match file.url {
            Some(url) => url.to_string(),
            None => format!("{RELEASES}/{}/{}", self.tag, file.name),
        }
    }

    pub fn find(id: &str) -> Option<&'static Addon> {
        ALL.iter().copied().find(|a| a.id == id)
    }

    /// Whether it has files to download. One without (Script reader) is
    /// already in the app: the switch only shows or hides it, and the
    /// downloader never touches it.
    pub fn downloads(&self) -> bool {
        !self.files.is_empty()
    }

    /// Whether this build can get every file it needs ([`AddonFile::pinned`]).
    /// One that can't stays off ([`Status::Unpinned`]): a build made while a
    /// file is still a placeholder (a new add-on whose files aren't on its
    /// release yet).
    pub fn pinned(&self) -> bool {
        self.files.iter().all(AddonFile::pinned)
    }
}

/// Prayer times: the place list and the adzan (`addons/sholat/` in the repo).
pub const PRAYER_TIMES: Addon = Addon {
    id: "sholat",
    title: "Prayer times",
    blurb: "Today's times for your city and a reminder in the island.",
    mac_blurb: None,
    tag: "addon-sholat-1",
    files: &[
        AddonFile { name: "places.json", size: 79_687, sha256: "4d2e1cd6c277b97273f415cf0460579db143fe92ac88f6e4bddb79cdf8cbf69b", url: None },
        AddonFile { name: "adzan.m4a", size: 1_822_513, sha256: "bd5a9db9d23eb2fb7e3368c33afeb08603b71b762549ba8247491c5aac24cb59", url: None },
        AddonFile { name: "NOTICE.txt", size: 869, sha256: "a1762a46d2ff3c7456070f76fe370e02e169f30ee749fea7763568034401185a", url: None },
    ],
    pro: false,
};

/// Script reader: the Script page, a teleprompter under the webcam. Its
/// code is in the app (a few dozen KB), so there is nothing to download; off,
/// the island has no Script tab, no Alt+Shift+S and nothing running for it.
pub const SCRIPT_READER: Addon = Addon {
    id: "script",
    title: "Script reader",
    blurb: "A teleprompter under your webcam, left out of screen shares.",
    mac_blurb: Some("A teleprompter under your webcam."),
    tag: "",
    files: &[],
    pro: false,
};

/// System: the System page (the processor, graphics, memory, drives,
/// network and power, and the apps using them). A tab for everyone up to
/// 0.2.8; an add-on, off, in the versions after it, so the island's own tabs
/// are the everyday ones. Its code is in the app, so there is nothing to download.
/// Off, there is no System tab, and its readings never start at rest
/// (`Settings::system_history` only counts while it is on).
pub const SYSTEM: Addon = Addon {
    id: "system",
    title: "System",
    blurb: "Live CPU, GPU, memory, drives, network and power, and the apps using them.",
    mac_blurb: None,
    tag: "",
    files: &[],
    pro: false,
};

pub const NOTIFICATIONS: Addon = Addon {
    id: "notifications",
    title: "Notifications",
    blurb: "Messages from WhatsApp, Telegram and your phone in the island.",
    mac_blurb: None,
    tag: "",
    files: &[],
    pro: false,
};

/// Screen recorder: the Record page and the recording pill, the face of
/// Oyenza Studio Recorder (its own install, about 120 MB). Its code is in the
/// island, so there is nothing to download. It shows on the Add-ons page once
/// the recorder is installed, and is on from then until switched off
/// (`Settings::recorder_on`). Off, there is no Record tab, and the recorder
/// shows its own bar and controls.
pub const SCREEN_RECORDER: Addon = Addon {
    id: "recorder",
    title: "Screen recorder",
    blurb: "Record your screen from the island, with Oyenza Studio Recorder.",
    mac_blurb: None,
    tag: "",
    files: &[],
    pro: false,
};

/// Meeting notes: the Notes page (`crate::notes`). One click and Oyen listens
/// to the microphone and the computer's own sound (a call in Meet, Zoom,
/// Teams; on a Mac through a Core Audio tap, macOS 14.2 and later), then the
/// Oyenza account writes the notes. Its code is in the
/// app, so there is nothing to download. Off, there is no Notes tab and
/// nothing of it runs: no recording, no calls. Part of Studio Pro: the page
/// records only for an account the server says has Pro, and the server
/// writes notes only for one.
pub const MEETING_NOTES: Addon = Addon {
    id: "notes",
    title: "Meeting notes",
    blurb: "One click and Oyen listens to your meeting, Meet, Zoom or Teams, and writes the notes.",
    mac_blurb: None,
    tag: "",
    files: &[],
    pro: true,
};

/// Every add-on, in the order the Add-ons page lists them.
pub const ALL: [&Addon; 6] =
    [&PRAYER_TIMES, &SCRIPT_READER, &SYSTEM, &NOTIFICATIONS, &MEETING_NOTES, &SCREEN_RECORDER];

/// Where an add-on stands, as the Add-ons page shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Switched off: none of its files on this PC, nothing running.
    Off,
    /// Switched on; looking at the files already on this PC.
    Checking,
    /// Bytes so far of the add-on's total.
    Downloading { done: u64, total: u64 },
    /// Switched on with no internet: the download starts by itself later.
    Waiting,
    /// Switched on, every file in place.
    Ready,
    /// A file came back different from the one the app expects, so it was
    /// thrown away. Trying again is the person's call (off, then on).
    Failed,
    /// Asked to switch on, but this build can't get its files yet
    /// ([`Addon::pinned`]): it stays off, and nothing downloads.
    Unpinned,
}

impl Status {
    /// The switch shows on for everything but `Off`, and `Unpinned`, which
    /// stays off.
    pub fn is_on(self) -> bool {
        !matches!(self, Status::Off | Status::Unpinned)
    }

    /// The line under the add-on's name; `mac`: in the Mac app.
    pub fn line(self, addon: &Addon, mac: bool) -> String {
        let here = if mac { "this Mac" } else { "this PC" };
        match self {
            Status::Off => addon.mac_blurb.filter(|_| mac).unwrap_or(addon.blurb).to_string(),
            Status::Checking => format!("Checking its files on {here}"),
            Status::Downloading { done, total } => format!("Downloading · {} of {}", fmt_bytes(done), fmt_bytes(total)),
            Status::Waiting => "No internet. It downloads by itself once you're back online.".to_string(),
            Status::Ready if !addon.downloads() => "On".to_string(),
            Status::Ready => format!("On · {} on {here}", fmt_bytes(addon.size())),
            Status::Failed => "The download didn't come through right. Switch it off and on to try again.".to_string(),
            Status::Unpinned => "This version can't get its files yet, so it stays off.".to_string(),
        }
    }

    /// Download progress 0..1, while downloading.
    pub fn progress(self) -> Option<f32> {
        match self {
            Status::Downloading { done, total } if total > 0 => Some((done as f64 / total as f64).clamp(0.0, 1.0) as f32),
            Status::Downloading { .. } => Some(0.0),
            _ => None,
        }
    }
}

/// "4.1 MB", "820 KB", "12 bytes": sizes as the island shows them.
pub fn fmt_bytes(n: u64) -> String {
    const K: f64 = 1000.0;
    let f = n as f64;
    if n < 1000 {
        format!("{n} {}", if n == 1 { "byte" } else { "bytes" })
    } else if f < K * K {
        format!("{:.0} KB", f / K)
    } else if f < K * K * K {
        let mb = f / (K * K);
        if mb < 10.0 {
            format!("{mb:.1} MB")
        } else {
            format!("{mb:.0} MB")
        }
    } else {
        format!("{:.1} GB", f / (K * K * K))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn formats_sizes() {
        assert_eq!(fmt_bytes(0), "0 bytes");
        assert_eq!(fmt_bytes(1), "1 byte");
        assert_eq!(fmt_bytes(999), "999 bytes");
        assert_eq!(fmt_bytes(820_400), "820 KB");
        assert_eq!(fmt_bytes(4_120_000), "4.1 MB");
        assert_eq!(fmt_bytes(84_000_000), "84 MB");
        assert_eq!(fmt_bytes(1_260_000_000), "1.3 GB");
    }

    use super::*;
    use crate::settings::Settings;

    #[test]
    fn script_reader_is_built_in() {
        assert!(!SCRIPT_READER.downloads() && SCRIPT_READER.size() == 0);
        assert!(PRAYER_TIMES.downloads() && !SCRIPT_READER.downloads());
        assert_eq!(Status::Ready.line(&SCRIPT_READER, false), "On");
        assert_eq!(Status::Off.line(&SCRIPT_READER, true), "A teleprompter under your webcam.");
        assert_eq!(Addon::find("script"), Some(&SCRIPT_READER));
    }

    #[test]
    fn pro_is_meeting_notes() {
        let pro: Vec<&str> = ALL.iter().filter(|a| a.pro).map(|a| a.id).collect();
        assert_eq!(pro, vec!["notes"]);
    }

    #[test]
    fn built_in_add_ons_are_off_until_switched_on() {
        for a in [&SYSTEM, &NOTIFICATIONS, &MEETING_NOTES] {
            assert!(!a.downloads() && a.size() == 0, "{}", a.id);
            assert_eq!(Addon::find(a.id), Some(a));
            assert_eq!(Status::Ready.line(a, false), "On");
            assert!(!Settings::default().addon_on(a.id), "{} is off for everyone until they switch it on", a.id);
        }
        // Clipboard history left the app after 0.2.8: no add-on by that id.
        assert_eq!(Addon::find("clipboard"), None);
    }

    #[test]
    fn screen_recorder_is_on_once_the_recorder_is_installed() {
        assert!(!SCREEN_RECORDER.downloads() && SCREEN_RECORDER.size() == 0);
        assert_eq!(Addon::find("recorder"), Some(&SCREEN_RECORDER));
        let mut s = Settings::default();
        assert!(!s.recorder_on(false), "no recorder: nothing to show");
        assert!(s.recorder_on(true), "installing the recorder is the switch");
        s.set_addon(SCREEN_RECORDER.id, false);
        assert!(!s.recorder_on(true), "switched off by hand: stays off");
        s.set_addon(SCREEN_RECORDER.id, true);
        assert!(s.recorder_on(true) && !s.recorder_on(false));
    }

    #[test]
    fn prayer_times_files_are_the_ones_in_the_repo() {
        // The pinned fingerprints must match addons/sholat/, which is what the release holds.
        let repo: [(&str, &[u8]); 3] = [
            ("places.json", include_bytes!("../../../addons/sholat/places.json")),
            ("adzan.m4a", include_bytes!("../../../addons/sholat/adzan.m4a")),
            ("NOTICE.txt", include_bytes!("../../../addons/sholat/NOTICE.txt")),
        ];
        assert_eq!(PRAYER_TIMES.files.len(), repo.len());
        for (f, (name, bytes)) in PRAYER_TIMES.files.iter().zip(repo) {
            assert_eq!(f.name, name);
            assert!(f.matches(bytes), "{name}: size {} sha256 {}", bytes.len(), sha256::hex(&sha256::sha256(bytes)));
        }
        let mut broken = repo[2].1.to_vec();
        broken[0] ^= 1;
        assert!(!PRAYER_TIMES.files[2].matches(&broken));
        assert!(!PRAYER_TIMES.files[2].matches(&repo[2].1[1..]));
    }

    #[test]
    fn files_come_from_the_release() {
        let a = Addon::find("sholat").unwrap();
        assert_eq!(a.url(&a.files[1]), "https://github.com/lucacadalora/oyenza-studio/releases/download/addon-sholat-1/adzan.m4a");
        assert_eq!(a.size(), 1_903_069);
        assert!(Addon::find("nope").is_none());
    }

    #[test]
    fn lines_say_what_is_happening() {
        let a = &PRAYER_TIMES;
        assert_eq!(Status::Off.line(a, false), a.blurb);
        assert_eq!(Status::Off.line(a, true), a.blurb, "no Mac line of its own: the same");
        assert_eq!(Status::Downloading { done: 1_300_000, total: a.size() }.line(a, false), "Downloading · 1.3 MB of 1.9 MB");
        assert_eq!(Status::Ready.line(a, false), "On · 1.9 MB on this PC");
        assert!(!Status::Off.is_on() && Status::Waiting.is_on() && Status::Failed.is_on());
        assert_eq!(Status::Downloading { done: 1, total: 4 }.progress(), Some(0.25));
        assert_eq!(Status::Downloading { done: 0, total: 0 }.progress(), Some(0.0));
        assert_eq!(Status::Ready.progress(), None);
    }

    #[test]
    fn add_ons_are_listed_in_order_each_with_its_own_id() {
        // The Add-ons page's order.
        let ids: Vec<&str> = ALL.iter().map(|a| a.id).collect();
        assert_eq!(ids, ["sholat", "script", "system", "notifications", "notes", "recorder"]);
        for (i, a) in ALL.iter().enumerate() {
            // The key in settings.json and the folder's name.
            assert!(!a.id.is_empty() && a.id.bytes().all(|b| b.is_ascii_lowercase()), "{}", a.id);
            assert!(ALL[..i].iter().all(|b| b.id != a.id && b.title != a.title), "{} twice", a.id);
            assert_eq!(Addon::find(a.id), Some(*a));
            assert_eq!(a.downloads(), !a.tag.is_empty(), "{}: a release exactly when there are files", a.id);
            // Each file a name of its own in the folder, and none of the
            // updater's own there (its `.ok` stamp, a `.part` on its way).
            for (j, f) in a.files.iter().enumerate() {
                assert!(a.files[..j].iter().all(|g| g.name != f.name), "{}: {} twice", a.id, f.name);
                assert!(!f.name.starts_with('.') && !f.name.ends_with(".part") && !f.name.contains(['/', '\\']), "{}: {}", a.id, f.name);
            }
        }
    }

    #[test]
    fn an_add_on_with_a_placeholder_file_stays_off() {
        const SOON: Addon = Addon {
            id: "soon",
            title: "Soon",
            blurb: "Not out yet.",
            mac_blurb: None,
            tag: "addon-soon-1",
            files: &[
                AddonFile { name: "tool.exe", size: 2_151_363, sha256: "2aeabf0092c3262d6b2609824418f7dd7ed1f1df939f73b2b15645230cac0d77", url: None },
                AddonFile { name: "NOTICE.txt", size: 0, sha256: "", url: None },
            ],
            pro: false,
        };
        let tool = SOON.files[0];
        assert!(tool.pinned() && !SOON.files[1].pinned() && !SOON.pinned());
        // Half a pin is none.
        for f in [
            AddonFile { sha256: "", ..tool },
            AddonFile { size: 0, ..tool },
            AddonFile { sha256: &tool.sha256[1..], ..tool },
            AddonFile { sha256: "2AEABF0092C3262D6B2609824418F7DD7ED1F1DF939F73B2B15645230CAC0D77", ..tool },
        ] {
            assert!(!f.pinned(), "{f:?}");
        }
        // Everything in the app has its files pinned (built-ins have none to
        // pin), in both apps.
        for a in ALL.iter().copied() {
            assert!(a.pinned(), "{} ({})", a.id, a.tag);
        }
        // The switch stays off and the line says why; nothing comes in.
        let s = Status::Unpinned;
        assert!(!s.is_on() && s.progress().is_none());
        assert_eq!(s.line(&SOON, false), "This version can't get its files yet, so it stays off.");
        assert_eq!(s.line(&SOON, true), s.line(&SOON, false));
    }

    #[test]
    fn a_file_on_disk_is_checked_a_piece_at_a_time() {
        // The prayer add-on's notice, read in pieces against its pin.
        let dict: &[u8] = include_bytes!("../../../addons/sholat/NOTICE.txt");
        let f = PRAYER_TIMES.files.iter().find(|f| f.name == "NOTICE.txt").unwrap();
        assert!(f.matches_read(dict));
        let mut changed = dict.to_vec();
        *changed.last_mut().unwrap() ^= 1;
        assert!(!f.matches_read(&changed[..]));
        assert!(!f.matches_read(&dict[..dict.len() - 1]), "a byte short");
        assert!(!f.matches_read(std::io::Read::chain(dict, &b"!"[..])), "a byte more");
        // A file that can't be read to its end isn't the file.
        struct Unplugged;
        impl std::io::Read for Unplugged {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("unplugged"))
            }
        }
        assert!(!f.matches_read(Unplugged));
    }

    #[test]
    fn a_big_add_on_reads_well() {
        // A big add-on's size: whole megabytes from 10 MB up.
        const BIG: Addon = Addon {
            id: "big",
            title: "Big",
            blurb: "Big tools.",
            mac_blurb: None,
            tag: "addon-big-1",
            files: &[AddonFile { name: "big.exe", size: 30_100_000, sha256: "f5389d3698ecde4ef06f4e6b1ce0294fe9ac1396228c8990b8d3416d6d177a7d", url: None }],
            pro: false,
        };
        assert_eq!(fmt_bytes(BIG.size()), "30 MB", "its size on the page while it's off");
        assert_eq!(Status::Downloading { done: 12_345_678, total: BIG.size() }.line(&BIG, false), "Downloading · 12 MB of 30 MB");
        assert_eq!(Status::Ready.line(&BIG, false), "On · 30 MB on this PC");
    }
}

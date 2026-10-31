//! Messages from service threads to the UI (main) thread.
//!
//! Services hold an [`EventSink`]. `send` pushes onto an mpsc channel and
//! queues a drain on the main dispatch queue; the main thread then applies
//! every waiting event. The event types match the Windows app's, so the
//! services that arrive in later slices plug in the same way.

use crate::gfx::Image;
use crate::state::{MediaSession, PowerState, SysStats};
use poni_core::lrc::Lyrics;
use poni_core::timeline::Timeline;
use std::sync::mpsc::Sender;

#[derive(Clone)]
pub struct EventSink {
    tx: Sender<AppEvent>,
}

impl EventSink {
    pub fn new(tx: Sender<AppEvent>) -> Self {
        Self { tx }
    }

    pub fn send(&self, ev: AppEvent) {
        if self.tx.send(ev).is_ok() {
            dispatch2::DispatchQueue::main().exec_async(|| {
                crate::app::drain_events();
            });
        }
    }
}

/// What Apple Music said about the user's token.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppleMusicStatus {
    Accepted,
    Rejected,
    Unreachable,
    NoAccess,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum LyricsResult {
    Found { lyrics: Lyrics, source: String },
    Instrumental,
    NotFound,
    Error(String),
}

#[allow(dead_code)]
pub enum AppEvent {
    /// The current media session changed (None = nothing playing / no session).
    Media(Option<MediaSession>),
    /// Only the timeline/playback state of the current track changed.
    MediaTimeline { track_key: String, timeline: Timeline },
    /// Album art finished decoding for a track.
    MediaArt { track_key: String, art: Option<Image> },
    Lyrics { track_key: String, result: LyricsResult },
    AppleMusic(AppleMusicStatus),
    /// Master volume changed (0..=1). `switched`: the output device changed
    /// (headphones connected, say), not its volume.
    Volume { level: f32, muted: bool, switched: bool },
    Power(PowerState),
    Bluetooth { name: String, connected: bool, battery: Option<u8>, audio: bool },
    /// A full-screen app took over (or left) the island's screen.
    Fullscreen(bool),
    /// Files dropped on the island.
    ShelfAdd(Vec<String>),
    /// Files are being dragged over the island (true) or left (false).
    DragHover(bool),
    ShelfThumb { path: String, image: Option<Image> },
    Stats(SysStats),
    /// The System page's latest reading and the minute behind it
    /// (`services::sysmon`, each second while the page is open).
    System(Box<poni_core::sysmon::Monitor>),
    /// An app's icon for the System page, by its path (`Command::LoadAppIcon`).
    AppIcon { path: String, image: Option<Image> },
    /// The displays went to sleep (true) or woke.
    ScreensAsleep(bool),
    /// The Mac was locked (true) or unlocked.
    Locked(bool),
    /// Where an add-on's files stand (`services::addons`).
    Addon { id: &'static str, status: poni_core::addons::Status },
    /// What the update file said (`services::update`).
    Update(crate::services::update::UpdateEvent),
    /// Where the Mac is, for Prayer times (`services::location`).
    Located(crate::services::location::Located),
    /// One of Prayer times' online city files (`services::cities`).
    Cities { file: String, got: Result<poni_core::prayer::World, String> },
    /// The AI tab's readings, typed keys and
    /// heads-up notices (`services::usage`).
    Usage(crate::services::usage::UsageEvent),
    /// Prayer times' adzan played to its end (`services::adzan`).
    AdzanEnded,
    /// Free-form log line from a service thread.
    Log(String),
}

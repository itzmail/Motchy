//! Side effects requested by pages; executed by the app after each frame.

use crate::pages::PageKind;
use studio_usage::ProviderId;

#[derive(Clone, Debug, PartialEq)]
pub enum MediaCmd {
    PlayPause,
    Next,
    Prev,
    /// Seek to absolute position in seconds.
    Seek(f64),
    /// The pointer reached the progress bar or the back button: get ready
    /// (Apple Music's own controls are looked up before the press, not during it).
    Prepare,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sound {
    /// Gentle chime (timer finished, reminder).
    Chime,
    /// Short tick (button feedback) — currently unused.
    Tick,
}

/// Requests for the Notifications add-on (`services::notifications`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotifyCmd {
    /// Ask Windows to let the island read notifications (its question).
    Ask,
    /// Look again at what Windows allows: the page opened, and the person
    /// may have changed it in Windows' settings meanwhile.
    Check,
    /// Open Windows' settings for which apps may read notifications.
    OpenWindowsSettings,
}

/// What the AI tab asks for. Hide and Show change `settings.usage`; the rest
/// go to its worker (`services::usage`).
#[derive(Clone, PartialEq)]
pub enum UsageCmd {
    /// Read every service now.
    Refresh,
    /// Keep a key typed for a service, sealed (DPAPI / the keychain), under
    /// its secret name (`usage::KeyAsk::secret`).
    SetKey { secret: &'static str, key: String },
    /// Forget a typed key.
    ClearKey(&'static str),
    /// Start a service's own sign-in on this computer (Claude Code's
    /// `claude auth login`, or the Claude app): `studio_usage::claude::sign_in`.
    SignIn(ProviderId),
    /// Take a service's card off the tab.
    Hide(ProviderId),
    /// Put it back, even when nothing of it is on this computer (a service
    /// that takes a typed key).
    Show(ProviderId),
}

/// Never shows a typed key (a command may end up in the log).
impl std::fmt::Debug for UsageCmd {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UsageCmd::Refresh => f.write_str("Refresh"),
            UsageCmd::SetKey { secret, .. } => write!(f, "SetKey({secret})"),
            UsageCmd::ClearKey(secret) => write!(f, "ClearKey({secret})"),
            UsageCmd::SignIn(id) => write!(f, "SignIn({})", id.key()),
            UsageCmd::Hide(id) => write!(f, "Hide({})", id.key()),
            UsageCmd::Show(id) => write!(f, "Show({})", id.key()),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Command {
    Media(MediaCmd),
    /// Open a file/folder with its default handler.
    OpenPath(String),
    /// Show a file in Explorer.
    RevealPath(String),
    /// Begin an OLE drag of these paths out of the island.
    DragOut(Vec<String>),
    /// Request a thumbnail for a shelf item (async; arrives as `AppEvent::ShelfThumb`).
    LoadThumb(String),
    /// Give the island keyboard focus for a text field.
    FocusText,
    /// The Script page's Paste: the clipboard's text, line breaks kept,
    /// becomes the script.
    ScriptPaste,
    /// The Script page's Record: Oyenza Studio Recorder records the screen
    /// the island is on (`services::recorder`).
    RecordScreen,
    /// The Record page's Record: a recording of `settings.record`'s target
    /// with its camera, microphone and system sound (`services::recorder`).
    Record,
    /// Pause, resume or stop the recording under way (the Record page).
    RecordPause,
    RecordResume,
    RecordStop,
    /// Drop keyboard focus.
    BlurText,
    /// Persist `AppState::store` soon.
    SaveStore,
    /// Persist `AppState::settings` now (after a page toggled a setting).
    SaveSettings,
    /// Open the Settings window, or bring it forward (the gear, Windows).
    OpenSettings,
    SetPage(PageKind),
    /// Keep the island open even when the cursor leaves.
    SetPinned(bool),
    Collapse,
    Play(Sound),
    /// Open a URL in the default browser.
    OpenUrl(String),
    /// Update to the version the update file offers: install it (Windows,
    /// once its Setup is downloaded), or its release page or DMG.
    Update,
    /// Install it the next time the app starts (Windows).
    InstallOnNextStart,
    /// Settings' Check now: read the update file now, as the menu's Check
    /// for updates… does (`AppState::update.said` takes the answer).
    CheckForUpdates,
    /// Start/stop the 1 Hz system-stats sampler.
    WantStats(bool),
    /// Start with Windows on or off (the Settings page's switch).
    SetAutostart(bool),
    /// Open the System page on this module (a Today ring was clicked).
    OpenSystem(poni_core::sysmon::Module),
    /// Request an app's icon for the System page (arrives in `AppState::app_icons`).
    LoadAppIcon(String),
    /// Get one of Prayer times' online city files ("ed", "near/55_-5"; the
    /// answer: `AppEvent::Cities`).
    GetCities(String),
    /// The Notifications add-on.
    Notify(NotifyCmd),
    Usage(UsageCmd),
}


//! Platform-independent models and logic for Poni.
//!
//! Everything here is pure Rust with no OS calls so it can be unit-tested on
//! any machine. The Windows crate (`poni`) owns all I/O and rendering and
//! feeds clocks/dates into these types. `store` (the JSON files) uses `std`
//! I/O, which still runs anywhere.

pub mod addons;
pub mod applemusic;
pub mod brand;
pub mod datetime;
pub mod frametime;
pub mod geom;
pub mod gitsync;
pub mod glass;
pub mod hdr;
pub mod look;
pub mod lrc;
pub mod lrclib;
pub mod mediaremote;
pub mod notifications;
pub mod nowplaying;
pub mod peek;
pub mod pomodoro;
pub mod power;
pub mod prayer;
pub mod script;
pub mod settings;
pub mod sha256;
pub mod shelf;
pub mod spot;
pub mod spring;
pub mod store;
pub mod svgpath;
pub mod sysmon;
pub mod timeline;
pub mod todo;
pub mod ttml;
pub mod update;
pub mod urlenc;
pub mod wakatime;

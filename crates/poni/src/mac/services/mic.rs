//! The error the Mac's recording sources share (`services::capture`, and
//! `services::notes` through it): macOS's microphone question and what can
//! go wrong opening a source.

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MicError {
    /// macOS says no: the person turned it down (now or in System Settings),
    /// or a profile on this Mac doesn't allow it.
    Denied,
    /// No microphone: none built in, none plugged in.
    NoDevice,
    Failed(String),
}

impl std::fmt::Display for MicError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MicError::Denied => f.write_str("macOS doesn't allow the microphone"),
            MicError::NoDevice => f.write_str("no microphone"),
            MicError::Failed(s) => f.write_str(s),
        }
    }
}

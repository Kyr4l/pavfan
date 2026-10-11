// SPDX-License-Identifier: GPL-3.0-or-later
use std::fmt;
use std::io;

/// Everything that can go wrong. Every variant is meant to be shown to a human.
#[derive(Debug)]
pub enum Error {
    /// An OS-level I/O failure, with what we were doing.
    Io { context: String, source: io::Error },
    /// This is not the machine the register map was verified on.
    UnsupportedMachine(String),
    /// The EC interface (ec_sys / debugfs) is unavailable or read-only.
    EcAccess(String),
    /// The EC reports something we do not expect; we refuse to guess.
    EcState(String),
    /// Something other than us already controls the fans.
    ForeignControl(String),
    /// A temperature sensor failed or returned garbage.
    Sensor(String),
    /// Bad user configuration (curve file, arguments).
    Config(String),
    /// Talking to the daemon (D-Bus) failed.
    Ipc(String),
}

pub type Result<T> = std::result::Result<T, Error>;

impl Error {
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Error::Io { context: context.into(), source }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Io { context, source } => write!(f, "{context}: {source}"),
            Error::UnsupportedMachine(m) => write!(f, "unsupported machine: {m}"),
            Error::EcAccess(m) => write!(f, "EC access: {m}"),
            Error::EcState(m) => write!(f, "unexpected EC state: {m}"),
            Error::ForeignControl(m) => write!(f, "fans already under external control: {m}"),
            Error::Sensor(m) => write!(f, "sensor: {m}"),
            Error::Config(m) => write!(f, "configuration: {m}"),
            Error::Ipc(m) => write!(f, "daemon: {m}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

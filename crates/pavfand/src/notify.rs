// SPDX-License-Identifier: GPL-3.0-or-later
//! Minimal `sd_notify(3)` client (READY/WATCHDOG/STATUS/STOPPING) without
//! linking libsystemd. Silently inactive when not started by systemd.

use std::env;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};

pub struct Notifier {
    target: Option<(UnixDatagram, SocketAddr)>,
}

impl Notifier {
    pub fn from_env() -> Self {
        let Some(path) = env::var_os("NOTIFY_SOCKET") else {
            return Self { target: None };
        };
        let path = path.to_string_lossy().into_owned();
        let addr = match path.strip_prefix('@') {
            Some(abstract_name) => SocketAddr::from_abstract_name(abstract_name.as_bytes()),
            None => SocketAddr::from_pathname(&path),
        };
        let target = match (UnixDatagram::unbound(), addr) {
            (Ok(sock), Ok(addr)) => Some((sock, addr)),
            _ => None,
        };
        Self { target }
    }

    pub fn active(&self) -> bool {
        self.target.is_some()
    }

    /// Best effort: notification failures must never affect fan control.
    pub fn send(&self, message: &str) {
        if let Some((sock, addr)) = &self.target {
            let _ = sock.send_to_addr(message.as_bytes(), addr);
        }
    }
}

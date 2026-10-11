// SPDX-License-Identifier: GPL-3.0-or-later
//! The D-Bus face of the daemon.
//!
//! Method calls never touch the EC. They validate their arguments and queue a
//! `Request`; the control loop (the only EC writer) applies queued requests at
//! the start of its next tick and publishes a fresh `Status` after every tick.

use std::sync::{Arc, Mutex, MutexGuard};

use pavfan_core::controller::Limits;
use pavfan_core::governor::AutoProfile;
use pavfan_ipc::{AUTO_PROFILES, MODES, Status};
use zbus::fdo;
use zbus::object_server::SignalEmitter;

use crate::config::curve_from_points;

#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Mode(String),
    FixedPercent(u8),
    Curve(Vec<(f64, u8)>),
    Profile(AutoProfile),
}

/// State shared between the control loop and the D-Bus methods.
pub struct Shared {
    pub status: Status,
    pub limits: Limits,
    pub mode_name: String,
    pub auto_profile: AutoProfile,
    pub fixed_percent: u8,
    pub manual_points: Vec<(f64, u8)>,
    pub pending: Vec<Request>,
}

impl Shared {
    pub fn new(
        limits: Limits,
        mode_name: String,
        auto_profile: AutoProfile,
        fixed_percent: u8,
        manual_points: Vec<(f64, u8)>,
    ) -> Self {
        let status = Status {
            mode: mode_name.clone(),
            auto_profile: auto_profile.name().into(),
            fixed_percent,
            has_manual_curve: !manual_points.is_empty(),
            min_level: limits.min,
            max_level: limits.max,
            cpu_c: -1.0,
            gpu_c: -1.0,
            effective_c: -1.0,
            health: "starting".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            ..Status::default()
        };
        Self { status, limits, mode_name, auto_profile, fixed_percent, manual_points, pending: Vec::new() }
    }
}

/// Lock that survives a poisoned mutex: the loop thread never panics while
/// holding it, and a stale status is better than a dead D-Bus service.
pub fn lock(shared: &Arc<Mutex<Shared>>) -> MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

pub struct Daemon1 {
    pub shared: Arc<Mutex<Shared>>,
}

#[zbus::interface(name = "org.pavfan.Daemon1")]
impl Daemon1 {
    fn get_status(&self) -> Status {
        lock(&self.shared).status.clone()
    }

    fn set_mode(&self, mode: &str) -> fdo::Result<()> {
        if !MODES.contains(&mode) {
            return Err(fdo::Error::InvalidArgs(format!("unknown mode '{mode}'; expected one of {MODES:?}")));
        }
        let mut s = lock(&self.shared);
        if mode == "manual" && s.manual_points.is_empty() {
            return Err(fdo::Error::Failed("no MANUAL curve configured; call SetManualCurve first".into()));
        }
        s.pending.push(Request::Mode(mode.to_owned()));
        Ok(())
    }

    fn set_fixed_percent(&self, percent: u8) -> fdo::Result<()> {
        if percent > 100 {
            return Err(fdo::Error::InvalidArgs("percent must be 0..=100".into()));
        }
        lock(&self.shared).pending.push(Request::FixedPercent(percent));
        Ok(())
    }

    fn set_auto_profile(&self, profile: &str) -> fdo::Result<()> {
        let p = AutoProfile::parse(profile)
            .ok_or_else(|| fdo::Error::InvalidArgs(format!("unknown profile '{profile}'; expected one of {AUTO_PROFILES:?}")))?;
        lock(&self.shared).pending.push(Request::Profile(p));
        Ok(())
    }

    fn set_manual_curve(&self, points: Vec<(f64, u8)>) -> fdo::Result<()> {
        let mut s = lock(&self.shared);
        curve_from_points(&points, s.limits).map_err(|e| fdo::Error::InvalidArgs(e.to_string()))?;
        // Publish the validated points at once so a SetMode("manual") in the same tick
        // succeeds; the control loop rebuilds the governor and persists the file.
        s.manual_points = points.clone();
        s.status.has_manual_curve = true;
        s.pending.push(Request::Curve(points));
        Ok(())
    }

    fn get_manual_curve(&self) -> Vec<(f64, u8)> {
        lock(&self.shared).manual_points.clone()
    }

    #[zbus(property)]
    fn version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_owned()
    }

    #[zbus(signal)]
    pub async fn mode_changed(emitter: &SignalEmitter<'_>, mode: &str) -> zbus::Result<()>;
}

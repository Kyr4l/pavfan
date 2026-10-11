// SPDX-License-Identifier: GPL-3.0-or-later
//! D-Bus contract between `pavfand` and its clients (`pavfan`, `pavfan-tray`).
//!
//! System bus, well-known name `org.pavfan.Daemon1`, object `/org/pavfan/Daemon1`.
//! Reading status is open to everyone; changing the mode is restricted by the
//! bus policy file shipped in `packaging/dbus/`.

use serde::{Deserialize, Serialize};
use zbus::zvariant::Type;

pub const BUS_NAME: &str = "org.pavfan.Daemon1";
pub const OBJECT_PATH: &str = "/org/pavfan/Daemon1";
pub const INTERFACE: &str = "org.pavfan.Daemon1";
/// Mode names accepted by `SetMode`.
pub const MODES: [&str; 4] = ["off", "auto", "fixed", "manual"];

/// Snapshot of the daemon's state, refreshed every control tick.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, Type)]
pub struct Status {
    /// "off" | "auto" | "fixed" | "manual"
    pub mode: String,
    /// Target used by FIXED mode, in percent of the EC range.
    pub fixed_percent: u8,
    /// Whether a MANUAL curve is configured (MANUAL can only be selected if so).
    pub has_manual_curve: bool,
    /// Fan level the EC is running (units of 100 RPM): ours when `engaged`, the BIOS's otherwise.
    pub level: u8,
    /// `level` as a percentage of the EC range.
    pub percent: u8,
    pub min_level: u8,
    pub max_level: u8,
    /// True while pavfan drives the fans (override active); false = BIOS control.
    pub engaged: bool,
    /// True while the hard thermal override forces maximum fans.
    pub hard_override: bool,
    /// Degrees Celsius; negative when unavailable.
    pub cpu_c: f64,
    /// Degrees Celsius; negative when unavailable or the GPU is asleep.
    pub gpu_c: f64,
    pub gpu_present: bool,
    /// The temperature decisions are based on; negative when unavailable.
    pub effective_c: f64,
    /// "ok" | "sensor-failure" | "recovering"
    pub health: String,
    /// Human-readable detail for `health` or the latest warning; may be empty.
    pub detail: String,
    pub version: String,
}

#[zbus::proxy(
    interface = "org.pavfan.Daemon1",
    default_service = "org.pavfan.Daemon1",
    default_path = "/org/pavfan/Daemon1"
)]
pub trait Daemon1 {
    fn get_status(&self) -> zbus::Result<Status>;
    /// "off" | "auto" | "fixed" | "manual"
    fn set_mode(&self, mode: &str) -> zbus::Result<()>;
    /// 0..=100; takes effect in FIXED mode.
    fn set_fixed_percent(&self, percent: u8) -> zbus::Result<()>;
    /// Points as (temperature C, percent of the EC range); validated by the daemon.
    fn set_manual_curve(&self, points: Vec<(f64, u8)>) -> zbus::Result<()>;
    fn get_manual_curve(&self) -> zbus::Result<Vec<(f64, u8)>>;

    #[zbus(signal)]
    fn mode_changed(&self, mode: &str) -> zbus::Result<()>;

    #[zbus(property)]
    fn version(&self) -> zbus::Result<String>;
}

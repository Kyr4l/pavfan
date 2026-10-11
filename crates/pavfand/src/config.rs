// SPDX-License-Identifier: GPL-3.0-or-later
//! Configuration (`/etc/pavfan/config.toml`, admin-owned defaults) and runtime
//! state (`/var/lib/pavfan/`, the last mode chosen through the tray or CLI).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use pavfan_core::controller::Limits;
use pavfan_core::curve::{Curve, Point, parse_curve};
use pavfan_core::governor::Params;
use pavfan_core::{Error, Result};
use pavfan_ipc::MODES;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Mode used when no saved state exists: off | auto | fixed | manual.
    pub default_mode: String,
    pub fixed_percent: u8,
    pub interval_ms: u64,
    /// MANUAL curve file (same format as `pavfan curve --file`).
    pub manual_curve: PathBuf,
    pub thresholds: Thresholds,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_mode: "auto".into(),
            fixed_percent: 50,
            interval_ms: 1000,
            manual_curve: PathBuf::from("/etc/pavfan/curve.toml"),
            thresholds: Thresholds::default(),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Thresholds {
    pub engage_c: f32,
    pub disengage_c: f32,
    pub gpu_bias_c: f32,
    pub hard_c: f32,
}

impl Default for Thresholds {
    fn default() -> Self {
        let p = Params::default();
        Self { engage_c: p.engage_c, disengage_c: p.disengage_c, gpu_bias_c: p.gpu_bias_c, hard_c: p.hard_c }
    }
}

impl Config {
    /// A missing file means defaults; a malformed one is an error (we refuse to guess).
    pub fn load(path: &Path) -> Result<Self> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|e| Error::Config(format!("{}: {e}", path.display()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::io(format!("reading {}", path.display()), e)),
        }
    }

    pub fn parse(text: &str) -> std::result::Result<Self, toml::de::Error> {
        toml::from_str(text)
    }

    pub fn params(&self) -> Result<Params> {
        let p = Params {
            engage_c: self.thresholds.engage_c,
            disengage_c: self.thresholds.disengage_c,
            gpu_bias_c: self.thresholds.gpu_bias_c,
            hard_c: self.thresholds.hard_c,
            ..Params::default()
        };
        p.validate()?;
        Ok(p)
    }
}

/// What the user last asked for; survives restarts and reboots.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct State {
    pub mode: String,
    pub fixed_percent: u8,
}

const STATE_FILE: &str = "state.toml";
const CURVE_FILE: &str = "curve.toml";

impl State {
    /// Saved state if valid, otherwise the configured defaults.
    pub fn load(dir: &Path, cfg: &Config) -> Self {
        let fallback = Self { mode: valid_mode(&cfg.default_mode), fixed_percent: cfg.fixed_percent.min(100) };
        match fs::read_to_string(dir.join(STATE_FILE)) {
            Ok(text) => match toml::from_str::<State>(&text) {
                Ok(s) => Self { mode: valid_mode(&s.mode), fixed_percent: s.fixed_percent.min(100) },
                Err(e) => {
                    eprintln!("pavfand: ignoring unreadable state file: {e}");
                    fallback
                }
            },
            Err(_) => fallback,
        }
    }

    pub fn save(&self, dir: &Path) {
        match toml::to_string(self) {
            Ok(text) => {
                if let Err(e) = write_atomic(&dir.join(STATE_FILE), &text) {
                    eprintln!("pavfand: could not save state to {}: {e}", dir.display());
                }
            }
            Err(e) => eprintln!("pavfand: could not serialise state: {e}"),
        }
    }
}

fn valid_mode(name: &str) -> String {
    if MODES.contains(&name) { name.to_owned() } else { "auto".to_owned() }
}

/// The MANUAL curve: the user's saved copy in the state dir wins over the admin's
/// file from the config. Returned as (temperature, percent) points; empty if none.
pub fn load_curve_points(state_dir: &Path, cfg: &Config, limits: Limits) -> Vec<(f64, u8)> {
    for path in [state_dir.join(CURVE_FILE), cfg.manual_curve.clone()] {
        let Ok(text) = fs::read_to_string(&path) else { continue };
        match parse_curve(&text, limits) {
            Ok(curve) => return curve_to_points(&curve, limits),
            Err(e) => eprintln!("pavfand: ignoring curve file {}: {e}", path.display()),
        }
    }
    Vec::new()
}

pub fn save_curve_points(state_dir: &Path, points: &[(f64, u8)]) {
    let mut text = String::from("# MANUAL fan curve saved by pavfand (temperature C -> percent of the EC range)\n");
    for (t, p) in points {
        text.push_str(&format!("\n[[point]]\ntemp = {t}\npercent = {p}\n"));
    }
    if let Err(e) = write_atomic(&state_dir.join(CURVE_FILE), &text) {
        eprintln!("pavfand: could not save curve to {}: {e}", state_dir.display());
    }
}

pub fn curve_to_points(curve: &Curve, limits: Limits) -> Vec<(f64, u8)> {
    curve.points().iter().map(|p| (f64::from(p.temp_c), limits.to_percent(p.level))).collect()
}

/// Build (and thereby validate) a curve from (temperature, percent) points.
pub fn curve_from_points(points: &[(f64, u8)], limits: Limits) -> Result<Curve> {
    if points.is_empty() {
        return Err(Error::Config("no MANUAL curve configured; set one first".into()));
    }
    let pts = points
        .iter()
        .map(|&(t, p)| Point { temp_c: t as f32, level: limits.from_percent(p) })
        .collect();
    Curve::new(pts, limits)
}

fn write_atomic(path: &Path, content: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, content)?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: Limits = Limits { min: 22, max: 42 };

    #[test]
    fn empty_config_is_all_defaults() {
        let c = Config::parse("").unwrap();
        assert_eq!(c.default_mode, "auto");
        assert_eq!(c.interval_ms, 1000);
        assert!((c.thresholds.hard_c - 88.0).abs() < 0.01);
        assert!(c.params().is_ok());
    }

    #[test]
    fn partial_thresholds_keep_other_defaults_and_are_validated() {
        let c = Config::parse("[thresholds]\nengage_c = 70\n").unwrap();
        assert!((c.thresholds.engage_c - 70.0).abs() < 0.01);
        assert!((c.thresholds.disengage_c - 62.0).abs() < 0.01);
        let bad = Config::parse("[thresholds]\nhard_c = 99\n").unwrap();
        assert!(bad.params().is_err());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::parse("defualt_mode = \"auto\"\n").is_err());
        assert!(Config::parse("[thresholds]\nbogus = 1\n").is_err());
    }

    #[test]
    fn state_falls_back_to_config_defaults() {
        let dir = std::env::temp_dir().join(format!("pavfand-test-{}", std::process::id()));
        let cfg = Config::parse("default_mode = \"fixed\"\nfixed_percent = 70\n").unwrap();
        let s = State::load(&dir, &cfg);
        assert_eq!(s, State { mode: "fixed".into(), fixed_percent: 70 });
        State { mode: "manual".into(), fixed_percent: 30 }.save(&dir);
        assert_eq!(State::load(&dir, &cfg), State { mode: "manual".into(), fixed_percent: 30 });
        fs::write(dir.join(STATE_FILE), "mode = \"bogus\"\nfixed_percent = 200\n").unwrap();
        assert_eq!(State::load(&dir, &cfg), State { mode: "auto".into(), fixed_percent: 100 });
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn curve_points_round_trip_and_validate() {
        let pts = vec![(60.0, 25u8), (75.0, 60), (85.0, 100)];
        let c = curve_from_points(&pts, L).unwrap();
        assert_eq!(curve_to_points(&c, L), pts);
        assert!(curve_from_points(&[], L).is_err());
        assert!(curve_from_points(&[(70.0, 50), (60.0, 60)], L).is_err());
    }
}

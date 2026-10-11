// SPDX-License-Identifier: GPL-3.0-or-later
//! Configuration (`/etc/pavfan/config.toml`, admin-owned defaults) and runtime
//! state (`/var/lib/pavfan/`, the last mode chosen through the tray or CLI).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use pavfan_core::controller::Limits;
use pavfan_core::curve::{Curve, Point, parse_curve};
use pavfan_core::governor::{AutoProfile, Params, secs_to_ticks};
use pavfan_core::{Error, Result};
use pavfan_ipc::MODES;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    /// Mode used when no saved state exists: off | auto | fixed | manual.
    pub default_mode: String,
    /// AUTO profile used when no saved state exists: quiet | early.
    pub auto_profile: String,
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
            auto_profile: "quiet".into(),
            fixed_percent: 50,
            interval_ms: 1000,
            manual_curve: PathBuf::from("/etc/pavfan/curve.toml"),
            thresholds: Thresholds::default(),
        }
    }
}

/// Every key is optional and overrides the selected AUTO profile's preset when set.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Thresholds {
    pub engage_c: Option<f32>,
    pub disengage_c: Option<f32>,
    pub gpu_bias_c: Option<f32>,
    pub hard_c: Option<f32>,
    /// AUTO takes over only after this long at/above `engage_c` (debounces load spikes).
    pub engage_delay_s: Option<f32>,
    /// AUTO hands back only after this long at/below `disengage_c`.
    pub release_delay_s: Option<f32>,
    /// Once engaged, AUTO stays engaged at least this long.
    pub min_engaged_s: Option<f32>,
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

    /// The configured default AUTO profile; an unknown name is an error (we refuse to guess).
    pub fn default_profile(&self) -> Result<AutoProfile> {
        AutoProfile::parse(&self.auto_profile)
            .ok_or_else(|| Error::Config(format!("auto_profile '{}' must be one of quiet, early", self.auto_profile)))
    }

    /// Governor parameters for `profile`: its presets (ticks rescaled to `interval`),
    /// overridden by whatever `[thresholds]` keys are set.
    pub fn params(&self, interval: Duration, profile: AutoProfile) -> Result<Params> {
        let t = &self.thresholds;
        if [t.engage_delay_s, t.release_delay_s, t.min_engaged_s].into_iter().flatten().any(|d| d < 0.0) {
            return Err(Error::Config("delays must not be negative".into()));
        }
        let base = profile.params().rescale_ticks(interval);
        let p = Params {
            engage_c: t.engage_c.unwrap_or(base.engage_c),
            disengage_c: t.disengage_c.unwrap_or(base.disengage_c),
            gpu_bias_c: t.gpu_bias_c.unwrap_or(base.gpu_bias_c),
            hard_c: t.hard_c.unwrap_or(base.hard_c),
            engage_ticks: t.engage_delay_s.map_or(base.engage_ticks, |s| secs_to_ticks(s, interval)),
            release_ticks: t.release_delay_s.map_or(base.release_ticks, |s| secs_to_ticks(s, interval)),
            min_engaged_ticks: t.min_engaged_s.map_or(base.min_engaged_ticks, |s| secs_to_ticks(s, interval)),
            ..base
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
    /// Missing in state files written by 0.1.x.
    #[serde(default = "quiet")]
    pub auto_profile: String,
}

fn quiet() -> String {
    "quiet".into()
}

const STATE_FILE: &str = "state.toml";
const CURVE_FILE: &str = "curve.toml";

impl State {
    /// Saved state if valid, otherwise the configured defaults.
    pub fn load(dir: &Path, cfg: &Config) -> Self {
        let default_profile = cfg.default_profile().map_or("quiet", AutoProfile::name).to_owned();
        let fallback = Self {
            mode: valid_mode(&cfg.default_mode),
            fixed_percent: cfg.fixed_percent.min(100),
            auto_profile: default_profile.clone(),
        };
        match fs::read_to_string(dir.join(STATE_FILE)) {
            Ok(text) => match toml::from_str::<State>(&text) {
                Ok(s) => Self {
                    mode: valid_mode(&s.mode),
                    fixed_percent: s.fixed_percent.min(100),
                    auto_profile: if AutoProfile::parse(&s.auto_profile).is_some() { s.auto_profile } else { default_profile },
                },
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
        assert_eq!(c.thresholds.hard_c, None);
        assert_eq!(c.default_profile().unwrap(), AutoProfile::Quiet);
        let p = c.params(Duration::from_millis(500), AutoProfile::Quiet).unwrap();
        assert_eq!((p.engage_ticks, p.release_ticks, p.min_engaged_ticks), (10, 20, 60));
        assert!((p.hard_c - 88.0).abs() < 0.01);
    }

    #[test]
    fn partial_thresholds_keep_other_defaults_and_are_validated() {
        let c = Config::parse("[thresholds]\nengage_c = 70\n").unwrap();
        let p = c.params(Duration::from_secs(1), AutoProfile::Quiet).unwrap();
        assert!((p.engage_c - 70.0).abs() < 0.01);
        assert!((p.disengage_c - 62.0).abs() < 0.01); // untouched preset
        // the same override applies on top of the other profile's presets
        let p = c.params(Duration::from_secs(1), AutoProfile::Early).unwrap();
        assert!((p.engage_c - 70.0).abs() < 0.01);
        assert!((p.disengage_c - 52.0).abs() < 0.01);
        assert_eq!(p.auto_profile, AutoProfile::Early);
        let bad = Config::parse("[thresholds]\nhard_c = 99\n").unwrap();
        assert!(bad.params(Duration::from_secs(1), AutoProfile::Quiet).is_err());
        let bad = Config::parse("[thresholds]\nengage_delay_s = -1\n").unwrap();
        assert!(bad.params(Duration::from_secs(1), AutoProfile::Quiet).is_err());
        let early = Config::parse("auto_profile = \"early\"\n").unwrap();
        assert_eq!(early.default_profile().unwrap(), AutoProfile::Early);
        assert!(Config::parse("auto_profile = \"loud\"\n").unwrap().default_profile().is_err());
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
        assert_eq!(s, State { mode: "fixed".into(), fixed_percent: 70, auto_profile: "quiet".into() });
        State { mode: "manual".into(), fixed_percent: 30, auto_profile: "early".into() }.save(&dir);
        assert_eq!(State::load(&dir, &cfg), State { mode: "manual".into(), fixed_percent: 30, auto_profile: "early".into() });
        // a 0.1.x state file has no auto_profile; a bogus one falls back to the config default
        fs::write(dir.join(STATE_FILE), "mode = \"bogus\"\nfixed_percent = 200\n").unwrap();
        assert_eq!(State::load(&dir, &cfg), State { mode: "auto".into(), fixed_percent: 100, auto_profile: "quiet".into() });
        fs::write(dir.join(STATE_FILE), "mode = \"auto\"\nfixed_percent = 50\nauto_profile = \"loud\"\n").unwrap();
        assert_eq!(State::load(&dir, &cfg).auto_profile, "quiet");
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

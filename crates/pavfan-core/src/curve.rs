// SPDX-License-Identifier: GPL-3.0-or-later
//! Temperature -> fan level curves, and their TOML representation.

use serde::Deserialize;

use crate::controller::Limits;
use crate::error::{Error, Result};
use crate::governor::AutoProfile;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub temp_c: f32,
    pub level: u8,
}

/// A validated curve: >= 2 points, strictly increasing temperatures, and levels
/// that never decrease as the temperature rises (a hotter machine must never get
/// less cooling), all inside the EC's own limits.
#[derive(Clone, Debug, PartialEq)]
pub struct Curve {
    points: Vec<Point>,
}

impl Curve {
    pub fn new(points: Vec<Point>, limits: Limits) -> Result<Self> {
        if points.len() < 2 {
            return Err(Error::Config("a curve needs at least 2 points".into()));
        }
        for p in &points {
            if !p.temp_c.is_finite() || !(20.0..=110.0).contains(&p.temp_c) {
                return Err(Error::Config(format!("temperature {} C is outside 20..110", p.temp_c)));
            }
            if p.level < limits.min || p.level > limits.max {
                return Err(Error::Config(format!(
                    "level {} is outside the EC range {}..{}",
                    p.level, limits.min, limits.max
                )));
            }
        }
        for w in points.windows(2) {
            if let [a, b] = w {
                if b.temp_c <= a.temp_c {
                    return Err(Error::Config(format!(
                        "temperatures must be strictly increasing ({} then {})",
                        a.temp_c, b.temp_c
                    )));
                }
                if b.level < a.level {
                    return Err(Error::Config(format!(
                        "level must not decrease as temperature rises ({} at {} C, then {} at {} C)",
                        a.level, a.temp_c, b.level, b.temp_c
                    )));
                }
            }
        }
        Ok(Self { points })
    }

    pub fn points(&self) -> &[Point] {
        &self.points
    }

    /// Piecewise-linear interpolation, clamped to the first/last point.
    pub fn level_at(&self, temp_c: f32) -> u8 {
        let (Some(first), Some(last)) = (self.points.first(), self.points.last()) else {
            return 0;
        };
        if temp_c <= first.temp_c {
            return first.level;
        }
        if temp_c >= last.temp_c {
            return last.level;
        }
        for w in self.points.windows(2) {
            if let [a, b] = w
                && temp_c <= b.temp_c
            {
                let t = (temp_c - a.temp_c) / (b.temp_c - a.temp_c);
                let l = f32::from(a.level) + t * (f32::from(b.level) - f32::from(a.level));
                return l.round().clamp(0.0, 255.0) as u8;
            }
        }
        last.level
    }

    /// Lowest temperature at which the curve starts.
    pub fn start_temp(&self) -> f32 {
        self.points.first().map_or(0.0, |p| p.temp_c)
    }
}

/// The built-in AUTO curves (levels are x100 RPM) over the *effective* temperature
/// (`max(CPU, GPU + bias)`, see `governor`).
///
/// `Quiet` is shaped by the sweeps in docs/thermal-measurements.md: the override never
/// cooled the CPU worse than the BIOS at any level, its gain is largest under GPU heat,
/// and under combined load the machine needs maximum fans from about 85 C.
/// `Early` starts 10 C lower, at the EC minimum, with smaller steps: the take-over is a
/// small change instead of a jump from silence, at the price of a hum more of the time.
pub fn auto_curve(profile: AutoProfile, limits: Limits) -> Result<Curve> {
    let pts: &[(f32, u8)] = match profile {
        AutoProfile::Quiet => &[(68.0, 30), (76.0, 34), (82.0, 39), (85.0, 42)],
        AutoProfile::Early => &[(58.0, 22), (64.0, 26), (70.0, 30), (76.0, 34), (82.0, 39), (85.0, 42)],
    };
    let pts = pts.iter().map(|&(temp_c, l)| Point { temp_c, level: limits.clamp(l) }).collect();
    Curve::new(pts, limits)
}

/// The default (`Quiet`) AUTO curve.
pub fn default_auto(limits: Limits) -> Result<Curve> {
    auto_curve(AutoProfile::Quiet, limits)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurveFile {
    point: Vec<PointCfg>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PointCfg {
    temp: f32,
    level: Option<u8>,
    percent: Option<u8>,
}

/// Parse a curve file:
///
/// ```toml
/// [[point]]
/// temp = 60
/// level = 32        # x100 RPM, or `percent = 50`
/// ```
pub fn parse_curve(text: &str, limits: Limits) -> Result<Curve> {
    let file: CurveFile = toml::from_str(text).map_err(|e| Error::Config(e.to_string()))?;
    let mut pts = Vec::with_capacity(file.point.len());
    for p in file.point {
        let level = match (p.level, p.percent) {
            (Some(l), None) => l,
            (None, Some(pc)) => limits.from_percent(pc),
            _ => return Err(Error::Config("each point needs exactly one of `level` or `percent`".into())),
        };
        pts.push(Point { temp_c: p.temp, level });
    }
    Curve::new(pts, limits)
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: Limits = Limits { min: 22, max: 42 };

    fn pts(v: &[(f32, u8)]) -> Vec<Point> {
        v.iter().map(|&(temp_c, level)| Point { temp_c, level }).collect()
    }

    #[test]
    fn interpolates_and_clamps() {
        let c = Curve::new(pts(&[(60.0, 30), (80.0, 40)]), L).ok();
        let c = c.as_ref();
        assert_eq!(c.map(|c| c.level_at(50.0)), Some(30));
        assert_eq!(c.map(|c| c.level_at(70.0)), Some(35));
        assert_eq!(c.map(|c| c.level_at(95.0)), Some(40));
    }

    #[test]
    fn rejects_decreasing_levels() {
        assert!(Curve::new(pts(&[(60.0, 35), (70.0, 30)]), L).is_err());
    }

    #[test]
    fn rejects_non_increasing_temps() {
        assert!(Curve::new(pts(&[(60.0, 30), (60.0, 31)]), L).is_err());
    }

    #[test]
    fn rejects_out_of_range_levels_and_temps() {
        assert!(Curve::new(pts(&[(60.0, 10), (70.0, 30)]), L).is_err());
        assert!(Curve::new(pts(&[(60.0, 30), (70.0, 99)]), L).is_err());
        assert!(Curve::new(pts(&[(5.0, 30), (70.0, 31)]), L).is_err());
        assert!(Curve::new(pts(&[(f32::NAN, 30), (70.0, 31)]), L).is_err());
    }

    #[test]
    fn rejects_single_point() {
        assert!(Curve::new(pts(&[(60.0, 30)]), L).is_err());
    }

    #[test]
    fn default_auto_is_valid() {
        assert!(default_auto(L).is_ok());
    }

    #[test]
    fn every_profile_curve_is_valid_and_starts_at_its_engage_temperature() {
        for p in AutoProfile::ALL {
            let c = auto_curve(p, L).unwrap();
            assert!((c.start_temp() - p.params().engage_c).abs() < 0.01, "{p:?}");
            assert_eq!(c.level_at(100.0), 42);
        }
    }

    #[test]
    fn parses_toml_with_level_and_percent() {
        let t = "[[point]]\ntemp = 60\nlevel = 30\n[[point]]\ntemp = 80\npercent = 100\n";
        let c = parse_curve(t, L).ok();
        assert_eq!(c.map(|c| c.level_at(80.0)), Some(42));
    }

    #[test]
    fn toml_rejects_unknown_fields_and_ambiguous_points() {
        assert!(parse_curve("[[point]]\ntemp=60\nlevel=30\nbogus=1\n[[point]]\ntemp=70\nlevel=31", L).is_err());
        assert!(parse_curve("[[point]]\ntemp=60\nlevel=30\npercent=5\n[[point]]\ntemp=70\nlevel=31", L).is_err());
        assert!(parse_curve("[[point]]\ntemp=60\n[[point]]\ntemp=70\nlevel=31", L).is_err());
    }
}

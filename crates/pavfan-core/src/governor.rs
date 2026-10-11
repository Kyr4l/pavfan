// SPDX-License-Identifier: GPL-3.0-or-later
//! Decides, tick by tick, what the fans should do. Pure logic, no I/O.

use crate::controller::Limits;
use crate::curve::{Curve, default_auto};
use crate::error::{Error, Result};

#[derive(Clone, Debug)]
pub enum Mode {
    /// Tool disabled: the BIOS controls the fans.
    Off,
    /// Built-in curve, BIOS control below the engage threshold.
    Auto,
    /// Constant level.
    Fixed(u8),
    /// User curve.
    Manual(Curve),
}

impl Mode {
    pub fn name(&self) -> &'static str {
        match self {
            Mode::Off => "off",
            Mode::Auto => "auto",
            Mode::Fixed(_) => "fixed",
            Mode::Manual(_) => "manual",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Inputs {
    pub cpu_c: f32,
    /// None = the GPU is asleep / absent (treated as cool).
    pub gpu_c: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Let the BIOS drive the fans.
    Bios,
    Level(u8),
}

#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// Added to the GPU temperature before it is compared with the CPU's. Positive by
    /// default: the BIOS ignores GPU heat (measured, see docs/thermal-measurements.md),
    /// and the GPU fan is the fan the override actually drives, so GPU heat is what we
    /// must react to.
    pub gpu_bias_c: f32,
    /// AUTO: hand control to us at/above this effective temperature...
    pub engage_c: f32,
    /// ...and back to the BIOS at/below this one (hysteresis).
    pub disengage_c: f32,
    /// At/above this effective temperature the fans go to maximum, in every mode but OFF.
    pub hard_c: f32,
    /// Hard override releases only once this many degrees below `hard_c`.
    pub hard_hysteresis_c: f32,
    /// Fan level may drop by one step only every N ticks (anti-hunting, less noise).
    pub down_every_ticks: u32,
}

/// Highest value `hard_c` may be configured to.
pub const HARD_C_CEILING: f32 = 90.0;

impl Default for Params {
    fn default() -> Self {
        Self {
            gpu_bias_c: 8.0,
            engage_c: 68.0,
            disengage_c: 62.0,
            hard_c: 88.0,
            hard_hysteresis_c: 5.0,
            down_every_ticks: 3,
        }
    }
}

impl Params {
    pub fn validate(&self) -> Result<()> {
        if self.hard_c > HARD_C_CEILING {
            return Err(Error::Config(format!("hard threshold must be <= {HARD_C_CEILING} C")));
        }
        if self.disengage_c >= self.engage_c {
            return Err(Error::Config("disengage must be below engage".into()));
        }
        if self.engage_c >= self.hard_c {
            return Err(Error::Config("engage must be below the hard threshold".into()));
        }
        if self.hard_hysteresis_c < 0.0 || self.down_every_ticks == 0 {
            return Err(Error::Config("negative hysteresis or zero tick count".into()));
        }
        if !(-20.0..=20.0).contains(&self.gpu_bias_c) {
            return Err(Error::Config("GPU bias must be within -20..20 C".into()));
        }
        Ok(())
    }
}

pub struct Governor {
    mode: Mode,
    params: Params,
    limits: Limits,
    auto_curve: Curve,
    engaged: bool,
    hard_active: bool,
    current: Option<u8>,
    down_ticks: u32,
}

impl Governor {
    pub fn new(mode: Mode, params: Params, limits: Limits) -> Result<Self> {
        params.validate()?;
        let auto_curve = default_auto(limits)?;
        let mode = match mode {
            Mode::Fixed(l) => Mode::Fixed(limits.clamp(l)),
            other => other,
        };
        Ok(Self {
            mode,
            params,
            limits,
            auto_curve,
            engaged: false,
            hard_active: false,
            current: None,
            down_ticks: 0,
        })
    }

    pub fn mode(&self) -> &Mode {
        &self.mode
    }

    /// True while the hard thermal override is forcing maximum fans.
    pub fn hard_active(&self) -> bool {
        self.hard_active
    }

    /// Temperature the decisions are based on: the hotter of the CPU and the
    /// (biased) GPU, because the shared cooler moves heat both ways.
    pub fn effective_c(&self, i: Inputs) -> f32 {
        match i.gpu_c {
            Some(g) => i.cpu_c.max(g + self.params.gpu_bias_c),
            None => i.cpu_c,
        }
    }

    /// Forget all state (used after a sensor failure: we fall back to the BIOS).
    pub fn reset(&mut self) {
        self.engaged = false;
        self.hard_active = false;
        self.current = None;
        self.down_ticks = 0;
    }

    pub fn decide(&mut self, i: Inputs) -> Decision {
        if matches!(self.mode, Mode::Off) {
            self.reset();
            return Decision::Bios;
        }
        let eff = self.effective_c(i);

        // Hard thermal override, with hysteresis. Applies to FIXED and MANUAL too:
        // a user's low setting must never be able to cook the machine.
        if self.hard_active {
            self.hard_active = eff >= self.params.hard_c - self.params.hard_hysteresis_c;
        } else {
            self.hard_active = eff >= self.params.hard_c;
        }
        if self.hard_active {
            self.current = Some(self.limits.max);
            self.down_ticks = 0;
            return Decision::Level(self.limits.max);
        }

        let desired = match &self.mode {
            Mode::Off => return Decision::Bios,
            Mode::Fixed(l) => {
                self.current = Some(*l);
                return Decision::Level(*l);
            }
            Mode::Manual(curve) => curve.level_at(eff),
            Mode::Auto => {
                if self.engaged {
                    if eff <= self.params.disengage_c {
                        self.engaged = false;
                    }
                } else if eff >= self.params.engage_c {
                    self.engaged = true;
                }
                if !self.engaged {
                    self.current = None;
                    self.down_ticks = 0;
                    return Decision::Bios;
                }
                self.auto_curve.level_at(eff)
            }
        };

        // Rise immediately, fall slowly.
        let next = match self.current {
            Some(cur) if desired < cur => {
                self.down_ticks += 1;
                if self.down_ticks >= self.params.down_every_ticks {
                    self.down_ticks = 0;
                    cur - 1
                } else {
                    cur
                }
            }
            _ => {
                self.down_ticks = 0;
                desired
            }
        };
        let next = self.limits.clamp(next);
        self.current = Some(next);
        Decision::Level(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curve::Point;

    const L: Limits = Limits { min: 22, max: 42 };

    fn gov(mode: Mode) -> Governor {
        Governor::new(mode, Params::default(), L).expect("valid governor")
    }

    fn cpu(t: f32) -> Inputs {
        Inputs { cpu_c: t, gpu_c: None }
    }

    #[test]
    fn off_always_defers_to_bios_even_when_hot() {
        let mut g = gov(Mode::Off);
        assert_eq!(g.decide(cpu(50.0)), Decision::Bios);
        assert_eq!(g.decide(cpu(99.0)), Decision::Bios);
    }

    #[test]
    fn auto_stays_with_bios_when_cool() {
        let mut g = gov(Mode::Auto);
        assert_eq!(g.decide(cpu(45.0)), Decision::Bios);
        assert_eq!(g.decide(cpu(67.9)), Decision::Bios);
    }

    #[test]
    fn auto_engages_with_hysteresis() {
        let mut g = gov(Mode::Auto);
        assert!(matches!(g.decide(cpu(69.0)), Decision::Level(_)));
        // between disengage (62) and engage (68): stays engaged
        assert!(matches!(g.decide(cpu(65.0)), Decision::Level(_)));
        assert_eq!(g.decide(cpu(61.0)), Decision::Bios);
        // and does not re-engage until engage threshold again
        assert_eq!(g.decide(cpu(65.0)), Decision::Bios);
    }

    #[test]
    fn auto_level_never_below_bios_observed_levels() {
        let mut g = gov(Mode::Auto);
        let Decision::Level(l) = g.decide(cpu(69.0)) else { panic!("expected a level") };
        assert!(l >= 30);
    }

    #[test]
    fn gpu_heat_raises_the_effective_temperature() {
        let g = gov(Mode::Auto);
        let eff = g.effective_c(Inputs { cpu_c: 50.0, gpu_c: Some(60.0) });
        assert!((eff - 68.0).abs() < 0.01);
        let eff = g.effective_c(Inputs { cpu_c: 80.0, gpu_c: Some(40.0) });
        assert!((eff - 80.0).abs() < 0.01);
    }

    #[test]
    fn hot_gpu_alone_engages_auto() {
        let mut g = gov(Mode::Auto);
        assert!(matches!(g.decide(Inputs { cpu_c: 45.0, gpu_c: Some(62.0) }), Decision::Level(_)));
    }

    #[test]
    fn level_rises_immediately_and_falls_one_step_per_n_ticks() {
        let mut g = gov(Mode::Auto);
        assert_eq!(g.decide(cpu(85.0)), Decision::Level(42));
        let mut seen = Vec::new();
        for _ in 0..6 {
            if let Decision::Level(l) = g.decide(cpu(70.0)) {
                seen.push(l);
            }
        }
        assert_eq!(seen, vec![42, 42, 41, 41, 41, 40]);
    }

    #[test]
    fn hard_override_forces_max_in_fixed_mode() {
        let mut g = gov(Mode::Fixed(22));
        assert_eq!(g.decide(cpu(60.0)), Decision::Level(22));
        assert_eq!(g.decide(cpu(89.0)), Decision::Level(42));
        // hysteresis: still max at 85, released below 83
        assert_eq!(g.decide(cpu(85.0)), Decision::Level(42));
        assert_eq!(g.decide(cpu(82.0)), Decision::Level(22));
    }

    #[test]
    fn hard_override_forces_max_in_manual_mode_and_via_gpu() {
        let curve = Curve::new(
            vec![Point { temp_c: 40.0, level: 22 }, Point { temp_c: 100.0, level: 24 }],
            L,
        )
        .expect("curve");
        let mut g = gov(Mode::Manual(curve));
        assert_eq!(g.decide(Inputs { cpu_c: 60.0, gpu_c: Some(95.0) }), Decision::Level(42));
    }

    #[test]
    fn hard_override_does_not_apply_in_off_mode() {
        let mut g = gov(Mode::Off);
        assert_eq!(g.decide(cpu(95.0)), Decision::Bios);
    }

    #[test]
    fn fixed_level_is_clamped_to_ec_limits() {
        let mut g = gov(Mode::Fixed(250));
        assert_eq!(g.decide(cpu(40.0)), Decision::Level(42));
        let mut g = gov(Mode::Fixed(1));
        assert_eq!(g.decide(cpu(40.0)), Decision::Level(22));
    }

    #[test]
    fn rejects_unsafe_params() {
        let p = Params { hard_c: 99.0, ..Params::default() };
        assert!(Governor::new(Mode::Auto, p, L).is_err());
        let p = Params { disengage_c: 70.0, ..Params::default() };
        assert!(Governor::new(Mode::Auto, p, L).is_err());
        let p = Params { engage_c: 95.0, ..Params::default() };
        assert!(Governor::new(Mode::Auto, p, L).is_err());
        let p = Params { gpu_bias_c: 30.0, ..Params::default() };
        assert!(Governor::new(Mode::Auto, p, L).is_err());
    }

    #[test]
    fn reset_returns_to_bios() {
        let mut g = gov(Mode::Auto);
        assert!(matches!(g.decide(cpu(70.0)), Decision::Level(_)));
        g.reset();
        assert_eq!(g.decide(cpu(55.0)), Decision::Bios);
    }
}

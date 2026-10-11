// SPDX-License-Identifier: GPL-3.0-or-later
//! The control-loop body shared by the CLI and the daemon.
//!
//! One `tick` reads the sensors, lets the governor decide and applies the
//! decision through the controller. All sensor-failure policy lives here, so
//! both front-ends behave identically: a failed sensor means the BIOS gets the
//! fans back, and control is only retaken after a few consecutive good readings.

use crate::controller::{FanController, Limits};
use crate::ec::EcIo;
use crate::error::Result;
use crate::governor::{Decision, Governor, Inputs, Mode, Params};
use crate::sensors::{CpuTemp, GpuTemp};

/// Consecutive good readings required before taking control again after a sensor failure.
pub const RECOVERY_TICKS: u32 = 3;
/// GPU read failures tolerated (reusing the last value) before falling back to the BIOS.
pub const GPU_FAIL_TOLERANCE: u32 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Health {
    Ok,
    /// A sensor failed; the fans were handed to the BIOS.
    SensorFailure(String),
    /// Sensors are back; waiting for `RECOVERY_TICKS` good readings.
    Recovering(u32),
}

#[derive(Clone, Debug)]
pub struct TickReport {
    pub cpu_c: Option<f32>,
    pub gpu_c: Option<f32>,
    pub effective_c: Option<f32>,
    pub decision: Decision,
    /// Level the EC is running: ours when engaged, the BIOS's otherwise.
    pub level: u8,
    pub engaged: bool,
    pub hard_override: bool,
    /// The EC had forgotten our override (suspend/resume) and it was re-applied.
    pub reasserted: bool,
    pub health: Health,
    /// Non-fatal remark, e.g. a tolerated GPU read failure.
    pub note: Option<String>,
}

pub struct Runtime<E: EcIo> {
    controller: FanController<E>,
    governor: Governor,
    params: Params,
    cpu: CpuTemp,
    gpu: Option<GpuTemp>,
    last_gpu: Option<f32>,
    gpu_failures: u32,
    good_streak: u32,
    recovering: bool,
}

impl<E: EcIo> Runtime<E> {
    /// `force` takes over fans that look externally controlled (see `FanController::new`).
    pub fn new(ec: E, force: bool, mode: Mode, params: Params) -> Result<Self> {
        let controller = FanController::new(ec, force)?;
        let limits = controller.limits();
        let governor = Governor::new(mode, params, limits)?;
        let cpu = CpuTemp::discover()?;
        let gpu = GpuTemp::discover()?;
        Ok(Self {
            controller,
            governor,
            params,
            cpu,
            gpu,
            last_gpu: None,
            gpu_failures: 0,
            good_streak: 0,
            recovering: false,
        })
    }

    pub fn limits(&self) -> Limits {
        self.controller.limits()
    }

    pub fn params(&self) -> Params {
        self.params
    }

    pub fn mode(&self) -> &Mode {
        self.governor.mode()
    }

    pub fn has_gpu_sensor(&self) -> bool {
        self.gpu.is_some()
    }

    /// Switch mode. The governor restarts from a clean state; the EC is only
    /// touched by the next `tick`.
    pub fn set_mode(&mut self, mode: Mode) -> Result<()> {
        self.governor = Governor::new(mode, self.params, self.limits())?;
        Ok(())
    }

    /// Replace the governor parameters (e.g. another AUTO profile), keeping the mode.
    pub fn set_params(&mut self, params: Params) -> Result<()> {
        let mode = self.governor.mode().clone();
        self.governor = Governor::new(mode, params, self.limits())?;
        self.params = params;
        Ok(())
    }

    /// One control step. `Err` only for EC failures, which are fatal for the
    /// caller (dropping the runtime still hands the fans back).
    pub fn tick(&mut self) -> Result<TickReport> {
        let cpu_r = self.cpu.read_c();
        let gpu_r = match &self.gpu {
            Some(g) => g.read_c(),
            None => Ok(None),
        };

        let mut note = None;
        let (inputs, failure) = match (cpu_r, gpu_r) {
            (Ok(c), Ok(g)) => {
                self.gpu_failures = 0;
                self.last_gpu = g;
                (Some(Inputs { cpu_c: c, gpu_c: g }), None)
            }
            (Ok(c), Err(e)) => {
                self.gpu_failures += 1;
                if self.gpu_failures < GPU_FAIL_TOLERANCE {
                    note = Some(format!("GPU read failed ({e}); reusing last value"));
                    (Some(Inputs { cpu_c: c, gpu_c: self.last_gpu }), None)
                } else {
                    (None, Some(format!("GPU sensor failing: {e}")))
                }
            }
            (Err(e), _) => (None, Some(format!("CPU sensor failed: {e}"))),
        };

        let (decision, health, cpu_c, gpu_c, effective_c) = match inputs {
            None => {
                self.good_streak = 0;
                self.recovering = true;
                self.governor.reset();
                (Decision::Bios, Health::SensorFailure(failure.unwrap_or_default()), None, None, None)
            }
            Some(i) => {
                self.good_streak += 1;
                let eff = self.governor.effective_c(i);
                if self.recovering && self.good_streak < RECOVERY_TICKS {
                    (Decision::Bios, Health::Recovering(self.good_streak), Some(i.cpu_c), i.gpu_c, Some(eff))
                } else {
                    self.recovering = false;
                    (self.governor.decide(i), Health::Ok, Some(i.cpu_c), i.gpu_c, Some(eff))
                }
            }
        };

        let (level, engaged, reasserted) = match decision {
            Decision::Bios => {
                self.controller.release()?;
                (self.controller.bios_level()?, false, false)
            }
            Decision::Level(l) => {
                let applied = self.controller.set_level(l)?;
                (applied.level, true, applied.reasserted)
            }
        };

        Ok(TickReport {
            cpu_c,
            gpu_c,
            effective_c,
            decision,
            level,
            engaged,
            hard_override: self.governor.hard_active(),
            reasserted,
            health,
            note,
        })
    }

    /// Hand the fans back to the BIOS now (also happens on drop).
    pub fn release(&mut self) -> Result<()> {
        self.controller.release()
    }
}

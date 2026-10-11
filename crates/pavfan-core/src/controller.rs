// SPDX-License-Identifier: GPL-3.0-or-later
//! The only code allowed to write to the EC.
//!
//! Register map (verified on hardware, see docs/ec-register-map.md):
//!   0x0F bit3  override enable (FNSW)
//!   0x11       current level, x100 RPM (read only)
//!   0x12/0x13  maximum / minimum level (read only)
//!   0x14       override target level, 0xFF = unset
//!
//! Invariants enforced here:
//!   * only 0x0F bit 3 and 0x14 are ever written;
//!   * engage: target first, then the enable bit; release: enable bit first,
//!     then the target back to 0xFF;
//!   * every target is clamped to the EC's own [min, max];
//!   * we refuse to start if something else already controls the fans;
//!   * Drop releases to the BIOS, including on panic unwinding.

use crate::ec::EcIo;
use crate::error::{Error, Result};

pub const REG_FLAGS: u8 = 0x0F;
pub const REG_LEVEL: u8 = 0x11;
pub const REG_MAX: u8 = 0x12;
pub const REG_MIN: u8 = 0x13;
pub const REG_TARGET: u8 = 0x14;
pub const FLAG_OVERRIDE: u8 = 0x08;
pub const TARGET_UNSET: u8 = 0xFF;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub min: u8,
    pub max: u8,
}

impl Limits {
    pub fn clamp(self, level: u8) -> u8 {
        level.clamp(self.min, self.max)
    }

    /// 0..=100 % of the EC's own range.
    pub fn from_percent(self, percent: u8) -> u8 {
        let p = u32::from(percent.min(100));
        let span = u32::from(self.max - self.min);
        let level = u32::from(self.min) + (p * span + 50) / 100;
        u8::try_from(level).unwrap_or(self.max)
    }

    pub fn to_percent(self, level: u8) -> u8 {
        let span = u32::from(self.max - self.min).max(1);
        let rel = u32::from(self.clamp(level) - self.min);
        u8::try_from((rel * 100 + span / 2) / span).unwrap_or(100)
    }
}

/// Nominal RPM for a level (the EC unit is 100 RPM). Nominal only: there is no tachometer.
pub fn nominal_rpm(level: u8) -> u32 {
    u32::from(level) * 100
}

/// What `set_level` did.
#[derive(Debug, PartialEq, Eq)]
pub struct Applied {
    pub level: u8,
    /// True if we found the EC no longer in the state we left it (e.g. after
    /// suspend/resume) and had to write it again.
    pub reasserted: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct EcStatus {
    pub flags: u8,
    pub level: u8,
    pub target: u8,
    pub limits: Limits,
}

impl EcStatus {
    pub fn override_active(&self) -> bool {
        self.flags & FLAG_OVERRIDE != 0
    }
}

fn read_limits<E: EcIo>(ec: &mut E) -> Result<Limits> {
    let min = ec.read(REG_MIN)?;
    let max = ec.read(REG_MAX)?;
    // Sanity window taken from what the real EC reports (22..42). If the EC says
    // something wildly different, our register map is wrong: refuse.
    if !(10..=30).contains(&min) || !(30..=60).contains(&max) || min >= max {
        return Err(Error::EcState(format!(
            "fan limits min={min} max={max} are outside the expected window; refusing to drive the fans"
        )));
    }
    Ok(Limits { min, max })
}

pub fn read_status<E: EcIo>(ec: &mut E) -> Result<EcStatus> {
    Ok(EcStatus {
        flags: ec.read(REG_FLAGS)?,
        level: ec.read(REG_LEVEL)?,
        target: ec.read(REG_TARGET)?,
        limits: read_limits(ec)?,
    })
}

/// Stateless release for a *different* process than the one that engaged the
/// override (crashed or SIGKILLed daemon, `ExecStopPost=`, manual rescue).
/// Returns whether anything had to be changed.
pub fn emergency_release<E: EcIo>(ec: &mut E) -> Result<bool> {
    let mut changed = false;
    let flags = ec.read(REG_FLAGS)?;
    if flags & FLAG_OVERRIDE != 0 {
        ec.write(REG_FLAGS, flags & !FLAG_OVERRIDE)?; // BIOS back in charge first
        changed = true;
    }
    if ec.read(REG_TARGET)? != TARGET_UNSET {
        ec.write(REG_TARGET, TARGET_UNSET)?;
        changed = true;
    }
    let (t, f) = (ec.read(REG_TARGET)?, ec.read(REG_FLAGS)?);
    if t != TARGET_UNSET || f & FLAG_OVERRIDE != 0 {
        return Err(Error::EcState(format!("release not confirmed: 0x14=0x{t:02X} 0x0F=0x{f:02X}")));
    }
    Ok(changed)
}

pub struct FanController<E: EcIo> {
    ec: E,
    limits: Limits,
    /// True while we may have the override (or a target) written.
    engaged: bool,
}

impl<E: EcIo> FanController<E> {
    /// Validates the EC state. `force` allows taking over fans that appear to be
    /// under external control (override bit already set, or a target already written).
    pub fn new(mut ec: E, force: bool) -> Result<Self> {
        let st = read_status(&mut ec)?;
        if (st.override_active() || st.target != TARGET_UNSET) && !force {
            return Err(Error::ForeignControl(format!(
                "0x0F=0x{:02X} 0x14=0x{:02X}; is NBFC or another tool running? (--force to take over)",
                st.flags, st.target
            )));
        }
        Ok(Self { ec, limits: st.limits, engaged: false })
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    pub fn is_engaged(&self) -> bool {
        self.engaged
    }

    pub fn status(&mut self) -> Result<EcStatus> {
        read_status(&mut self.ec)
    }

    /// The level the BIOS itself is currently using. Only meaningful while released.
    pub fn bios_level(&mut self) -> Result<u8> {
        self.ec.read(REG_LEVEL)
    }

    /// Command a level (clamped to the EC limits) and verify it by reading it back.
    pub fn set_level(&mut self, level: u8) -> Result<Applied> {
        let level = self.limits.clamp(level);
        let mut reasserted = false;

        if self.engaged {
            let target = self.ec.read(REG_TARGET)?;
            let flags = self.ec.read(REG_FLAGS)?;
            if target == level && flags & FLAG_OVERRIDE != 0 {
                return Ok(Applied { level, reasserted: false });
            }
            // We believe the override is on. If the bit is gone, the EC was reset
            // behind our back (suspend/resume); otherwise we are merely changing level.
            reasserted = flags & FLAG_OVERRIDE == 0;
        }

        // From here on we may leave the EC modified: Drop must undo it.
        self.engaged = true;
        self.ec.write(REG_TARGET, level)?; // value first...
        let flags = self.ec.read(REG_FLAGS)?;
        if flags & FLAG_OVERRIDE == 0 {
            self.ec.write(REG_FLAGS, flags | FLAG_OVERRIDE)?; // ...then enable
        }

        let (t, f) = (self.ec.read(REG_TARGET)?, self.ec.read(REG_FLAGS)?);
        if t != level || f & FLAG_OVERRIDE == 0 {
            let _ = self.release();
            return Err(Error::EcState(format!(
                "read-back mismatch after write: wanted target=0x{level:02X}+override, got 0x14=0x{t:02X} 0x0F=0x{f:02X}"
            )));
        }
        Ok(Applied { level, reasserted })
    }

    /// Hand the fans back to the BIOS: enable bit first, then the target.
    pub fn release(&mut self) -> Result<()> {
        if !self.engaged {
            return Ok(());
        }
        let flags = self.ec.read(REG_FLAGS)?;
        if flags & FLAG_OVERRIDE != 0 {
            self.ec.write(REG_FLAGS, flags & !FLAG_OVERRIDE)?;
        }
        // Only after the BIOS has control again: never write 0xFF under an active override.
        self.ec.write(REG_TARGET, TARGET_UNSET)?;

        let (t, f) = (self.ec.read(REG_TARGET)?, self.ec.read(REG_FLAGS)?);
        if t != TARGET_UNSET || f & FLAG_OVERRIDE != 0 {
            return Err(Error::EcState(format!(
                "release not confirmed: 0x14=0x{t:02X} 0x0F=0x{f:02X}"
            )));
        }
        self.engaged = false;
        Ok(())
    }
}

impl<E: EcIo> Drop for FanController<E> {
    fn drop(&mut self) {
        if self.engaged
            && let Err(e) = self.release()
        {
            eprintln!("pavfan: CRITICAL: failed to hand the fans back to the BIOS on exit: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ec::MemEc;

    #[test]
    fn engage_writes_target_before_enable_bit() {
        let mut ec = MemEc::bios_idle();
        {
            let mut c = FanController::new(&mut ec, false).unwrap();
            assert_eq!(c.set_level(32).unwrap(), Applied { level: 32, reasserted: false });
            assert!(c.is_engaged());
        }
        // engage, then Drop's release
        assert_eq!(
            ec.writes,
            vec![(REG_TARGET, 32), (REG_FLAGS, 0x09), (REG_FLAGS, 0x01), (REG_TARGET, 0xFF)]
        );
    }

    #[test]
    fn release_clears_enable_bit_before_restoring_target() {
        let mut ec = MemEc::bios_idle();
        {
            let mut c = FanController::new(&mut ec, false).unwrap();
            c.set_level(40).unwrap();
            c.release().unwrap();
            assert!(!c.is_engaged());
        }
        let w = &ec.writes;
        let flag_clear = w.iter().position(|&x| x == (REG_FLAGS, 0x01)).unwrap();
        let target_unset = w.iter().position(|&x| x == (REG_TARGET, 0xFF)).unwrap();
        assert!(flag_clear < target_unset, "BIOS must regain control before target is reset");
    }

    #[test]
    fn levels_are_clamped_to_ec_limits() {
        let mut ec = MemEc::bios_idle();
        let mut c = FanController::new(&mut ec, false).unwrap();
        assert_eq!(c.set_level(0).unwrap().level, 22);
        assert_eq!(c.set_level(255).unwrap().level, 42);
        assert_eq!(c.set_level(30).unwrap().level, 30);
    }

    #[test]
    fn unchanged_level_causes_no_extra_writes() {
        let mut ec = MemEc::bios_idle();
        let mut c = FanController::new(&mut ec, false).unwrap();
        c.set_level(35).unwrap();
        let n = {
            c.set_level(35).unwrap();
            c.set_level(35).unwrap();
            c.status().unwrap().target
        };
        assert_eq!(n, 35);
        drop(c);
        // 2 engage writes + 2 release writes, nothing for the repeated calls
        assert_eq!(ec.writes.len(), 4);
    }

    #[test]
    fn refuses_when_something_else_controls_the_fans() {
        let mut ec = MemEc::bios_idle();
        ec.mem[usize::from(REG_FLAGS)] = 0x09;
        assert!(matches!(FanController::new(&mut ec, false), Err(Error::ForeignControl(_))));
        let mut ec = MemEc::bios_idle();
        ec.mem[usize::from(REG_TARGET)] = 0x20;
        assert!(matches!(FanController::new(&mut ec, false), Err(Error::ForeignControl(_))));
        // forced takeover is allowed, and writes nothing by itself
        let mut ec = MemEc::bios_idle();
        ec.mem[usize::from(REG_FLAGS)] = 0x09;
        assert!(FanController::new(&mut ec, true).is_ok());
        assert!(ec.writes.is_empty());
    }

    #[test]
    fn refuses_implausible_limits() {
        for (min, max) in [(0u8, 42u8), (22, 200), (42, 22), (22, 22)] {
            let mut ec = MemEc::bios_idle();
            ec.mem[usize::from(REG_MIN)] = min;
            ec.mem[usize::from(REG_MAX)] = max;
            assert!(matches!(FanController::new(&mut ec, false), Err(Error::EcState(_))), "{min}..{max}");
        }
    }

    #[test]
    fn drop_hands_fans_back_even_without_explicit_release() {
        let mut ec = MemEc::bios_idle();
        {
            let mut c = FanController::new(&mut ec, false).unwrap();
            c.set_level(42).unwrap();
        }
        assert_eq!(ec.peek(REG_FLAGS), 0x01);
        assert_eq!(ec.peek(REG_TARGET), 0xFF);
    }

    #[test]
    fn panic_unwinding_still_releases() {
        let mut ec = MemEc::bios_idle();
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut c = FanController::new(&mut ec, false).unwrap();
            c.set_level(42).unwrap();
            panic!("simulated bug in the control loop");
        }));
        assert!(r.is_err());
        assert_eq!(ec.peek(REG_FLAGS), 0x01);
        assert_eq!(ec.peek(REG_TARGET), 0xFF);
    }

    #[test]
    fn only_whitelisted_registers_are_ever_written() {
        let mut ec = MemEc::bios_idle();
        {
            let mut c = FanController::new(&mut ec, false).unwrap();
            for l in [22, 30, 42, 25] {
                c.set_level(l).unwrap();
            }
            c.release().unwrap();
            c.set_level(33).unwrap();
        }
        assert!(ec.writes.iter().all(|&(off, _)| off == REG_FLAGS || off == REG_TARGET));
        // the enable bit is the only bit of 0x0F ever changed
        assert!(ec.writes.iter().filter(|w| w.0 == REG_FLAGS).all(|&(_, v)| v & !FLAG_OVERRIDE == 0x01));
    }

    #[test]
    fn unrelated_flag_bits_are_preserved() {
        let mut ec = MemEc::bios_idle();
        ec.mem[usize::from(REG_FLAGS)] = 0x05; // e.g. a different BIOS state: bits 0 and 2
        {
            let mut c = FanController::new(&mut ec, false).unwrap();
            c.set_level(30).unwrap();
            assert_eq!(c.status().unwrap().flags, 0x0D);
        }
        assert_eq!(ec.peek(REG_FLAGS), 0x05);
    }

    #[test]
    fn detects_and_repairs_an_ec_reset_behind_our_back() {
        let mut ec = MemEc::bios_idle();
        let mut c = FanController::new(&mut ec, false).unwrap();
        c.set_level(35).unwrap();
        // simulate resume from suspend: EC forgets the override
        c.ec.mem[usize::from(REG_FLAGS)] = 0x01;
        c.ec.mem[usize::from(REG_TARGET)] = 0xFF;
        let a = c.set_level(35).unwrap();
        assert!(a.reasserted);
        assert_eq!(c.status().unwrap().target, 35);
    }

    #[test]
    fn write_failure_is_an_error_and_never_panics() {
        let mut ec = MemEc::bios_idle();
        ec.fail_writes_after = Some(1);
        let mut c = FanController::new(&mut ec, false).unwrap();
        assert!(c.set_level(35).is_err());
    }

    #[test]
    fn emergency_release_recovers_a_dead_process_state() {
        let mut ec = MemEc::bios_idle();
        ec.mem[usize::from(REG_FLAGS)] = 0x09;
        ec.mem[usize::from(REG_TARGET)] = 22;
        assert!(emergency_release(&mut ec).unwrap());
        assert_eq!(ec.writes, vec![(REG_FLAGS, 0x01), (REG_TARGET, 0xFF)]);
        // idempotent: nothing left to do, nothing written
        assert!(!emergency_release(&mut ec).unwrap());
        assert_eq!(ec.writes.len(), 2);
    }

    #[test]
    fn limits_percent_mapping() {
        let l = Limits { min: 22, max: 42 };
        assert_eq!(l.from_percent(0), 22);
        assert_eq!(l.from_percent(100), 42);
        assert_eq!(l.from_percent(50), 32);
        assert_eq!(l.from_percent(250), 42);
        assert_eq!(l.to_percent(22), 0);
        assert_eq!(l.to_percent(42), 100);
        assert_eq!(l.to_percent(32), 50);
    }
}

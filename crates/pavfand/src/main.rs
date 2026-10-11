// SPDX-License-Identifier: GPL-3.0-or-later
//! `pavfand`: the pavfan system daemon.
//!
//! Runs as root under systemd, drives the fans through `pavfan_core::runtime`
//! (the same loop the CLI uses) and exposes status/mode switching on the D-Bus
//! system bus. Exit codes: 3 = unsupported machine, 4 = no EC access; systemd is
//! told not to restart on those.

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod config;
mod dbus;
mod notify;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use pavfan_core::controller::{Limits, TARGET_UNSET, emergency_release, nominal_rpm, read_status};
use pavfan_core::ec::{DebugfsEc, EcIo, MemEc, ensure_ec_sys_writable};
use pavfan_core::governor::Mode;
use pavfan_core::machine::{self, Compat, DMI_DIR};
use pavfan_core::runtime::{Health, RECOVERY_TICKS, Runtime, TickReport};
use pavfan_core::{Error, Result};
use pavfan_ipc::{BUS_NAME, OBJECT_PATH, Status};
use zbus::blocking::Connection;

use crate::config::{Config, State, curve_from_points, load_curve_points, save_curve_points};
use crate::dbus::{Daemon1, Request, Shared, lock};
use crate::notify::Notifier;

const EXIT_UNSUPPORTED: u8 = 3;
const EXIT_NO_EC: u8 = 4;

#[derive(Parser)]
#[command(name = "pavfand", version, about = "pavfan fan-control daemon for the HP Pavilion Gaming 15-dk1xxx")]
struct Cli {
    #[arg(long, default_value = "/etc/pavfan/config.toml")]
    config: PathBuf,
    /// Where the last chosen mode and MANUAL curve are remembered.
    #[arg(long, default_value = "/var/lib/pavfan")]
    state_dir: PathBuf,
    /// In-memory fake EC: nothing is written to the hardware.
    #[arg(long)]
    dry_run: bool,
    /// Serve on the session bus instead of the system bus (development).
    #[arg(long)]
    session_bus: bool,
    /// Allow a BIOS version the register map was not verified on (risky).
    #[arg(long)]
    allow_unverified_bios: bool,
    /// Do not (re)load ec_sys; expect it to be loaded with write_support=1.
    #[arg(long)]
    no_module_setup: bool,
    /// Control loop period (overrides the config file).
    #[arg(long, value_parser = clap::value_parser!(u64).range(250..=10000))]
    interval_ms: Option<u64>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pavfand: fatal: {e}");
            match e {
                Error::UnsupportedMachine(_) => ExitCode::from(EXIT_UNSUPPORTED),
                Error::EcAccess(_) => ExitCode::from(EXIT_NO_EC),
                _ => ExitCode::FAILURE,
            }
        }
    }
}

fn run(cli: &Cli) -> Result<()> {
    let cfg = Config::load(&cli.config)?;
    let params = cfg.params()?;

    let id = machine::read_identity(Path::new(DMI_DIR))?;
    match machine::check(&id, cli.allow_unverified_bios)? {
        Compat::Verified => println!("pavfand: machine {} (board {}), BIOS {}", id.product, id.board, id.bios),
        Compat::UnverifiedBios(b) => {
            eprintln!("pavfand: WARNING: BIOS {b} is not verified; the EC register map may differ")
        }
    }

    let interval = Duration::from_millis(cli.interval_ms.unwrap_or(cfg.interval_ms).clamp(250, 10_000));
    let state = State::load(&cli.state_dir, &cfg);

    if cli.dry_run {
        println!("pavfand: DRY RUN: in-memory EC, hardware untouched");
        return serve(MemEc::bios_idle(), cli, &cfg, params, state, interval);
    }
    if !cli.no_module_setup {
        ensure_ec_sys_writable()?;
    }
    serve(DebugfsEc::open()?, cli, &cfg, params, state, interval)
}

fn serve<E: EcIo>(
    mut ec: E,
    cli: &Cli,
    cfg: &Config,
    params: pavfan_core::governor::Params,
    state: State,
    interval: Duration,
) -> Result<()> {
    // A previous instance that was SIGKILLed may have left its override in place.
    // This daemon is the system's fan owner, so take over rather than refuse.
    let st = read_status(&mut ec)?;
    if st.override_active() || st.target != TARGET_UNSET {
        eprintln!(
            "pavfand: EC override was left engaged (0x0F=0x{:02X} 0x14=0x{:02X}); releasing it before start",
            st.flags, st.target
        );
        emergency_release(&mut ec)?;
    }
    let limits = st.limits;

    let manual_points = load_curve_points(&cli.state_dir, cfg, limits);
    let mode = match build_mode(&state.mode, state.fixed_percent, &manual_points, limits) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("pavfand: saved mode '{}' is unusable ({e}); starting in AUTO", state.mode);
            Mode::Auto
        }
    };
    let mode_name = mode.name().to_owned();

    let mut rt = Runtime::new(ec, false, mode, params)?;
    if !rt.has_gpu_sensor() {
        eprintln!("pavfand: note: no NVIDIA GPU sensor available; deciding on CPU temperature only");
    }
    println!(
        "pavfand: mode {mode_name} | EC limits {}..{} | tick {} ms | effective = max(CPU, GPU {:+.0} C) | engage {:.0} C, release {:.0} C, hard max {:.0} C",
        limits.min, limits.max, interval.as_millis(), params.gpu_bias_c, params.engage_c, params.disengage_c, params.hard_c
    );

    let shared = Arc::new(Mutex::new(Shared::new(limits, mode_name, state.fixed_percent, manual_points)));
    let conn = match connect(cli.session_bus, Arc::clone(&shared)) {
        Ok(c) => {
            println!("pavfand: serving {BUS_NAME} on the {} bus", if cli.session_bus { "session" } else { "system" });
            Some(c)
        }
        Err(e) => {
            eprintln!("pavfand: WARNING: D-Bus unavailable ({e}); fan control continues without IPC");
            None
        }
    };

    let stop = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM, signal_hook::consts::SIGHUP] {
        signal_hook::flag::register(sig, Arc::clone(&stop)).map_err(|e| Error::io("installing signal handler", e))?;
    }

    let sd = Notifier::from_env();
    sd.send("READY=1");
    if sd.active() {
        println!("pavfand: systemd notify active (watchdog armed)");
    }

    let mut last_key = String::new();
    let outcome = loop {
        if stop.load(Ordering::Relaxed) {
            break Ok(());
        }

        let requests = std::mem::take(&mut lock(&shared).pending);
        for req in requests {
            apply(&mut rt, &shared, req, &cli.state_dir, conn.as_ref());
        }

        let report = match rt.tick() {
            Ok(r) => r,
            Err(e) => break Err(e),
        };

        let status = {
            let mut s = lock(&shared);
            let status = make_status(&report, limits, &s, rt.has_gpu_sensor());
            s.status = status.clone();
            status
        };

        let key = format!("{}|{}|{}|{}", status.mode, status.engaged, status.level, status.health);
        if key != last_key {
            println!("pavfand: {}", summary(&status));
            last_key = key;
        }
        if let Some(n) = &report.note {
            eprintln!("pavfand: {n}");
        }
        if report.reasserted {
            eprintln!("pavfand: EC had dropped the override (suspend/resume?); re-applied");
        }
        sd.send(&format!("WATCHDOG=1\nSTATUS={}", summary(&status)));

        // Sleep in slices: react to signals promptly and to requests within ~50 ms.
        let t0 = Instant::now();
        while t0.elapsed() < interval && !stop.load(Ordering::Relaxed) {
            if !lock(&shared).pending.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
    };

    sd.send("STOPPING=1");
    let released = rt.release();
    match &released {
        Ok(()) => println!("pavfand: fans handed back to the BIOS"),
        Err(e) => eprintln!("pavfand: CRITICAL: could not hand the fans back: {e}"),
    }
    outcome?;
    released
}

fn connect(session: bool, shared: Arc<Mutex<Shared>>) -> Result<Connection> {
    let builder = if session {
        zbus::blocking::connection::Builder::session()
    } else {
        zbus::blocking::connection::Builder::system()
    };
    builder
        .and_then(|b| b.serve_at(OBJECT_PATH, Daemon1 { shared }))
        .and_then(|b| b.name(BUS_NAME))
        .and_then(|b| b.build())
        .map_err(|e| Error::Ipc(e.to_string()))
}

fn build_mode(name: &str, fixed_percent: u8, points: &[(f64, u8)], limits: Limits) -> Result<Mode> {
    match name {
        "off" => Ok(Mode::Off),
        "auto" => Ok(Mode::Auto),
        "fixed" => Ok(Mode::Fixed(limits.from_percent(fixed_percent))),
        "manual" => Ok(Mode::Manual(curve_from_points(points, limits)?)),
        other => Err(Error::Config(format!("unknown mode '{other}'"))),
    }
}

/// Apply one queued request: rebuild the governor, persist, announce.
fn apply<E: EcIo>(rt: &mut Runtime<E>, shared: &Arc<Mutex<Shared>>, req: Request, state_dir: &Path, conn: Option<&Connection>) {
    let (cur_mode, cur_fixed, cur_points) = {
        let s = lock(shared);
        (s.mode_name.clone(), s.fixed_percent, s.manual_points.clone())
    };
    let curve_changed = matches!(req, Request::Curve(_));
    let (mode_name, fixed, points) = match req {
        Request::Mode(m) => (m, cur_fixed, cur_points),
        Request::FixedPercent(p) => (cur_mode, p, cur_points),
        Request::Curve(pts) => (cur_mode, cur_fixed, pts),
    };

    let mode = match build_mode(&mode_name, fixed, &points, rt.limits()) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("pavfand: request rejected: {e}");
            return;
        }
    };
    if let Err(e) = rt.set_mode(mode) {
        eprintln!("pavfand: could not switch mode: {e}");
        return;
    }

    let mode_changed = {
        let mut s = lock(shared);
        let changed = s.mode_name != mode_name;
        s.mode_name = mode_name.clone();
        s.fixed_percent = fixed;
        s.manual_points = points.clone();
        changed
    };
    println!("pavfand: request applied: mode {mode_name}, fixed {fixed}%, manual curve {} points", points.len());
    State { mode: mode_name.clone(), fixed_percent: fixed }.save(state_dir);
    if curve_changed {
        save_curve_points(state_dir, &points);
    }
    if mode_changed && let Some(conn) = conn {
        match conn.object_server().interface::<_, Daemon1>(OBJECT_PATH) {
            Ok(iface) => {
                if let Err(e) = zbus::block_on(Daemon1::mode_changed(iface.signal_emitter(), &mode_name)) {
                    eprintln!("pavfand: could not emit ModeChanged: {e}");
                }
            }
            Err(e) => eprintln!("pavfand: could not emit ModeChanged: {e}"),
        }
    }
}

fn make_status(r: &TickReport, limits: Limits, s: &Shared, gpu_present: bool) -> Status {
    let (health, detail) = match &r.health {
        Health::Ok => ("ok", r.note.clone().unwrap_or_default()),
        Health::SensorFailure(m) => ("sensor-failure", m.clone()),
        Health::Recovering(n) => ("recovering", format!("sensors recovering ({n}/{RECOVERY_TICKS})")),
    };
    Status {
        mode: s.mode_name.clone(),
        fixed_percent: s.fixed_percent,
        has_manual_curve: !s.manual_points.is_empty(),
        level: r.level,
        percent: limits.to_percent(r.level),
        min_level: limits.min,
        max_level: limits.max,
        engaged: r.engaged,
        hard_override: r.hard_override,
        cpu_c: r.cpu_c.map_or(-1.0, f64::from),
        gpu_c: r.gpu_c.map_or(-1.0, f64::from),
        gpu_present,
        effective_c: r.effective_c.map_or(-1.0, f64::from),
        health: health.into(),
        detail,
        version: env!("CARGO_PKG_VERSION").into(),
    }
}

fn summary(s: &Status) -> String {
    let temps = if s.cpu_c < 0.0 {
        "temps unavailable".to_owned()
    } else if s.gpu_c < 0.0 {
        format!("cpu {:.0}C gpu -- eff {:.0}C", s.cpu_c, s.effective_c)
    } else {
        format!("cpu {:.0}C gpu {:.0}C eff {:.0}C", s.cpu_c, s.gpu_c, s.effective_c)
    };
    let fans = if s.engaged {
        format!("level {} ({}%, {} RPM nominal){}", s.level, s.percent, nominal_rpm(s.level), if s.hard_override { ", HARD OVERRIDE" } else { "" })
    } else {
        format!("BIOS control (level {})", s.level)
    };
    let health = if s.health == "ok" { String::new() } else { format!(" [{}: {}]", s.health, s.detail) };
    format!("mode {} | {temps} | {fans}{health}", s.mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    const L: Limits = Limits { min: 22, max: 42 };

    #[test]
    fn build_mode_maps_names_and_validates() {
        assert!(matches!(build_mode("off", 50, &[], L), Ok(Mode::Off)));
        assert!(matches!(build_mode("auto", 50, &[], L), Ok(Mode::Auto)));
        assert!(matches!(build_mode("fixed", 50, &[], L), Ok(Mode::Fixed(32))));
        assert!(matches!(build_mode("fixed", 250, &[], L), Ok(Mode::Fixed(42))));
        assert!(build_mode("manual", 50, &[], L).is_err());
        assert!(matches!(build_mode("manual", 50, &[(60.0, 20), (80.0, 100)], L), Ok(Mode::Manual(_))));
        assert!(build_mode("turbo", 50, &[], L).is_err());
    }
}

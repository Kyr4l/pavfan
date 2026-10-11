// SPDX-License-Identifier: GPL-3.0-or-later
//! `pavfan`: command-line front-end.
//!
//! Two roles:
//!  * client of the `pavfand` daemon (`status`, `set`): talks D-Bus, no root needed;
//!  * stand-alone test unit (`run`, `release`): drives the EC directly as root,
//!    with the same control loop the daemon uses. `run` refuses while the daemon
//!    is active, so there is only ever one fan owner.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use clap::{Args, Parser, Subcommand, ValueEnum};
use pavfan_core::controller::{Limits, emergency_release, nominal_rpm, read_status};
use pavfan_core::curve::{default_auto, parse_curve};
use pavfan_core::ec::{
    DebugfsEc, EC_IO_PATH, EcIo, MemEc, ec_sys_loaded, ec_sys_write_enabled, ensure_ec_sys_loaded,
    ensure_ec_sys_writable,
};
use pavfan_core::governor::{Decision, Mode, Params};
use pavfan_core::machine::{self, Compat, DMI_DIR};
use pavfan_core::runtime::{Health, RECOVERY_TICKS, Runtime};
use pavfan_core::sensors::{CpuTemp, GpuTemp};
use pavfan_core::{Error, Result};
use pavfan_ipc::{BUS_NAME, Daemon1ProxyBlocking, Status};
use zbus::blocking::Connection;
use zbus::names::BusName;

#[derive(Parser)]
#[command(name = "pavfan", version, about = "Fan control for the HP Pavilion Gaming 15-dk1xxx")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Args, Clone)]
struct Common {
    /// Use an in-memory fake EC: nothing is written to the hardware.
    #[arg(long)]
    dry_run: bool,
    /// Allow a BIOS version the register map was not verified on (risky).
    #[arg(long)]
    allow_unverified_bios: bool,
    /// Do not (re)load ec_sys automatically.
    #[arg(long)]
    no_module_setup: bool,
    /// Talk to a daemon on the session bus instead of the system bus (development).
    #[arg(long)]
    session_bus: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show daemon status (if running), machine compatibility, EC state and temperatures.
    Status(Common),
    /// Ask the running daemon to change mode: off | auto | fixed [PERCENT] | manual.
    Set {
        #[arg(value_enum)]
        mode: ModeArg,
        /// FIXED: percent of the fan range (0-100). Keeps the previous value if omitted.
        percent: Option<u8>,
        #[command(flatten)]
        common: Common,
    },
    /// Stand-alone test unit: control the fans in the foreground until Ctrl-C (root).
    Run(RunArgs),
    /// Rescue: hand the fans back to the BIOS (clears a stuck override).
    Release(Common),
    /// Print the built-in AUTO curve, or validate a curve file.
    Curve {
        #[arg(long)]
        file: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, ValueEnum, PartialEq, Eq)]
enum ModeArg {
    Off,
    Auto,
    Fixed,
    Manual,
}

impl ModeArg {
    fn name(self) -> &'static str {
        match self {
            ModeArg::Off => "off",
            ModeArg::Auto => "auto",
            ModeArg::Fixed => "fixed",
            ModeArg::Manual => "manual",
        }
    }
}

#[derive(Args)]
struct RunArgs {
    #[command(flatten)]
    common: Common,
    /// OFF = BIOS control, AUTO = built-in curve, FIXED = constant, MANUAL = your curve.
    #[arg(long, value_enum, default_value = "auto")]
    mode: ModeArg,
    /// FIXED: level in units of 100 RPM (clamped to the EC range, 22..42).
    #[arg(long, conflicts_with = "percent")]
    level: Option<u8>,
    /// FIXED: 0-100 % of the EC range.
    #[arg(long)]
    percent: Option<u8>,
    /// MANUAL: TOML curve file (see `pavfan curve`).
    #[arg(long)]
    curve: Option<PathBuf>,
    /// Control loop period.
    #[arg(long, default_value_t = 1000, value_parser = clap::value_parser!(u64).range(250..=10000))]
    interval_ms: u64,
    /// Stop after this many seconds (default: run until interrupted).
    #[arg(long)]
    duration: Option<u64>,
    /// AUTO: take over from the BIOS at/above this effective temperature (C).
    #[arg(long, default_value_t = 68.0)]
    engage_c: f32,
    /// AUTO: hand back to the BIOS at/below this effective temperature (C).
    #[arg(long, default_value_t = 62.0)]
    disengage_c: f32,
    /// Added to the GPU temperature before comparing it with the CPU's (-20..20).
    /// The BIOS ignores GPU heat and the GPU fan is the one the EC setpoint really drives.
    #[arg(long, default_value_t = 8.0)]
    gpu_bias_c: f32,
    /// Hard thermal override: maximum fans at/above this effective temperature,
    /// in every mode but OFF (at most 90).
    #[arg(long, default_value_t = 88.0)]
    hard_c: f32,
    /// Take over even if the fans look externally controlled (NBFC, or a running pavfand).
    #[arg(long)]
    force: bool,
    /// Print only state changes and errors.
    #[arg(long)]
    quiet: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Status(c) => status(&c),
        Cmd::Set { mode, percent, common } => set(&common, mode, percent),
        Cmd::Run(a) => run(&a),
        Cmd::Release(c) => release(&c),
        Cmd::Curve { file } => curve(file.as_deref()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("pavfan: error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn is_root() -> bool {
    std::fs::metadata("/proc/self").map(|m| m.uid() == 0).unwrap_or(false)
}

fn check_machine(c: &Common) -> Result<()> {
    let id = machine::read_identity(Path::new(DMI_DIR))?;
    println!("machine: {} (board {}), BIOS {}", id.product, id.board, id.bios);
    match machine::check(&id, c.allow_unverified_bios)? {
        Compat::Verified => {}
        Compat::UnverifiedBios(b) => {
            eprintln!("pavfan: WARNING: BIOS {b} is not verified; the EC register map may differ");
        }
    }
    Ok(())
}

fn require_root() -> Result<()> {
    if is_root() { Ok(()) } else { Err(Error::EcAccess("EC access needs root (try sudo), or use --dry-run".into())) }
}

fn open_rw(c: &Common) -> Result<DebugfsEc> {
    require_root()?;
    if !c.no_module_setup {
        ensure_ec_sys_writable()?;
    }
    DebugfsEc::open()
}

// ------------------------------------------------------------- daemon client --

fn bus_label(c: &Common) -> &'static str {
    if c.session_bus { "session" } else { "system" }
}

fn bus(c: &Common) -> Result<Connection> {
    let conn = if c.session_bus { Connection::session() } else { Connection::system() };
    conn.map_err(|e| Error::Ipc(format!("connecting to the {} bus: {e}", bus_label(c))))
}

fn daemon_running(c: &Common) -> bool {
    let Ok(conn) = bus(c) else { return false };
    let Ok(dbus) = zbus::blocking::fdo::DBusProxy::new(&conn) else { return false };
    let Ok(name) = BusName::try_from(BUS_NAME) else { return false };
    dbus.name_has_owner(name).unwrap_or(false)
}

fn daemon_proxy(c: &Common) -> Result<Daemon1ProxyBlocking<'static>> {
    let conn = bus(c)?;
    Daemon1ProxyBlocking::new(&conn).map_err(|e| Error::Ipc(e.to_string()))
}

fn print_daemon_status(s: &Status) {
    let fixed = if s.mode == "fixed" { format!(" ({}%)", s.fixed_percent) } else { String::new() };
    println!(
        "pavfand {}: mode {}{fixed}{}",
        s.version,
        s.mode,
        if s.has_manual_curve { "" } else { " [no manual curve configured]" }
    );
    if s.engaged {
        println!(
            "  fans: level {} ({}%, {} RPM nominal), pavfan controlling{}",
            s.level,
            s.percent,
            nominal_rpm(s.level),
            if s.hard_override { " [HARD THERMAL OVERRIDE]" } else { "" }
        );
    } else {
        println!("  fans: BIOS control (EC level {})", s.level);
    }
    let gpu = if !s.gpu_present {
        "no GPU sensor".to_owned()
    } else if s.gpu_c < 0.0 {
        "GPU asleep".to_owned()
    } else {
        format!("GPU {:.0} C", s.gpu_c)
    };
    if s.cpu_c >= 0.0 {
        println!("  temps: CPU {:.0} C, {gpu}, effective {:.0} C", s.cpu_c, s.effective_c);
    }
    if s.health != "ok" {
        println!("  health: {} ({})", s.health, s.detail);
    } else if !s.detail.is_empty() {
        println!("  note: {}", s.detail);
    }
}

// ---------------------------------------------------------------- status --

fn status(c: &Common) -> Result<()> {
    check_machine(c)?;
    match daemon_proxy(c).and_then(|p| p.get_status().map_err(|e| Error::Ipc(e.to_string()))) {
        Ok(s) => print_daemon_status(&s),
        Err(_) => println!("pavfand: not running (or not reachable on the {} bus)", bus_label(c)),
    }

    if !c.dry_run && !c.no_module_setup && is_root() {
        // Read-only module is enough to look; never enables write support here.
        if let Err(e) = ensure_ec_sys_loaded() {
            eprintln!("pavfan: note: could not load ec_sys: {e}");
        }
    } else if !c.dry_run && !is_root() {
        println!("note: not running as root; run with sudo to read the EC directly");
    }
    println!("ec_sys: loaded={} write_support={}", ec_sys_loaded(), ec_sys_write_enabled());
    let ec_state = if c.dry_run {
        read_status(&mut MemEc::bios_idle())
    } else {
        DebugfsEc::open_read_only().and_then(|mut ec| read_status(&mut ec))
    };
    match ec_state {
        Ok(s) => println!(
            "EC: level={} ({} RPM nominal) target=0x{:02X} flags=0x{:02X} override={} limits={}..{}",
            s.level,
            nominal_rpm(s.level),
            s.target,
            s.flags,
            if s.override_active() { "ACTIVE" } else { "off (BIOS control)" },
            s.limits.min,
            s.limits.max
        ),
        Err(e) => println!("EC: unavailable ({e}); expected without root: {EC_IO_PATH}"),
    }
    match CpuTemp::discover().and_then(|s| s.read_c()) {
        Ok(t) => println!("CPU: {t:.0} C"),
        Err(e) => println!("CPU: {e}"),
    }
    match GpuTemp::discover() {
        Ok(Some(g)) => match g.read_c() {
            Ok(Some(t)) => println!("GPU: {t:.0} C"),
            Ok(None) => println!("GPU: asleep (runtime-suspended; not woken)"),
            Err(e) => println!("GPU: {e}"),
        },
        Ok(None) => println!("GPU: no NVIDIA GPU / nvidia-smi found"),
        Err(e) => println!("GPU: {e}"),
    }
    Ok(())
}

// ------------------------------------------------------------------- set --

fn set(c: &Common, mode: ModeArg, percent: Option<u8>) -> Result<()> {
    if let Some(p) = percent
        && (p > 100 || mode != ModeArg::Fixed)
    {
        return Err(Error::Config("a percent (0-100) only makes sense with `set fixed`".into()));
    }
    let proxy = daemon_proxy(c)?;
    let ipc = |e: zbus::Error| Error::Ipc(e.to_string());
    if let Some(p) = percent {
        proxy.set_fixed_percent(p).map_err(ipc)?;
    }
    proxy.set_mode(mode.name()).map_err(ipc)?;
    // Give the control loop one tick to apply it, then show the result.
    thread::sleep(Duration::from_millis(1300));
    let s = proxy.get_status().map_err(ipc)?;
    print_daemon_status(&s);
    Ok(())
}

// --------------------------------------------------------------- release --

fn release(c: &Common) -> Result<()> {
    check_machine(c)?;
    if c.dry_run {
        let mut ec = MemEc::bios_idle();
        println!("dry-run: released = {}", emergency_release(&mut ec)?);
        return Ok(());
    }
    let mut ec = open_rw(c)?;
    let changed = emergency_release(&mut ec)?;
    let s = read_status(&mut ec)?;
    println!(
        "{}: 0x0F=0x{:02X} 0x14=0x{:02X}; the BIOS is in control",
        if changed { "released" } else { "nothing to release" },
        s.flags,
        s.target
    );
    Ok(())
}

// ----------------------------------------------------------------- curve --

fn curve(file: Option<&Path>) -> Result<()> {
    let limits = Limits { min: 22, max: 42 };
    let c = match file {
        Some(p) => {
            let text = std::fs::read_to_string(p).map_err(|e| Error::io(format!("reading {}", p.display()), e))?;
            println!("{}: valid", p.display());
            parse_curve(&text, limits)?
        }
        None => {
            let p = Params::default();
            println!(
                "built-in AUTO curve over the effective temperature max(CPU, GPU {:+.0} C);\n\
                 BIOS control below {:.0} C, hysteresis down to {:.0} C, maximum fans from {:.0} C:",
                p.gpu_bias_c, p.engage_c, p.disengage_c, p.hard_c
            );
            default_auto(limits)?
        }
    };
    for p in c.points() {
        println!("  {:>5.1} C -> level {} ({} RPM nominal)", p.temp_c, p.level, nominal_rpm(p.level));
    }
    Ok(())
}

// ------------------------------------------------------------------- run --

fn run(a: &RunArgs) -> Result<()> {
    check_machine(&a.common)?;

    if a.mode == ModeArg::Off {
        println!("mode OFF: handing control to the BIOS");
        return release(&a.common);
    }
    if !a.common.dry_run && daemon_running(&a.common) {
        if a.force {
            eprintln!("pavfan: WARNING: pavfand is running; --force given, the two will fight over the EC");
        } else {
            return Err(Error::ForeignControl(
                "pavfand is running; use `pavfan set ...`, or `systemctl stop pavfand` first (--force overrides)".into(),
            ));
        }
    }

    if a.common.dry_run {
        println!("DRY RUN: in-memory EC, hardware untouched");
        control_loop(MemEc::bios_idle(), a)
    } else {
        control_loop(open_rw(&a.common)?, a)
    }
}

fn build_mode(a: &RunArgs, limits: Limits) -> Result<Mode> {
    Ok(match a.mode {
        ModeArg::Off => Mode::Off,
        ModeArg::Auto => Mode::Auto,
        ModeArg::Fixed => match (a.level, a.percent) {
            (Some(l), None) => Mode::Fixed(l),
            (None, Some(p)) => Mode::Fixed(limits.from_percent(p)),
            _ => return Err(Error::Config("--mode fixed needs exactly one of --level / --percent".into())),
        },
        ModeArg::Manual => {
            let p = a.curve.as_deref().ok_or_else(|| Error::Config("--mode manual needs --curve FILE".into()))?;
            let text = std::fs::read_to_string(p).map_err(|e| Error::io(format!("reading {}", p.display()), e))?;
            Mode::Manual(parse_curve(&text, limits)?)
        }
    })
}

fn control_loop<E: EcIo>(ec: E, a: &RunArgs) -> Result<()> {
    let params = Params {
        gpu_bias_c: a.gpu_bias_c,
        engage_c: a.engage_c,
        disengage_c: a.disengage_c,
        hard_c: a.hard_c,
        ..Params::default()
    };
    // FIXED/MANUAL need the EC limits, which the runtime learns on construction: start in
    // AUTO (writes nothing) and switch before the first tick.
    let mut rt = Runtime::new(ec, a.force, Mode::Auto, params)?;
    let limits = rt.limits();
    rt.set_mode(build_mode(a, limits)?)?;
    if !rt.has_gpu_sensor() {
        eprintln!("pavfan: note: no NVIDIA GPU sensor available; deciding on CPU temperature only");
    }

    let stop = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM, signal_hook::consts::SIGHUP] {
        signal_hook::flag::register(sig, Arc::clone(&stop)).map_err(|e| Error::io("installing signal handler", e))?;
    }

    println!(
        "mode {} | EC limits {}..{} | tick {} ms | Ctrl-C hands the fans back to the BIOS",
        rt.mode().name(),
        limits.min,
        limits.max,
        a.interval_ms
    );
    println!(
        "effective = max(CPU, GPU {:+.0} C) | AUTO engages at {:.0} C, releases at {:.0} C | hard max at {:.0} C",
        params.gpu_bias_c, params.engage_c, params.disengage_c, params.hard_c
    );

    let start = Instant::now();
    let deadline = a.duration.map(|d| start + Duration::from_secs(d));
    let interval = Duration::from_millis(a.interval_ms);
    let mut last_key: Option<String> = None;

    let outcome = loop {
        if stop.load(Ordering::Relaxed) || deadline.is_some_and(|d| Instant::now() >= d) {
            break Ok(());
        }
        let tick_start = Instant::now();
        let r = match rt.tick() {
            Ok(r) => r,
            Err(e) => break Err(e),
        };

        let summary = match (&r.health, r.cpu_c) {
            (Health::SensorFailure(m), _) => format!("sensor failure -> BIOS ({m})"),
            (Health::Recovering(n), _) => format!("sensors recovering ({n}/{RECOVERY_TICKS})"),
            (Health::Ok, Some(cpu)) => format!(
                "cpu {cpu:.0}C gpu {} eff {:.0}C",
                r.gpu_c.map_or_else(|| "--".to_owned(), |g| format!("{g:.0}C")),
                r.effective_c.unwrap_or(cpu)
            ),
            (Health::Ok, None) => "no readings".to_owned(),
        };
        let action = match r.decision {
            Decision::Bios => format!("BIOS (level {})", r.level),
            Decision::Level(_) => format!(
                "level {} ({}%, {} RPM nominal){}{}",
                r.level,
                limits.to_percent(r.level),
                nominal_rpm(r.level),
                if r.hard_override { " [HARD OVERRIDE]" } else { "" },
                if r.reasserted { " [EC had reset: re-applied]" } else { "" }
            ),
        };
        // "level 38" / "BIOS": the part of the action that identifies a state change.
        let key = action.split(" (").next().unwrap_or_default().to_owned();
        let changed = last_key.as_deref() != Some(key.as_str());
        let note = r.note.as_deref().map(|n| format!(" [{n}]")).unwrap_or_default();
        if !a.quiet || changed || !note.is_empty() {
            println!("[{:>5}s] {summary} | {action}{note}", start.elapsed().as_secs());
        }
        last_key = Some(key);

        while tick_start.elapsed() < interval && !stop.load(Ordering::Relaxed) {
            thread::sleep(Duration::from_millis(50));
        }
    };

    let released = rt.release();
    if stop.load(Ordering::Relaxed) {
        println!("signal received: releasing to the BIOS");
    }
    match &released {
        Ok(()) => println!("fans handed back to the BIOS"),
        Err(e) => eprintln!("pavfan: CRITICAL: could not hand the fans back: {e}"),
    }
    outcome?;
    released
}

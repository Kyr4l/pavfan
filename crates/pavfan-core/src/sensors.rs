// SPDX-License-Identifier: GPL-3.0-or-later
//! Temperature sources. Failures are explicit errors: the caller falls back to
//! the BIOS rather than guessing.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

/// Readings outside this window are treated as sensor garbage.
const PLAUSIBLE_C: std::ops::RangeInclusive<f32> = 1.0..=125.0;

fn plausible(what: &str, t: f32) -> Result<f32> {
    if t.is_finite() && PLAUSIBLE_C.contains(&t) {
        Ok(t)
    } else {
        Err(Error::Sensor(format!("{what} reported an implausible {t} C")))
    }
}

/// CPU temperature: the hottest `temp*_input` of the `coretemp` hwmon device.
pub struct CpuTemp {
    inputs: Vec<PathBuf>,
}

impl CpuTemp {
    pub fn discover() -> Result<Self> {
        Self::discover_in(Path::new("/sys/class/hwmon"))
    }

    pub fn discover_in(hwmon_root: &Path) -> Result<Self> {
        let entries = fs::read_dir(hwmon_root).map_err(|e| Error::io("listing hwmon", e))?;
        for entry in entries.flatten() {
            let dir = entry.path();
            let name = fs::read_to_string(dir.join("name")).unwrap_or_default();
            if name.trim() != "coretemp" {
                continue;
            }
            let mut inputs: Vec<PathBuf> = fs::read_dir(&dir)
                .map_err(|e| Error::io("listing coretemp", e))?
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("temp") && n.ends_with("_input"))
                })
                .collect();
            inputs.sort();
            if inputs.is_empty() {
                continue;
            }
            return Ok(Self { inputs });
        }
        Err(Error::Sensor("no coretemp hwmon device found".into()))
    }

    pub fn read_c(&self) -> Result<f32> {
        let mut hottest: Option<f32> = None;
        for p in &self.inputs {
            let raw = fs::read_to_string(p)
                .map_err(|e| Error::io(format!("reading {}", p.display()), e))?;
            let milli: f32 = raw
                .trim()
                .parse()
                .map_err(|_| Error::Sensor(format!("unparsable value in {}", p.display())))?;
            let t = plausible("CPU", milli / 1000.0)?;
            hottest = Some(hottest.map_or(t, |h: f32| h.max(t)));
        }
        hottest.ok_or_else(|| Error::Sensor("no CPU temperature inputs".into()))
    }
}

/// NVIDIA dGPU temperature through `nvidia-smi`.
///
/// Querying a runtime-suspended dGPU would wake it (battery!), so we first ask
/// the kernel for its PCI power state and report `None` ("asleep, cool") when it
/// is not active.
pub struct GpuTemp {
    smi: PathBuf,
    runtime_status: PathBuf,
}

impl GpuTemp {
    /// `Ok(None)`: this machine has no usable NVIDIA GPU setup.
    pub fn discover() -> Result<Option<Self>> {
        let Some(runtime_status) = find_nvidia_runtime_status(Path::new("/sys/bus/pci/devices")) else {
            return Ok(None);
        };
        let smi = ["/usr/bin/nvidia-smi", "/bin/nvidia-smi", "/usr/local/bin/nvidia-smi"]
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file());
        Ok(smi.map(|smi| Self { smi, runtime_status }))
    }

    /// `Ok(None)` when the GPU is asleep.
    pub fn read_c(&self) -> Result<Option<f32>> {
        let status = fs::read_to_string(&self.runtime_status).unwrap_or_default();
        if status.trim() != "active" {
            return Ok(None);
        }
        let out = run_with_timeout(
            &self.smi,
            &["--query-gpu=temperature.gpu", "--format=csv,noheader,nounits"],
            Duration::from_secs(3),
        )?;
        let t: f32 = out
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .parse()
            .map_err(|_| Error::Sensor(format!("unparsable nvidia-smi output {out:?}")))?;
        plausible("GPU", t).map(Some)
    }
}

fn find_nvidia_runtime_status(pci_root: &Path) -> Option<PathBuf> {
    for entry in fs::read_dir(pci_root).ok()?.flatten() {
        let dir = entry.path();
        let vendor = fs::read_to_string(dir.join("vendor")).unwrap_or_default();
        let class = fs::read_to_string(dir.join("class")).unwrap_or_default();
        // 0x10de = NVIDIA, class 0x03xxxx = display controller
        if vendor.trim() == "0x10de" && class.trim().starts_with("0x03") {
            let p = dir.join("power/runtime_status");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

fn run_with_timeout(program: &Path, args: &[&str], timeout: Duration) -> Result<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Error::io(format!("spawning {}", program.display()), e))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(so) = child.stdout.take() {
                    let _ = so.take(4096).read_to_string(&mut out);
                }
                return if status.success() {
                    Ok(out)
                } else {
                    Err(Error::Sensor(format!("{} exited with {status}", program.display())))
                };
            }
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Sensor(format!("{} timed out", program.display())));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(Error::io("waiting for nvidia-smi", e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_implausible_values() {
        assert!(plausible("x", 0.0).is_err());
        assert!(plausible("x", -274.0).is_err());
        assert!(plausible("x", 200.0).is_err());
        assert!(plausible("x", f32::NAN).is_err());
        assert!(plausible("x", 55.0).is_ok());
    }

    #[test]
    fn fake_coretemp_takes_the_hottest_input() {
        let root = std::env::temp_dir().join(format!("pavfan-test-hwmon-{}", std::process::id()));
        let dev = root.join("hwmon9");
        fs::create_dir_all(&dev).expect("mkdir");
        fs::write(dev.join("name"), "coretemp\n").expect("name");
        fs::write(dev.join("temp1_input"), "61000\n").expect("t1");
        fs::write(dev.join("temp2_input"), "72500\n").expect("t2");
        let cpu = CpuTemp::discover_in(&root).expect("discover");
        let t = cpu.read_c().expect("read");
        assert!((t - 72.5).abs() < 0.01);
        fs::write(dev.join("temp2_input"), "garbage\n").expect("t2");
        assert!(cpu.read_c().is_err());
        let _ = fs::remove_dir_all(&root);
    }
}

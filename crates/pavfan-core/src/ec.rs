// SPDX-License-Identifier: GPL-3.0-or-later
//! Raw EC byte access.
//!
//! Access goes through the kernel's `ec_sys` debugfs file, NOT through raw port
//! I/O on 0x62/0x66: the kernel serialises our transactions with its own ACPI
//! EC driver, so we cannot corrupt a transaction in flight.
//!
//! `EcIo` is deliberately dumb (read/write one byte). All policy (which bytes may
//! be written, in which order) lives in `controller`, which is the only caller.

use std::fs::{self, File, OpenOptions};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Error, Result};

pub const EC_IO_PATH: &str = "/sys/kernel/debug/ec/ec0/io";
const WRITE_SUPPORT_PARAM: &str = "/sys/module/ec_sys/parameters/write_support";
const MODULE_DIR: &str = "/sys/module/ec_sys";

/// Single-byte access to the 256-byte EC address space.
pub trait EcIo {
    fn read(&mut self, offset: u8) -> Result<u8>;
    fn write(&mut self, offset: u8, value: u8) -> Result<()>;
}

/// Lend an EC to a controller (used by tests to inspect the EC afterwards).
impl<T: EcIo + ?Sized> EcIo for &mut T {
    fn read(&mut self, offset: u8) -> Result<u8> {
        (**self).read(offset)
    }

    fn write(&mut self, offset: u8, value: u8) -> Result<()> {
        (**self).write(offset, value)
    }
}

/// The real EC, through `ec_sys`.
pub struct DebugfsEc {
    file: File,
}

impl DebugfsEc {
    /// Open for reading and writing. Refuses unless `ec_sys` has write support,
    /// so we never mistake a read-only module for a working one.
    pub fn open() -> Result<Self> {
        if !ec_sys_write_enabled() {
            return Err(Error::EcAccess(
                "ec_sys is not loaded with write_support=1 \
                 (try: modprobe -r ec_sys && modprobe ec_sys write_support=1)"
                    .into(),
            ));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(EC_IO_PATH)
            .map_err(|e| Error::io(format!("opening {EC_IO_PATH} (are you root?)"), e))?;
        Ok(Self { file })
    }

    /// Open for reading only (status display); works with a read-only `ec_sys`.
    pub fn open_read_only() -> Result<Self> {
        let file = File::open(EC_IO_PATH)
            .map_err(|e| Error::io(format!("opening {EC_IO_PATH} (root + ec_sys needed)"), e))?;
        Ok(Self { file })
    }
}

impl EcIo for DebugfsEc {
    fn read(&mut self, offset: u8) -> Result<u8> {
        let mut b = [0u8; 1];
        self.file
            .read_exact_at(&mut b, u64::from(offset))
            .map_err(|e| Error::io(format!("EC read 0x{offset:02X}"), e))?;
        Ok(b[0])
    }

    fn write(&mut self, offset: u8, value: u8) -> Result<()> {
        self.file
            .write_all_at(&[value], u64::from(offset))
            .map_err(|e| Error::io(format!("EC write 0x{offset:02X}=0x{value:02X}"), e))
    }
}

pub fn ec_sys_loaded() -> bool {
    Path::new(MODULE_DIR).is_dir()
}

pub fn ec_sys_write_enabled() -> bool {
    fs::read_to_string(WRITE_SUPPORT_PARAM).map(|s| s.trim() == "Y").unwrap_or(false)
}

fn find_tool(name: &str) -> Result<PathBuf> {
    // Absolute paths only: never resolve a privileged helper through $PATH.
    for dir in ["/usr/sbin", "/sbin", "/usr/bin", "/bin"] {
        let p = Path::new(dir).join(name);
        if p.is_file() {
            return Ok(p);
        }
    }
    Err(Error::EcAccess(format!("`{name}` not found in /usr/sbin, /sbin, /usr/bin or /bin")))
}

fn run(tool: &str, args: &[&str]) -> Result<()> {
    let path = find_tool(tool)?;
    let status = Command::new(&path)
        .args(args)
        .status()
        .map_err(|e| Error::io(format!("running {}", path.display()), e))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::EcAccess(format!("`{tool} {}` failed ({status})", args.join(" "))))
    }
}

/// Make sure `ec_sys` is loaded (read-only is fine; used by read-only commands).
pub fn ensure_ec_sys_loaded() -> Result<()> {
    if ec_sys_loaded() {
        return Ok(());
    }
    run("modprobe", &["ec_sys"])?;
    if ec_sys_loaded() {
        Ok(())
    } else {
        Err(Error::EcAccess("ec_sys did not load".into()))
    }
}

/// Make sure `ec_sys` is loaded with `write_support=1`, reloading it if needed,
/// and verify the result. Reloading `ec_sys` is harmless: it is a debug interface.
pub fn ensure_ec_sys_writable() -> Result<()> {
    if ec_sys_write_enabled() {
        return Ok(());
    }
    if ec_sys_loaded() {
        run("rmmod", &["ec_sys"])?;
    }
    run("modprobe", &["ec_sys", "write_support=1"])?;
    if ec_sys_write_enabled() {
        Ok(())
    } else {
        Err(Error::EcAccess("ec_sys still has no write support after reloading it".into()))
    }
}

/// In-memory EC for tests and `--dry-run`. Records every write in order.
pub struct MemEc {
    pub mem: [u8; 256],
    pub writes: Vec<(u8, u8)>,
    pub fail_writes_after: Option<usize>,
}

impl MemEc {
    /// The state observed on the real machine under BIOS control at idle.
    pub fn bios_idle() -> Self {
        let mut mem = [0u8; 256];
        for (off, val) in [(0x0F, 0x01), (0x11, 22), (0x12, 42), (0x13, 22), (0x14, 0xFF)] {
            if let Some(slot) = mem.get_mut(off) {
                *slot = val;
            }
        }
        Self { mem, writes: Vec::new(), fail_writes_after: None }
    }

    pub fn peek(&self, offset: u8) -> u8 {
        self.mem.get(usize::from(offset)).copied().unwrap_or(0)
    }
}

impl EcIo for MemEc {
    fn read(&mut self, offset: u8) -> Result<u8> {
        Ok(self.peek(offset))
    }

    fn write(&mut self, offset: u8, value: u8) -> Result<()> {
        if let Some(limit) = self.fail_writes_after
            && self.writes.len() >= limit
        {
            return Err(Error::EcAccess("injected write failure".into()));
        }
        if let Some(slot) = self.mem.get_mut(usize::from(offset)) {
            *slot = value;
        }
        self.writes.push((offset, value));
        Ok(())
    }
}

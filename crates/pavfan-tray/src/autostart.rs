// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-user "start at login": an XDG autostart entry in `$XDG_CONFIG_HOME/autostart`.
//! Nothing system-wide; the user opts in from the tray menu.

use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

const FILE_NAME: &str = "pavfan-tray.desktop";

/// `~/.config/autostart/pavfan-tray.desktop`, honouring `$XDG_CONFIG_HOME`.
pub fn path() -> Option<PathBuf> {
    let config_home = env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").filter(|v| !v.is_empty()).map(|h| PathBuf::from(h).join(".config")))?;
    Some(config_home.join("autostart").join(FILE_NAME))
}

pub fn is_enabled() -> bool {
    path().is_some_and(|p| p.is_file())
}

/// The entry launches this very executable, so it also works for a build that is
/// not installed in `$PATH`.
fn entry() -> io::Result<String> {
    let exe = env::current_exe()?;
    Ok(format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=pavfan tray\n\
         Comment=Fan mode and temperatures for the HP Pavilion Gaming 15-dk1xxx\n\
         Exec={}\n\
         Icon=pavfan\n\
         Terminal=false\n\
         Categories=System;Monitor;\n\
         StartupNotify=false\n\
         X-GNOME-Autostart-enabled=true\n\
         X-KDE-autostart-after=panel\n",
        exe.display()
    ))
}

pub fn set_enabled(enabled: bool) -> io::Result<()> {
    let path = path().ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "neither $XDG_CONFIG_HOME nor $HOME is set"))?;
    if enabled {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        fs::write(&path, entry()?)
    } else {
        match fs::remove_file(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

// SPDX-License-Identifier: GPL-3.0-or-later
//! `pavfan-tray`: a StatusNotifierItem tray icon for the pavfan daemon.
//!
//! Works on any desktop with an SNI host (KDE Plasma natively; GNOME with the
//! AppIndicator extension; XFCE, LXQt, Waybar, ...). Polls `pavfand` over the
//! system bus and offers the mode switch in its menu. Runs as the desktop user;
//! changing the mode needs the D-Bus policy to allow it (wheel/sudo/pavfan group).
//! Starting at login is a per-user opt-in from the menu (XDG autostart entry).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod autostart;
mod icon;

use std::time::Duration;

use clap::Parser;
use ksni::menu::{CheckmarkItem, MenuItem, RadioGroup, RadioItem, StandardItem, SubMenu};
use ksni::{Category, ToolTip, TrayMethods};
use pavfan_ipc::{Daemon1Proxy, Status};
use tokio::sync::mpsc;

use crate::icon::IconState;

#[derive(Parser)]
#[command(name = "pavfan-tray", version, about = "Tray icon for the pavfan fan-control daemon")]
struct Cli {
    /// Talk to a daemon on the session bus instead of the system bus (development).
    #[arg(long)]
    session_bus: bool,
    /// Status refresh period in seconds.
    #[arg(long, default_value_t = 2, value_parser = clap::value_parser!(u64).range(1..=60))]
    poll_s: u64,
}

/// Menu actions are forwarded to the async main loop, which owns the D-Bus proxy.
#[derive(Debug)]
enum Cmd {
    SetMode(&'static str),
    SetFixed(u8),
    Quit,
}

const FIXED_CHOICES: [u8; 5] = [0, 25, 50, 75, 100];

struct PavfanTray {
    status: Option<Status>,
    error: Option<String>,
    tx: mpsc::UnboundedSender<Cmd>,
}

impl PavfanTray {
    fn state(&self) -> IconState {
        match &self.status {
            None => IconState::Error,
            Some(s) if s.health != "ok" => IconState::Error,
            Some(s) if s.mode == "off" => IconState::Off,
            Some(s) if s.hard_override => IconState::Hot,
            Some(s) if s.engaged => IconState::Active,
            Some(_) => IconState::Idle,
        }
    }

    fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    fn headline(&self) -> String {
        match &self.status {
            None => "pavfan: daemon not reachable".into(),
            Some(s) => {
                let mode = match s.mode.as_str() {
                    "fixed" => format!("FIXED {}%", s.fixed_percent),
                    m => m.to_uppercase(),
                };
                let fans = if s.engaged {
                    format!("fans {}%{}", s.percent, if s.hard_override { " (hard override)" } else { "" })
                } else {
                    "fans: BIOS".to_owned()
                };
                format!("pavfan {mode} · {fans}")
            }
        }
    }

    fn temps(&self) -> String {
        match &self.status {
            Some(s) if s.cpu_c >= 0.0 => {
                let gpu = if !s.gpu_present { String::new() } else if s.gpu_c < 0.0 { " · GPU asleep".into() } else { format!(" · GPU {:.0} °C", s.gpu_c) };
                format!("CPU {:.0} °C{gpu}", s.cpu_c)
            }
            Some(s) => format!("{}: {}", s.health, s.detail),
            None => self.error.clone().unwrap_or_else(|| "is pavfand running?".into()),
        }
    }
}

impl ksni::Tray for PavfanTray {
    /// SNI `ItemIsMenu`: a left click opens the menu (Plasma, GNOME AppIndicator).
    const MENU_ON_ACTIVATE: bool = true;

    fn id(&self) -> String {
        "pavfan".into()
    }

    fn title(&self) -> String {
        "pavfan".into()
    }

    fn category(&self) -> Category {
        Category::Hardware
    }

    fn icon_name(&self) -> String {
        self.state().name().into()
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        icon::pixmaps(self.state())
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip { title: self.headline(), description: self.temps(), ..Default::default() }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let connected = self.status.is_some();
        let mode = self.status.as_ref().map(|s| s.mode.clone()).unwrap_or_default();
        let has_curve = self.status.as_ref().is_some_and(|s| s.has_manual_curve);
        let fixed_pct = self.status.as_ref().map_or(50, |s| s.fixed_percent);

        let mode_index = match mode.as_str() {
            "off" => 0,
            "auto" => 1,
            "fixed" => 2,
            "manual" => 3,
            _ => 1,
        };
        let fixed_index = FIXED_CHOICES.iter().position(|&p| p == fixed_pct).unwrap_or(2);

        vec![
            StandardItem { label: self.headline(), enabled: false, ..Default::default() }.into(),
            StandardItem { label: self.temps(), enabled: false, ..Default::default() }.into(),
            MenuItem::Separator,
            RadioGroup {
                selected: mode_index,
                select: Box::new(|this: &mut Self, idx| {
                    let m = match idx {
                        0 => "off",
                        1 => "auto",
                        2 => "fixed",
                        _ => "manual",
                    };
                    this.send(Cmd::SetMode(m));
                }),
                options: vec![
                    RadioItem { label: "Off (BIOS controls the fans)".into(), enabled: connected, ..Default::default() },
                    RadioItem { label: "Auto".into(), enabled: connected, ..Default::default() },
                    RadioItem { label: format!("Fixed ({fixed_pct}%)"), enabled: connected, ..Default::default() },
                    RadioItem { label: "Manual curve".into(), enabled: connected && has_curve, ..Default::default() },
                ],
            }
            .into(),
            SubMenu {
                label: "Fixed speed".into(),
                enabled: connected,
                submenu: vec![
                    RadioGroup {
                        selected: fixed_index,
                        select: Box::new(|this: &mut Self, idx| {
                            if let Some(&p) = FIXED_CHOICES.get(idx) {
                                this.send(Cmd::SetFixed(p));
                            }
                        }),
                        options: FIXED_CHOICES
                            .iter()
                            .map(|p| RadioItem { label: format!("{p}%"), ..Default::default() })
                            .collect(),
                    }
                    .into(),
                ],
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            CheckmarkItem {
                label: "Start at login (this user)".into(),
                checked: autostart::is_enabled(),
                activate: Box::new(|this: &mut Self| {
                    if let Err(e) = autostart::set_enabled(!autostart::is_enabled()) {
                        this.error = Some(format!("autostart: {e}"));
                    }
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Exit tray".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|this: &mut Self| this.send(Cmd::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let (tx, mut rx) = mpsc::unbounded_channel();

    let handle = PavfanTray { status: None, error: None, tx }.spawn().await.map_err(|e| {
        format!("no StatusNotifier host found (is a system tray running?): {e}")
    })?;

    let conn = if cli.session_bus { zbus::Connection::session().await } else { zbus::Connection::system().await }?;
    let proxy = Daemon1Proxy::new(&conn).await?;

    let mut ticker = tokio::time::interval(Duration::from_secs(cli.poll_s));
    loop {
        tokio::select! {
            _ = ticker.tick() => refresh(&proxy, &handle).await,
            cmd = rx.recv() => {
                match cmd {
                    Some(Cmd::SetMode(m)) => {
                        if let Err(e) = proxy.set_mode(m).await {
                            handle.update(|t| t.error = Some(e.to_string())).await;
                        }
                        tokio::time::sleep(Duration::from_millis(1300)).await;
                        refresh(&proxy, &handle).await;
                    }
                    Some(Cmd::SetFixed(p)) => {
                        let r = match proxy.set_fixed_percent(p).await {
                            Ok(()) => proxy.set_mode("fixed").await,
                            Err(e) => Err(e),
                        };
                        if let Err(e) = r {
                            handle.update(|t| t.error = Some(e.to_string())).await;
                        }
                        tokio::time::sleep(Duration::from_millis(1300)).await;
                        refresh(&proxy, &handle).await;
                    }
                    Some(Cmd::Quit) | None => break,
                }
            }
        }
        if handle.is_closed() {
            break;
        }
    }
    handle.shutdown().await;
    Ok(())
}

async fn refresh(proxy: &Daemon1Proxy<'_>, handle: &ksni::Handle<PavfanTray>) {
    match proxy.get_status().await {
        Ok(s) => {
            handle
                .update(|t| {
                    t.status = Some(s);
                    t.error = None;
                })
                .await;
        }
        Err(e) => {
            handle
                .update(|t| {
                    t.status = None;
                    t.error = Some(e.to_string());
                })
                .await;
        }
    }
}

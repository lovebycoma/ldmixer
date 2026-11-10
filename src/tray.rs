//! The tray icon (StatusNotifierItem), shown while the daemon runs.
//!
//! Left click opens the mixer window; the menu offers the window, the main
//! monitor switches and Quit. Desktops without a StatusNotifier host (plain
//! GNOME without the AppIndicator extension, bare compositors) simply show
//! nothing; the daemon keeps working.

use std::sync::mpsc::Sender;

use ksni::blocking::TrayMethods;
use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::{MenuItem, Tray};

use crate::daemon::Msg;
use crate::engine::MonitorState;
use crate::protocol::monitor;

pub enum Action {
    OpenGui,
    ToggleMonitor(u8),
    Quit,
}

/// Everything the tray displays; the daemon updates the tray only when
/// this changes.
#[derive(Clone, Default, PartialEq)]
pub struct Shown {
    model: Option<String>,
    switches: Option<Switches>,
}

#[derive(Clone, Copy, Default, PartialEq)]
struct Switches {
    dim: bool,
    cut: bool,
    alt: bool,
    mono: bool,
    talkback: bool,
}

impl Shown {
    pub fn new(model: Option<String>, monitor: Option<&MonitorState>) -> Shown {
        let switches = monitor.map(|m| Switches {
            dim: m.dim,
            cut: m.cut,
            alt: m.alt,
            mono: m.mono,
            talkback: m.talkback,
        });
        Shown { model, switches }
    }
}

struct LdTray {
    tx: Sender<Msg>,
    shown: Shown,
}

impl LdTray {
    fn send(&self, a: Action) {
        let _ = self.tx.send(Msg::Tray(a));
    }

    fn toggle(&self, label: &str, sel: u8, on: bool) -> MenuItem<Self> {
        CheckmarkItem {
            label: label.into(),
            checked: on,
            enabled: self.shown.switches.is_some(),
            activate: Box::new(move |t: &mut Self| t.send(Action::ToggleMonitor(sel))),
            ..Default::default()
        }
        .into()
    }
}

impl Tray for LdTray {
    fn id(&self) -> String {
        "ldmixer".into()
    }

    fn title(&self) -> String {
        "lD Mixer".into()
    }

    fn icon_name(&self) -> String {
        crate::APP_ID.into()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: "lD Mixer".into(),
            description: match &self.shown.model {
                Some(m) => format!("{m} connected"),
                None => "No interface connected".into(),
            },
            ..Default::default()
        }
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(Action::OpenGui);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let m = self.shown.switches.unwrap_or_default();
        vec![
            StandardItem {
                label: "Open lD Mixer".into(),
                icon_name: crate::APP_ID.into(),
                activate: Box::new(|t: &mut Self| t.send(Action::OpenGui)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: match &self.shown.model {
                    Some(m) => format!("{m} connected"),
                    None => "No interface connected".into(),
                },
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            self.toggle("Dim", monitor::DIM, m.dim),
            self.toggle("Cut", monitor::CUT, m.cut),
            self.toggle("Alt Speakers", monitor::ALT, m.alt),
            self.toggle("Mono", monitor::MONO, m.mono),
            self.toggle("Talkback", monitor::TALKBACK, m.talkback),
            MenuItem::Separator,
            StandardItem {
                label: "Quit lD Mixer".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|t: &mut Self| t.send(Action::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

pub struct TrayHandle(ksni::blocking::Handle<LdTray>);

impl TrayHandle {
    pub fn update(&self, shown: &Shown) {
        self.0.update(|t| t.shown = shown.clone());
    }

    pub fn shutdown(self) {
        self.0.shutdown().wait();
    }
}

/// Show the tray icon; `None` only when the session bus is unreachable.
///
/// The daemon often starts before the tray host (autostart races the
/// panel), so a missing StatusNotifierWatcher is not an error: the icon
/// registers as soon as a watcher appears, and again if the panel restarts.
pub fn spawn(tx: Sender<Msg>) -> Option<TrayHandle> {
    let tray = LdTray { tx, shown: Shown::default() };
    match tray.assume_sni_available(true).spawn() {
        Ok(h) => Some(TrayHandle(h)),
        Err(e) => {
            log::warn!("no tray icon: {e}");
            None
        }
    }
}

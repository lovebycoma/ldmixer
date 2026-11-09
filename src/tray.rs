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

struct LdTray {
    tx: Sender<Msg>,
    model: Option<String>,
    monitor: Option<MonitorState>,
}

impl LdTray {
    fn send(&self, a: Action) {
        let _ = self.tx.send(Msg::Tray(a));
    }

    fn toggle(&self, label: &str, sel: u8, on: bool) -> MenuItem<Self> {
        CheckmarkItem {
            label: label.into(),
            checked: on,
            enabled: self.monitor.is_some(),
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
            description: match &self.model {
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
        let m = self.monitor.clone().unwrap_or_default();
        vec![
            StandardItem {
                label: "Open lD Mixer".into(),
                icon_name: crate::APP_ID.into(),
                activate: Box::new(|t: &mut Self| t.send(Action::OpenGui)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: match &self.model {
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
    pub fn update(&self, model: Option<String>, monitor: Option<MonitorState>) {
        self.0.update(|t| {
            t.model = model;
            t.monitor = monitor;
        });
    }

    pub fn shutdown(self) {
        self.0.shutdown().wait();
    }
}

/// Show the tray icon; `None` when the desktop has no tray.
pub fn spawn(tx: Sender<Msg>) -> Option<TrayHandle> {
    let tray = LdTray { tx, model: None, monitor: None };
    match tray.spawn() {
        Ok(h) => Some(TrayHandle(h)),
        Err(e) => {
            log::warn!("no tray icon: {e}");
            None
        }
    }
}

//! `ld-mixer daemon`: the background service.
//!
//! The daemon is the one process that owns the USB device. It keeps the
//! mixer state, restores it whenever the interface (re)connects, runs the
//! virtual PipeWire devices, shows the tray icon, and serves the GUI and the
//! CLI over a Unix socket (see [`crate::ipc`]). Closing the GUI leaves all of
//! this running; `ld-mixer stop` or the tray's Quit ends it.
//!
//! Threads: the device engine, one reader per client, the socket listener,
//! a signal waiter, a timer, and the tray all feed one channel that the main
//! thread drains, so all state lives on the main thread without locks.

use std::collections::HashMap;
use std::io::BufReader;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::config::{self, Settings};
use crate::engine::{self, Command, Desired, EngineHandle, Event, MonitorState, SystemState};
use crate::ipc::{self, DeviceSummary, MonitorCmd, Notice, Request};
use crate::mixer::MixerState;
use crate::protocol::{self, monitor, Profile};
use crate::tray;
use crate::vdev;

/// Everything the main thread reacts to.
pub enum Msg {
    Engine(Event),
    Connected(u64, UnixStream),
    Request(u64, Request),
    Gone(u64),
    Tray(tray::Action),
    Tick,
    Quit,
}

struct Client {
    stream: UnixStream,
    gui: bool,
}

struct Daemon {
    profile: &'static Profile,
    mock: bool,
    engine: EngineHandle,
    state: MixerState,
    settings: Settings,
    /// No saved state yet: read the device's mix before writing anything.
    first_run: bool,
    awaiting_readback: bool,
    dirty: bool,
    last_change: std::time::Instant,
    device: Option<DeviceSummary>,
    device_error: Option<String>,
    monitor: Option<MonitorState>,
    system: Option<SystemState>,
    vdevs: vdev::Manager,
    clients: HashMap<u64, Client>,
    tray: Option<tray::TrayHandle>,
}

/// Run the daemon in the foreground until stopped.
pub fn run(mock: bool) -> Result<()> {
    let path = ipc::socket_path();
    if ipc::daemon_running() {
        bail!("the lD Mixer daemon is already running");
    }
    let _ = std::fs::remove_file(&path); // stale socket from a crash
    let listener = UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;

    // Block SIGINT/SIGTERM in every thread; one thread waits for them so
    // the main loop can shut down cleanly (and stop the virtual devices).
    let sigset = unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        libc::sigaddset(&mut set, libc::SIGINT);
        libc::sigaddset(&mut set, libc::SIGTERM);
        libc::sigaddset(&mut set, libc::SIGHUP);
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        set
    };

    let (tx, rx) = mpsc::channel::<Msg>();

    {
        let tx = tx.clone();
        thread::Builder::new().name("signals".into()).spawn(move || {
            let mut sig = 0;
            unsafe { libc::sigwait(&sigset, &mut sig) };
            let _ = tx.send(Msg::Quit);
        })?;
    }
    {
        let tx = tx.clone();
        thread::Builder::new().name("timer".into()).spawn(move || loop {
            thread::sleep(Duration::from_millis(500));
            if tx.send(Msg::Tick).is_err() {
                return;
            }
        })?;
    }
    spawn_listener(listener, tx.clone())?;

    let (etx, erx) = async_channel::unbounded();
    let engine = engine::spawn(mock, etx);
    {
        let tx = tx.clone();
        thread::Builder::new().name("engine-events".into()).spawn(move || {
            while let Ok(ev) = erx.recv_blocking() {
                if tx.send(Msg::Engine(ev)).is_err() {
                    return;
                }
            }
        })?;
    }

    if let Err(e) = vdev::check_tools() {
        log::warn!("virtual devices unavailable: {e}");
    }

    let profile = &protocol::PROFILES[0];
    let saved = config::load_state(profile);
    let mut d = Daemon {
        profile,
        mock,
        engine,
        first_run: saved.is_none(),
        state: saved.unwrap_or_else(|| MixerState::default_for(profile)),
        settings: config::load_settings(),
        awaiting_readback: false,
        dirty: false,
        last_change: std::time::Instant::now(),
        device: None,
        device_error: None,
        monitor: None,
        system: None,
        vdevs: vdev::Manager::default(),
        clients: HashMap::new(),
        tray: tray::spawn(tx.clone()),
    };
    log::info!("lD Mixer daemon listening on {}", path.display());

    while let Ok(msg) = rx.recv() {
        if !d.handle(msg) {
            break;
        }
    }

    d.broadcast(&Notice::Quitting, false);
    d.save(true);
    d.vdevs.clear();
    if let Some(t) = d.tray.take() {
        t.shutdown();
    }
    let _ = std::fs::remove_file(&path);
    log::info!("lD Mixer daemon stopped");
    Ok(())
}

fn spawn_listener(listener: UnixListener, tx: Sender<Msg>) -> Result<()> {
    thread::Builder::new().name("listener".into()).spawn(move || {
        let mut next_id = 1u64;
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let id = next_id;
            next_id += 1;
            let Ok(reader) = stream.try_clone() else { continue };
            if tx.send(Msg::Connected(id, stream)).is_err() {
                return;
            }
            let tx = tx.clone();
            let _ = thread::Builder::new().name(format!("client-{id}")).spawn(move || {
                let mut reader = BufReader::new(reader);
                loop {
                    match ipc::read_line::<Request>(&mut reader) {
                        Ok(Some(req)) => {
                            if tx.send(Msg::Request(id, req)).is_err() {
                                return;
                            }
                        }
                        Ok(None) | Err(_) => break,
                    }
                }
                let _ = tx.send(Msg::Gone(id));
            });
        }
    })?;
    Ok(())
}

impl Daemon {
    /// Returns false when the daemon should exit.
    fn handle(&mut self, msg: Msg) -> bool {
        match msg {
            Msg::Quit => return false,
            Msg::Tick => {
                self.save(false);
                if self.vdevs.maintain() {
                    self.broadcast_vdevs();
                }
            }
            Msg::Connected(id, stream) => {
                self.clients.insert(id, Client { stream, gui: false });
            }
            Msg::Gone(id) => {
                self.clients.remove(&id);
            }
            Msg::Request(id, req) => return self.request(id, req),
            Msg::Engine(ev) => self.engine_event(ev),
            Msg::Tray(action) => return self.tray_action(action),
        }
        true
    }

    fn send_to(&mut self, id: u64, n: &Notice) {
        let failed = match self.clients.get_mut(&id) {
            Some(c) => ipc::write_line(&mut c.stream, n).is_err(),
            None => false,
        };
        if failed {
            self.clients.remove(&id);
        }
    }

    /// Send to GUI clients (or every client when `all`).
    fn broadcast(&mut self, n: &Notice, all: bool) {
        self.broadcast_except(n, all, None);
    }

    fn broadcast_except(&mut self, n: &Notice, all: bool, except: Option<u64>) {
        let line = match serde_json::to_vec(n) {
            Ok(mut l) => {
                l.push(b'\n');
                l
            }
            Err(_) => return,
        };
        let mut dead = Vec::new();
        for (id, c) in self.clients.iter_mut() {
            if (all || c.gui) && Some(*id) != except {
                use std::io::Write;
                if c.stream.write_all(&line).is_err() {
                    dead.push(*id);
                }
            }
        }
        for id in dead {
            self.clients.remove(&id);
        }
    }

    fn device_notice(&self) -> Notice {
        Notice::Device(self.device.clone(), self.device_error.clone())
    }

    fn request(&mut self, id: u64, req: Request) -> bool {
        match req {
            Request::Hello { gui } => {
                if let Some(c) = self.clients.get_mut(&id) {
                    c.gui = gui;
                }
                self.send_to(id, &Notice::Welcome { version: env!("CARGO_PKG_VERSION").into() });
                self.send_to(id, &self.device_notice());
                if gui {
                    self.send_to(id, &Notice::State(Box::new(self.state.clone())));
                    self.send_to(id, &Notice::Settings(self.settings.clone()));
                    if let Some(m) = self.monitor.clone() {
                        self.send_to(id, &Notice::Monitor(m));
                    }
                    if let Some(s) = self.system.clone() {
                        self.send_to(id, &Notice::System(s));
                    }
                    let st = self.vdevs.status();
                    self.send_to(id, &Notice::VirtualDevices(st));
                }
            }
            Request::SetState(s) => {
                self.state = s.sanitize(self.profile);
                self.first_run = false;
                self.dirty = true;
                self.last_change = std::time::Instant::now();
                self.apply();
                self.reconcile_vdevs();
                let n = Notice::State(Box::new(self.state.clone()));
                self.broadcast_except(&n, false, Some(id));
            }
            Request::SetSettings(s) => {
                self.settings = s;
                if let Err(e) = config::save_settings(&self.settings) {
                    log::warn!("saving settings failed: {e}");
                }
                self.reconcile_vdevs();
                let n = Notice::Settings(self.settings.clone());
                self.broadcast_except(&n, false, Some(id));
            }
            Request::Monitor(cmd) => self.monitor_cmd(cmd),
            Request::Optical { output, mode } => self.engine.send(Command::Optical { output, mode }),
            Request::ReadBack => {
                if self.device.is_some() {
                    self.awaiting_readback = true;
                    self.engine.send(Command::ReadBack);
                }
            }
            Request::Raw(op) => self.engine.send(Command::Raw { client: id, op }),
            Request::Shutdown => return false,
        }
        true
    }

    fn monitor_cmd(&mut self, cmd: MonitorCmd) {
        self.engine.send(match cmd {
            MonitorCmd::Bool(sel, on) => Command::MonitorBool(sel, on),
            MonitorCmd::Level(sel, db) => Command::MonitorLevel(sel, db),
            MonitorCmd::Byte(sel, v) => Command::MonitorByte(sel, v),
        });
    }

    fn engine_event(&mut self, ev: Event) {
        match ev {
            Event::Connected(info) => {
                self.device = Some(DeviceSummary::from(&info));
                self.device_error = None;
                let n = self.device_notice();
                self.broadcast(&n, false);
                if self.first_run {
                    // Never overwrite an unknown device state blindly.
                    self.awaiting_readback = true;
                    self.engine.send(Command::ReadBack);
                } else {
                    self.apply();
                }
                self.reconcile_vdevs();
                self.update_tray();
            }
            Event::Disconnected(reason) => {
                self.device = None;
                self.device_error = Some(reason);
                self.monitor = None;
                let n = self.device_notice();
                self.broadcast(&n, false);
                self.reconcile_vdevs();
                self.update_tray();
            }
            Event::ReadBack { matrix, routing } => {
                let was_first = self.first_run;
                self.awaiting_readback = false;
                self.first_run = false;
                let toast = if self.state.apply_readback(self.profile, matrix.as_deref(), &routing) {
                    "Loaded the current mix from the device"
                } else if was_first {
                    "Could not read the device's mix; applied lD Mixer defaults"
                } else {
                    "The device did not report its mix"
                };
                self.dirty = true;
                self.last_change = std::time::Instant::now();
                self.apply();
                self.broadcast(&Notice::State(Box::new(self.state.clone())), false);
                self.broadcast(&Notice::Toast(toast.into()), false);
            }
            Event::Meters(m) => self.broadcast(&Notice::Meters(m), false),
            Event::Monitor(m) => {
                self.monitor = Some(m.clone());
                self.broadcast(&Notice::Monitor(m), false);
                self.update_tray();
            }
            Event::System(s) => {
                self.system = Some(s.clone());
                self.broadcast(&Notice::System(s), false);
            }
            Event::RawReply { client, result } => self.send_to(client, &Notice::RawReply(result)),
            Event::Error(e) => self.broadcast(&Notice::Toast(format!("Device error: {e}")), false),
        }
    }

    /// Push the mix to the device (unless waiting for a readback).
    fn apply(&mut self) {
        if self.device.is_none() || self.awaiting_readback {
            return;
        }
        self.engine.send(Command::Apply(Desired {
            matrix: self.state.matrix(),
            routing: self.state.routing(self.profile),
            phases: self.state.phases(self.profile),
        }));
    }

    /// Write the state file, at most once a second after the last change
    /// (a fader drag changes it continuously), or right away when `now`.
    fn save(&mut self, now: bool) {
        let settled = now || self.last_change.elapsed() >= Duration::from_secs(1);
        if self.dirty && !self.first_run && settled {
            self.dirty = false;
            if let Err(e) = config::save_state(self.profile, &self.state) {
                log::warn!("saving state failed: {e}");
            }
        }
    }

    /// The virtual devices wanted right now.
    fn wanted_vdevs(&self) -> Vec<vdev::Spec> {
        // The mock device has no PipeWire nodes to attach to.
        if self.device.is_none() || self.mock {
            return Vec::new();
        }
        let mut v = Vec::new();
        if self.settings.loopback_source {
            if let Some(ch) = self.profile.loopback_capture {
                v.push(vdev::Spec::loopback(self.profile.name, ch));
            }
        }
        v.extend(self.state.virtual_devices(self.profile));
        v
    }

    fn reconcile_vdevs(&mut self) {
        let wanted = self.wanted_vdevs();
        if self.vdevs.reconcile(&wanted) {
            self.broadcast_vdevs();
        }
    }

    fn broadcast_vdevs(&mut self) {
        let st = self.vdevs.status();
        self.broadcast(&Notice::VirtualDevices(st), false);
    }

    fn update_tray(&mut self) {
        if let Some(t) = &self.tray {
            t.update(self.device.as_ref().map(|d| d.model.clone()), self.monitor.clone());
        }
    }

    fn tray_action(&mut self, action: tray::Action) -> bool {
        match action {
            tray::Action::OpenGui => {
                if let Err(e) = open_gui(self.mock) {
                    log::warn!("cannot open the GUI: {e}");
                }
            }
            tray::Action::ToggleMonitor(sel) => {
                let current = self.monitor.as_ref().map(|m| match sel {
                    monitor::DIM => m.dim,
                    monitor::CUT => m.cut,
                    monitor::ALT => m.alt,
                    monitor::MONO => m.mono,
                    monitor::TALKBACK => m.talkback,
                    _ => false,
                });
                if let Some(on) = current {
                    self.engine.send(Command::MonitorBool(sel, !on));
                }
            }
            tray::Action::Quit => return false,
        }
        true
    }
}

/// Start `ld-mixer gui` as a separate process.
fn open_gui(mock: bool) -> Result<()> {
    let exe = std::env::current_exe()?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("gui");
    if mock {
        cmd.arg("--mock");
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            crate::reset_signal_mask();
            Ok(())
        });
    }
    let mut child = cmd.spawn()?;
    // Reap it in the background so it does not linger as a zombie.
    thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

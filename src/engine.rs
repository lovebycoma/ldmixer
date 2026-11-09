//! The device engine: a background thread that owns the USB connection.
//!
//! The UI never touches USB. It sends [`Command`]s; the engine writes only
//! what changed, polls meters and the monitor section, reconnects when the
//! device comes back, and reports [`Event`]s.
//!
//! Safety rule learned by BiD: control traffic while the stream changes
//! sample rate can wedge the firmware's control plane. The engine watches
//! the momentary rate in `/proc/asound` (free, no USB) and goes quiet for a
//! few seconds whenever it changes.

use std::collections::HashMap;
use std::fs;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::device::{self, ControlIo, DeviceInfo};
use crate::protocol::{self, entity, monitor};

const TICK: Duration = Duration::from_millis(33);
const MONITOR_EVERY: Duration = Duration::from_millis(150);
/// PipeWire's quantum rarely changes; reading it spawns `pw-metadata`.
const QUANTUM_EVERY: Duration = Duration::from_secs(5);
const SYSTEM_EVERY: Duration = Duration::from_millis(1000);
const RECONNECT_EVERY: Duration = Duration::from_millis(2000);
const RATE_CHANGE_QUIET: Duration = Duration::from_secs(3);

/// The hardware state lD Mixer wants. Sent whole; the engine diffs it.
#[derive(Clone, Debug, Default)]
pub struct Desired {
    pub matrix: Vec<i16>,
    pub routing: Vec<(u8, u8)>,
    pub phases: Vec<(u8, bool)>,
}

/// The monitor section, as read from the device.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MonitorState {
    pub volume: Option<f32>,
    pub dim: bool,
    pub dim_level: Option<f32>,
    pub cut: bool,
    pub alt: bool,
    pub talkback: bool,
    pub talkback_source: u8,
    pub mono: bool,
    pub mono_mode: u8,
    pub polarity: bool,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SystemState {
    pub sample_rate: Option<u32>,
    /// True when an audio stream is running (rate read from /proc).
    pub streaming: bool,
    /// 0 = ADAT, 1 = S/PDIF.
    pub optical_in: Option<i32>,
    pub optical_out: Option<i32>,
    /// PipeWire's graph quantum (buffer size in samples), read from its
    /// `settings` metadata; lD Mixer never changes it.
    #[serde(default)]
    pub quantum: Option<u32>,
    /// True when the quantum is pinned by `clock.force-quantum`.
    #[serde(default)]
    pub quantum_forced: bool,
}

pub enum Command {
    Apply(Desired),
    MonitorBool(u8, bool),
    MonitorLevel(u8, Option<f32>),
    MonitorByte(u8, u8),
    Optical { output: bool, mode: i32 },
    /// Read matrix and routing back from the device.
    ReadBack,
    /// A raw request on behalf of a client; answered with `Event::RawReply`.
    Raw { client: u64, op: crate::ipc::RawOp },
    Quit,
}

pub enum Event {
    Connected(DeviceInfo),
    Disconnected(String),
    /// Input levels per mixer node, dBFS (`None` = silence).
    Meters(Vec<Option<f32>>),
    Monitor(MonitorState),
    System(SystemState),
    ReadBack { matrix: Option<Vec<i16>>, routing: Vec<Option<u8>> },
    RawReply { client: u64, result: Result<Vec<u8>, String> },
    Error(String),
}

pub struct EngineHandle {
    tx: Sender<Command>,
    thread: Option<thread::JoinHandle<()>>,
}

impl EngineHandle {
    pub fn send(&self, cmd: Command) {
        let _ = self.tx.send(cmd);
    }
}

impl Drop for EngineHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

pub fn spawn(mock: bool, events: async_channel::Sender<Event>) -> EngineHandle {
    let (tx, rx) = mpsc::channel();
    let thread = thread::Builder::new()
        .name("ld-mixer-engine".into())
        .spawn(move || Engine::new(mock, events).run(rx))
        .expect("spawn engine thread");
    EngineHandle { tx, thread: Some(thread) }
}

struct Engine {
    mock: bool,
    events: async_channel::Sender<Event>,
    dev: Option<Box<dyn ControlIo>>,
    last_error: Option<String>,
    // What we believe the device holds, so only changes get written.
    cells: HashMap<usize, i16>,
    routes: HashMap<u8, u8>,
    phases: HashMap<u8, bool>,
    pending: Option<Desired>,
    monitor: Option<MonitorState>,
    system: Option<SystemState>,
    last_monitor: Instant,
    last_system: Instant,
    last_attempt: Option<Instant>,
    proc_rate: Option<String>,
    quiet_until: Option<Instant>,
    quantum: ((Option<u32>, bool), Option<Instant>),
}

impl Engine {
    fn new(mock: bool, events: async_channel::Sender<Event>) -> Engine {
        let past = Instant::now() - Duration::from_secs(60);
        Engine {
            mock,
            events,
            dev: None,
            last_error: None,
            cells: HashMap::new(),
            routes: HashMap::new(),
            phases: HashMap::new(),
            pending: None,
            monitor: None,
            system: None,
            last_monitor: past,
            last_system: past,
            last_attempt: None,
            proc_rate: None,
            quiet_until: None,
            quantum: ((None, false), None),
        }
    }

    fn emit(&self, ev: Event) {
        let _ = self.events.send_blocking(ev);
    }

    fn run(mut self, rx: Receiver<Command>) {
        loop {
            // Wait for a command or the next tick, then drain the queue,
            // keeping only the newest Apply (fader drags send many).
            match rx.recv_timeout(TICK) {
                Ok(cmd) => {
                    if !self.handle(cmd) {
                        return;
                    }
                    while let Ok(cmd) = rx.try_recv() {
                        if !self.handle(cmd) {
                            return;
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
            self.tick();
        }
    }

    /// Returns false on Quit.
    fn handle(&mut self, cmd: Command) -> bool {
        match cmd {
            Command::Quit => return false,
            Command::Apply(d) => self.pending = Some(d),
            Command::Raw { client, op: _ } if self.dev.is_none() => {
                self.emit(Event::RawReply { client, result: Err("no device connected".into()) });
            }
            other => {
                if self.dev.is_none() || self.is_quiet() {
                    return true;
                }
                let r = self.run_command(other);
                self.check(r);
            }
        }
        true
    }

    fn run_command(&mut self, cmd: Command) -> anyhow::Result<()> {
        let dev = self.dev.as_ref().expect("connected");
        match cmd {
            Command::MonitorBool(sel, on) => {
                dev.set_u8(entity::MONITOR, sel, 0, on as u8)?;
                self.monitor = None; // re-read and re-report on the next poll
            }
            Command::MonitorLevel(sel, db) => {
                dev.set_i16(entity::MONITOR, sel, 0, protocol::db_to_raw(db))?;
                self.monitor = None;
            }
            Command::MonitorByte(sel, v) => {
                dev.set_u8(entity::MONITOR, sel, 0, v)?;
                self.monitor = None;
            }
            Command::Optical { output, mode } => {
                dev.set_optical_mode(output, mode)?;
                self.system = None;
                self.last_system = Instant::now() - SYSTEM_EVERY;
            }
            Command::ReadBack => {
                let p = dev.profile();
                let matrix: Option<Vec<i16>> = (0..p.nodes.len() * protocol::MIXER_STRIDE)
                    .map(|c| dev.get_mixer_cell(c).ok())
                    .collect();
                let routing = (0..p.routing_outputs).map(|o| dev.get_route(o).ok()).collect();
                self.emit(Event::ReadBack { matrix, routing });
            }
            Command::Raw { client, op } => {
                use crate::ipc::RawOp;
                let r = match op {
                    RawOp::Get { ent, sel, ch, len } => dev.get(ent, sel, ch, len),
                    RawOp::Set { ent, sel, ch, data } => dev.set(ent, sel, ch, &data).map(|_| Vec::new()),
                    RawOp::GetMem { ent, offset, len } => dev.get_mem(ent, offset, len),
                };
                let gone = r.as_ref().err().is_some_and(device::is_gone);
                self.emit(Event::RawReply { client, result: r.map_err(|e| e.to_string()) });
                if gone {
                    self.disconnect("device lost".into());
                }
                // Writes may have changed what we think the device holds.
                self.cells.clear();
                self.routes.clear();
                self.phases.clear();
                self.monitor = None;
            }
            Command::Apply(_) | Command::Quit => unreachable!(),
        }
        Ok(())
    }

    /// Turn a failed transfer into a disconnect when the device is gone.
    fn check(&mut self, r: anyhow::Result<()>) {
        if let Err(e) = r {
            if device::is_gone(&e) {
                self.disconnect(format!("device lost: {e}"));
            } else {
                self.emit(Event::Error(e.to_string()));
            }
        }
    }

    fn disconnect(&mut self, reason: String) {
        self.dev = None;
        self.cells.clear();
        self.routes.clear();
        self.phases.clear();
        self.monitor = None;
        self.system = None;
        self.last_error = Some(reason.clone());
        self.emit(Event::Disconnected(reason));
    }

    fn is_quiet(&self) -> bool {
        self.quiet_until.is_some_and(|t| Instant::now() < t)
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if self.dev.is_none() {
            if self.last_attempt.is_none_or(|t| now - t >= RECONNECT_EVERY) {
                self.last_attempt = Some(now);
                self.connect();
            }
            if self.dev.is_none() {
                return;
            }
        }
        self.watch_rate();
        if self.is_quiet() {
            return;
        }
        if let Some(d) = self.pending.take() {
            let r = self.apply(&d);
            self.check(r);
        }
        if self.dev.is_none() {
            return;
        }
        let r = self.poll_meters();
        self.check(r);
        if now - self.last_monitor >= MONITOR_EVERY {
            self.last_monitor = now;
            let r = self.poll_monitor();
            self.check(r);
        }
        if now - self.last_system >= SYSTEM_EVERY {
            self.last_system = now;
            self.poll_system();
        }
    }

    fn connect(&mut self) {
        match device::open(self.mock) {
            Ok(dev) => {
                let info = dev.info().clone();
                log::info!("connected to {} on interface {}", info.profile.name, info.interface);
                self.dev = Some(dev);
                self.last_error = None;
                self.last_monitor = Instant::now() - MONITOR_EVERY;
                self.last_system = Instant::now() - SYSTEM_EVERY;
                self.emit(Event::Connected(info));
            }
            Err(e) => {
                let msg = e.to_string();
                if self.last_error.as_deref() != Some(&msg) {
                    self.last_error = Some(msg.clone());
                    self.emit(Event::Disconnected(msg));
                }
            }
        }
    }

    fn apply(&mut self, d: &Desired) -> anyhow::Result<()> {
        let dev = self.dev.as_ref().expect("connected");
        for (cell, &raw) in d.matrix.iter().enumerate() {
            if self.cells.get(&cell) != Some(&raw) {
                dev.set_mixer_cell(cell, raw)?;
                self.cells.insert(cell, raw);
            }
        }
        for &(out, code) in &d.routing {
            if self.routes.get(&out) != Some(&code) {
                dev.set_route(out, code)?;
                self.routes.insert(out, code);
            }
        }
        for &(input, on) in &d.phases {
            if self.phases.get(&input) != Some(&on) {
                dev.set_phase(input, on)?;
                self.phases.insert(input, on);
            }
        }
        Ok(())
    }

    fn poll_meters(&mut self) -> anyhow::Result<()> {
        let dev = self.dev.as_ref().expect("connected");
        let n = dev.profile().nodes.len();
        match dev.read_input_meters(n) {
            Ok(levels) => self.emit(Event::Meters(levels)),
            Err(e) if device::is_gone(&e) => return Err(e),
            Err(_) => {} // meters are optional; ignore stalls
        }
        Ok(())
    }

    fn poll_monitor(&mut self) -> anyhow::Result<()> {
        let dev = self.dev.as_ref().expect("connected");
        let b = |sel| dev.get_u8(entity::MONITOR, sel, 0);
        let l = |sel| dev.get_i16(entity::MONITOR, sel, 0).map(protocol::raw_to_db);
        let state = MonitorState {
            volume: l(monitor::VOLUME)?,
            dim: b(monitor::DIM)? != 0,
            dim_level: l(monitor::DIM_LEVEL).unwrap_or(None),
            cut: b(monitor::CUT)? != 0,
            alt: b(monitor::ALT)? != 0,
            talkback: b(monitor::TALKBACK)? != 0,
            talkback_source: b(monitor::TALKBACK_SOURCE).unwrap_or(0x10),
            mono: b(monitor::MONO)? != 0,
            mono_mode: b(monitor::MONO_MODE).unwrap_or(0),
            polarity: b(monitor::POLARITY)? != 0,
        };
        if self.monitor.as_ref() != Some(&state) {
            self.monitor = Some(state.clone());
            self.emit(Event::Monitor(state));
        }
        Ok(())
    }

    fn poll_system(&mut self) {
        let dev = self.dev.as_ref().expect("connected");
        let (proc_rate, streaming) = proc_stream_rate(dev.info().mock);
        let state = SystemState {
            sample_rate: proc_rate.or_else(|| dev.sample_rate().ok().map(|r| r as u32)),
            streaming,
            optical_in: dev.optical_mode(false).ok(),
            optical_out: dev.optical_mode(true).ok(),
            quantum: None,
            quantum_forced: false,
        };
        let mut state = state;
        if self.quantum.1.is_none_or(|t| t.elapsed() >= QUANTUM_EVERY) {
            self.quantum = (pipewire_quantum(), Some(Instant::now()));
        }
        (state.quantum, state.quantum_forced) = self.quantum.0;
        if self.system.as_ref() != Some(&state) {
            self.system = Some(state.clone());
            self.emit(Event::System(state));
        }
    }

    /// Go quiet when the stream's momentary rate changes.
    fn watch_rate(&mut self) {
        let mock = self.dev.as_ref().is_some_and(|d| d.info().mock);
        let now = read_stream_status(mock);
        if self.proc_rate.is_some() && now != self.proc_rate {
            log::info!("stream rate changed; pausing control traffic");
            self.quiet_until = Some(Instant::now() + RATE_CHANGE_QUIET);
            self.last_system = Instant::now(); // re-read after the quiet period
        }
        self.proc_rate = now;
    }
}

/// The iD's `/proc/asound/cardN` directory.
fn proc_card() -> Option<std::path::PathBuf> {
    for entry in fs::read_dir("/proc/asound").ok()?.flatten() {
        let p = entry.path();
        if let Ok(id) = fs::read_to_string(p.join("usbid")) {
            if id.trim().starts_with("2708:") {
                return Some(p);
            }
        }
    }
    None
}

/// Raw "Status/Momentary freq" lines of the playback stream, used only to
/// detect changes.
fn read_stream_status(mock: bool) -> Option<String> {
    if mock {
        return None;
    }
    let text = fs::read_to_string(proc_card()?.join("stream0")).ok()?;
    Some(
        text.lines()
            .filter(|l| l.contains("Status:") || l.contains("Momentary freq"))
            .collect::<Vec<_>>()
            .join("|"),
    )
}

/// Current stream rate from procfs, and whether a stream is running.
fn proc_stream_rate(mock: bool) -> (Option<u32>, bool) {
    if mock {
        return (Some(48000), true);
    }
    let Some(text) = proc_card().and_then(|p| fs::read_to_string(p.join("stream0")).ok()) else {
        return (None, false);
    };
    let running = text.contains("Status: Running");
    let rate = text
        .lines()
        .find(|l| l.contains("Momentary freq"))
        .and_then(|l| l.split('=').nth(1))
        .and_then(|v| v.split_whitespace().next())
        .and_then(|v| v.parse().ok());
    (if running { rate } else { None }, running)
}

/// The graph quantum from `pw-metadata -n settings`: the forced value when
/// `clock.force-quantum` is set, otherwise the default `clock.quantum`.
fn pipewire_quantum() -> (Option<u32>, bool) {
    let Ok(out) = std::process::Command::new("pw-metadata")
        .args(["-n", "settings"])
        .stderr(std::process::Stdio::null())
        .output()
    else {
        return (None, false);
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let value = |key: &str| -> Option<u32> {
        text.lines()
            .find(|l| l.contains(&format!("key:'{key}'")))
            .and_then(|l| l.split("value:'").nth(1))
            .and_then(|v| v.split('\'').next())
            .and_then(|v| v.parse().ok())
    };
    match value("clock.force-quantum") {
        Some(q) if q > 0 => (Some(q), true),
        _ => (value("clock.quantum"), false),
    }
}

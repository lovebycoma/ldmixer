//! The daemon's local socket protocol.
//!
//! One JSON message per line over a Unix socket in `$XDG_RUNTIME_DIR`
//! (mode 0600, so only the user can connect). The GUI and the CLI are both
//! clients; the daemon is the only process that talks to the USB device.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::Settings;
use crate::device::{ControlIo, DeviceInfo};
use crate::engine::{MonitorState, SystemState};
use crate::mixer::MixerState;
use crate::protocol;
use crate::vdev;

pub fn socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).filter(|p| p.is_absolute()) {
        Some(dir) => dir.join("ldmixer.sock"),
        None => std::env::temp_dir().join(format!("ldmixer-{}.sock", unsafe { libc::getuid() })),
    }
}

/// Identity of the connected device, as sent over the socket.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceSummary {
    pub product_id: u16,
    pub model: String,
    pub serial: Option<String>,
    pub firmware: u16,
    pub interface: u8,
    pub mock: bool,
}

impl From<&DeviceInfo> for DeviceSummary {
    fn from(i: &DeviceInfo) -> Self {
        DeviceSummary {
            product_id: i.profile.product_id,
            model: i.profile.name.to_string(),
            serial: i.serial.clone(),
            firmware: i.firmware,
            interface: i.interface,
            mock: i.mock,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MonitorCmd {
    Bool(u8, bool),
    Level(u8, Option<f32>),
    Byte(u8, u8),
}

/// Raw control requests, for the CLI's inspection commands.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum RawOp {
    Get { ent: u8, sel: u8, ch: u8, len: usize },
    Set { ent: u8, sel: u8, ch: u8, data: Vec<u8> },
    GetMem { ent: u8, offset: u16, len: usize },
}

/// Client to daemon.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Request {
    /// First message. `gui` clients get state updates and meters.
    Hello { gui: bool },
    SetState(Box<MixerState>),
    SetSettings(Settings),
    Monitor(MonitorCmd),
    Optical { output: bool, mode: i32 },
    /// Re-read the mix from the device.
    ReadBack,
    Raw(RawOp),
    /// Stop the daemon.
    Shutdown,
}

/// Daemon to client.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Notice {
    Welcome { version: String },
    /// The connected device, or `None` with the reason it is not.
    Device(Option<DeviceSummary>, Option<String>),
    State(Box<MixerState>),
    Settings(Settings),
    Meters(Vec<Option<f32>>),
    Monitor(MonitorState),
    System(SystemState),
    VirtualDevices(Vec<vdev::Status>),
    Toast(String),
    RawReply(Result<Vec<u8>, String>),
    /// The daemon is exiting on request; GUIs close too.
    Quitting,
}

pub fn write_line<T: Serialize>(w: &mut impl Write, msg: &T) -> Result<()> {
    let mut line = serde_json::to_vec(msg)?;
    line.push(b'\n');
    w.write_all(&line)?;
    Ok(())
}

pub fn read_line<T: for<'de> Deserialize<'de>>(r: &mut impl BufRead) -> Result<Option<T>> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&line)?))
}

/// True when a daemon answers on the socket.
pub fn daemon_running() -> bool {
    UnixStream::connect(socket_path()).is_ok()
}

/// A connected client: a writer, plus a reader to consume notices.
pub struct Connection {
    pub writer: UnixStream,
    pub reader: BufReader<UnixStream>,
}

impl Connection {
    pub fn connect(gui: bool) -> Result<Connection> {
        let stream = UnixStream::connect(socket_path()).context("connecting to the lD Mixer daemon")?;
        let mut writer = stream.try_clone()?;
        write_line(&mut writer, &Request::Hello { gui })?;
        Ok(Connection { writer, reader: BufReader::new(stream) })
    }

    pub fn send(&mut self, req: &Request) -> Result<()> {
        write_line(&mut self.writer, req)
    }

    pub fn recv(&mut self) -> Result<Option<Notice>> {
        read_line(&mut self.reader)
    }
}

/// Start `ldmixer daemon` in the background, detached from this process,
/// with its log in `$XDG_STATE_HOME/ldmixer/daemon.log`.
pub fn spawn_daemon(mock: bool) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let exe = std::env::current_exe().context("locating the ldmixer binary")?;
    let log_dir = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir)
        .join("ldmixer");
    std::fs::create_dir_all(&log_dir)?;
    let log = std::fs::File::create(log_dir.join("daemon.log"))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("daemon");
    if mock {
        cmd.arg("--mock");
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // SAFETY: setsid is async-signal-safe; it detaches from our session so
    // the daemon outlives the terminal or GUI that started it.
    unsafe {
        cmd.pre_exec(|| {
            crate::reset_signal_mask();
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().context("starting the lD Mixer daemon")?;
    Ok(())
}

/// Connect, starting the daemon first if none is running.
pub fn connect_or_spawn(gui: bool, mock: bool) -> Result<Connection> {
    if let Ok(c) = Connection::connect(gui) {
        return Ok(c);
    }
    spawn_daemon(mock)?;
    for _ in 0..50 {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(c) = Connection::connect(gui) {
            return Ok(c);
        }
    }
    bail!("the lD Mixer daemon did not start; see ~/.local/state/ldmixer/daemon.log")
}

/// [`ControlIo`] through the daemon, so CLI commands work while it owns
/// the device.
pub struct RemoteIo {
    conn: Mutex<Connection>,
    info: DeviceInfo,
}

impl RemoteIo {
    pub fn connect() -> Result<RemoteIo> {
        let mut conn = Connection::connect(false)?;
        loop {
            match conn.recv()? {
                Some(Notice::Device(Some(d), _)) => {
                    let profile = protocol::profile_for(d.product_id)
                        .ok_or_else(|| anyhow!("daemon reports an unsupported device"))?;
                    let info = DeviceInfo {
                        profile,
                        serial: d.serial,
                        firmware: d.firmware,
                        interface: d.interface,
                        mock: d.mock,
                    };
                    return Ok(RemoteIo { conn: Mutex::new(conn), info });
                }
                Some(Notice::Device(None, reason)) => bail!(
                    "the lD Mixer daemon is running but has no device: {}",
                    reason.unwrap_or_else(|| "not connected".into())
                ),
                Some(_) => continue,
                None => bail!("the lD Mixer daemon closed the connection"),
            }
        }
    }

    fn call(&self, op: RawOp) -> Result<Vec<u8>> {
        let mut conn = self.conn.lock().unwrap();
        conn.send(&Request::Raw(op))?;
        loop {
            match conn.recv()? {
                Some(Notice::RawReply(r)) => return r.map_err(|e| anyhow!(e)),
                Some(_) => continue,
                None => bail!("the lD Mixer daemon closed the connection"),
            }
        }
    }
}

impl ControlIo for RemoteIo {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn set(&self, ent: u8, sel: u8, ch: u8, data: &[u8]) -> Result<()> {
        self.call(RawOp::Set { ent, sel, ch, data: data.to_vec() }).map(|_| ())
    }

    fn get(&self, ent: u8, sel: u8, ch: u8, len: usize) -> Result<Vec<u8>> {
        let b = self.call(RawOp::Get { ent, sel, ch, len })?;
        if b.len() != len {
            bail!("short read ({} of {len})", b.len());
        }
        Ok(b)
    }

    fn get_mem(&self, ent: u8, offset: u16, len: usize) -> Result<Vec<u8>> {
        self.call(RawOp::GetMem { ent, offset, len })
    }
}

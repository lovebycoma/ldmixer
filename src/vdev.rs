//! Virtual PipeWire devices that live only while lD Mixer runs.
//!
//! The iD24 appears in PipeWire as one 16-channel output and one 12-channel
//! input, which apps that want "a speaker" or "a microphone" cannot use
//! channel by channel. For each requested [`Spec`], lD Mixer spawns a
//! `pw-loopback` child that exposes just those channels as their own node:
//! a source for inputs (Mic, Digital, Loopback) or a sink for outputs (DAW
//! pairs). Nothing is written to any PipeWire configuration. A node exists
//! exactly as long as its child process, and each child is tied to lD
//! Mixer's lifetime: it is killed on drop and receives SIGTERM if lD Mixer
//! dies.

use std::collections::HashMap;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

/// One virtual device.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spec {
    /// PipeWire `node.name`; stable, used as the identity.
    pub node_name: String,
    /// Name shown in apps, e.g. "iD24 DAW 3+4".
    pub description: String,
    /// True: a sink apps play into (DAW). False: a source apps record from.
    pub playback: bool,
    /// Zero-based channels of the device's stream: one (mono) or two (stereo).
    pub channels: Vec<u8>,
}

impl Spec {
    /// The hardware loopback pair as a stereo source.
    pub fn loopback(model: &str, channels: (u8, u8)) -> Spec {
        Spec {
            node_name: "ldmixer_loopback".into(),
            description: format!("{model} Loopback"),
            playback: false,
            channels: vec![channels.0, channels.1],
        }
    }
}

/// State of one device, for display.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub spec: Spec,
    pub running: bool,
    pub error: Option<String>,
}

/// The device's own PipeWire nodes the loopbacks attach to.
#[derive(Clone, Debug, Default)]
pub struct HardwareNodes {
    pub capture: Option<String>,
    pub playback: Option<String>,
}

/// Find the Audient capture and playback nodes in `pw-dump`.
pub fn find_hardware_nodes() -> Result<HardwareNodes> {
    let out = Command::new("pw-dump")
        .stderr(Stdio::null())
        .output()
        .context("cannot run pw-dump (part of PipeWire)")?;
    let objects: serde_json::Value =
        serde_json::from_slice(&out.stdout).context("parsing pw-dump output")?;
    let mut nodes = HardwareNodes::default();
    for o in objects.as_array().into_iter().flatten() {
        let props = &o["info"]["props"];
        let name = props["node.name"].as_str().unwrap_or("");
        match props["media.class"].as_str() {
            Some("Audio/Source") if name.starts_with("alsa_input.usb-Audient") => {
                nodes.capture = Some(name.to_string())
            }
            Some("Audio/Sink") if name.starts_with("alsa_output.usb-Audient") => {
                nodes.playback = Some(name.to_string())
            }
            _ => {}
        }
    }
    Ok(nodes)
}

fn positions(prefix: &str, channels: &[u8]) -> String {
    let list: Vec<String> = channels.iter().map(|c| format!("{prefix}{c}")).collect();
    format!("[{}]", list.join(" "))
}

/// A running `pw-loopback` child.
struct Running {
    child: Child,
}

impl Running {
    fn start(spec: &Spec, hw: &HardwareNodes) -> Result<Running> {
        let n = spec.channels.len();
        anyhow::ensure!(n == 1 || n == 2, "a virtual device has one or two channels");
        let app_side = if n == 1 { "[MONO]" } else { "[FL FR]" };
        let target = if spec.playback { hw.playback.as_deref() } else { hw.capture.as_deref() };
        // The stream on the interface side must stay pinned to the hardware:
        // effect hosts such as EasyEffects move every app stream into their
        // own node, which here would route the device back into itself (a
        // feedback cycle that silences everything). `node.dont-move` refuses
        // such moves, `node.dont-fallback` forbids falling back to the
        // default device, and the `state.*` flags keep WirePlumber from
        // remembering anything about these short-lived streams.
        let device_side = format!(
            "audio.position={} target.object={} stream.dont-remix=true node.passive=true \
             node.dont-reconnect=true node.dont-move=true node.dont-fallback=true \
             state.restore-props=false state.restore-target=false",
            positions("AUX", &spec.channels),
            target.unwrap_or("")
        );
        let app_props = |class: &str| {
            format!(
                "media.class={class} node.name={} node.description=\"{}\" audio.position={app_side}",
                spec.node_name,
                spec.description.replace('"', "'")
            )
        };
        let mut cmd = Command::new("pw-loopback");
        cmd.args(["-n", &spec.node_name, "-c", &n.to_string(), "-m", app_side]);
        if spec.playback {
            let target = hw.playback.as_deref().context(
                "the interface has no playback device in PipeWire (is its profile set to Off or Input?)",
            )?;
            cmd.args(["-P", target, "-i", &app_props("Audio/Sink"), "-o", &device_side]);
        } else {
            let target = hw.capture.as_deref().context(
                "the interface has no capture device in PipeWire; choose the \"Multichannel Duplex\" or \"Pro Audio\" profile",
            )?;
            cmd.args(["-C", target, "-i", &device_side, "-o", &app_props("Audio/Source")]);
        }
        cmd.stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        // SAFETY: prctl is async-signal-safe and touches no Rust state.
        unsafe {
            cmd.pre_exec(|| {
                crate::reset_signal_mask();
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let child = cmd.spawn().context("cannot start pw-loopback (part of PipeWire)")?;
        Ok(Running { child })
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Entry {
    spec: Spec,
    running: Option<Running>,
    error: Option<String>,
    last_attempt: Option<Instant>,
}

/// Keeps the running devices equal to the wanted set.
#[derive(Default)]
pub struct Manager {
    entries: HashMap<String, Entry>,
    order: Vec<String>,
}

const RETRY_EVERY: Duration = Duration::from_secs(3);

impl Manager {
    /// Make the wanted set current: stop what is no longer wanted (or whose
    /// spec changed), start what is missing. Returns true if anything changed.
    pub fn reconcile(&mut self, wanted: &[Spec]) -> bool {
        let mut changed = false;
        let wanted_names: Vec<&str> = wanted.iter().map(|s| s.node_name.as_str()).collect();
        self.entries.retain(|name, e| {
            let keep = wanted.iter().any(|w| &w.node_name == name && *w == e.spec);
            changed |= !keep;
            keep
        });
        for spec in wanted {
            if !self.entries.contains_key(&spec.node_name) {
                self.entries.insert(
                    spec.node_name.clone(),
                    Entry { spec: spec.clone(), running: None, error: None, last_attempt: None },
                );
                changed = true;
            }
        }
        self.order = wanted_names.iter().map(|s| s.to_string()).collect();
        changed | self.start_missing(true)
    }

    /// Restart devices whose child died and retry failed ones, rate limited.
    /// Returns true if any status changed.
    pub fn maintain(&mut self) -> bool {
        self.start_missing(false)
    }

    fn start_missing(&mut self, force: bool) -> bool {
        let pending: Vec<String> = self
            .entries
            .iter_mut()
            .filter_map(|(name, e)| {
                let dead = e.running.as_mut().is_none_or(|r| !r.alive());
                let due = force && e.last_attempt.is_none()
                    || e.last_attempt.is_none_or(|t| t.elapsed() >= RETRY_EVERY);
                (dead && due).then(|| name.clone())
            })
            .collect();
        if pending.is_empty() {
            return false;
        }
        let hw = find_hardware_nodes();
        for name in &pending {
            let e = self.entries.get_mut(name).expect("pending entry");
            e.last_attempt = Some(Instant::now());
            e.running = None;
            let started = match &hw {
                Ok(hw) => Running::start(&e.spec, hw),
                Err(err) => Err(anyhow::anyhow!("{err:#}")),
            };
            match started {
                Ok(r) => {
                    e.running = Some(r);
                    e.error = None;
                }
                Err(err) => e.error = Some(format!("{err:#}")),
            }
        }
        true
    }

    /// Stop everything.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
    }

    pub fn status(&mut self) -> Vec<Status> {
        self.order
            .iter()
            .filter_map(|name| {
                let e = self.entries.get_mut(name)?;
                let running = e.running.as_mut().is_some_and(|r| r.alive());
                Some(Status { spec: e.spec.clone(), running, error: e.error.clone() })
            })
            .collect()
    }
}

/// Fail early, with a clear message, when PipeWire's tools are missing.
pub fn check_tools() -> Result<()> {
    for tool in ["pw-loopback", "pw-dump"] {
        let found = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).any(|d| d.join(tool).is_file()))
            .unwrap_or(false);
        if !found {
            bail!("{tool} not found; install PipeWire's command-line tools");
        }
    }
    Ok(())
}

//! "iD24 Loopback": a stereo PipeWire source for other apps, alive only
//! while lD Mixer runs.
//!
//! The hardware loopback arrives as two channels inside the multichannel
//! capture node, which apps that pick "a microphone" cannot select. This
//! spawns `pw-loopback`, which exposes just those two channels as their own
//! source. Nothing is written to any PipeWire configuration: the node exists
//! as long as the child process does, and the child is tied to lD Mixer's
//! lifetime (it is killed on drop and receives SIGTERM if we die).

use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};

use anyhow::{bail, Context, Result};

pub const NODE_NAME: &str = "ld_mixer_loopback";

pub struct LoopbackSource {
    child: Child,
}

impl LoopbackSource {
    /// Start the source; `channels` are the zero-based capture channels
    /// carrying the loopback (10 and 11 on the iD24).
    pub fn start(channels: (u8, u8)) -> Result<LoopbackSource> {
        let input = find_capture_node()?;
        let capture_props = format!(
            "audio.position=[AUX{} AUX{}] stream.dont-remix=true node.passive=true node.dont-reconnect=true",
            channels.0, channels.1
        );
        let playback_props = format!(
            "media.class=Audio/Source node.name={NODE_NAME} node.description=\"iD24 Loopback\" audio.position=[FL FR]"
        );
        let mut cmd = Command::new("pw-loopback");
        cmd.args(["-n", "ld-mixer-loopback", "-C", &input, "-i", &capture_props, "-o", &playback_props])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: prctl is async-signal-safe and touches no Rust state.
        unsafe {
            cmd.pre_exec(|| {
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
                Ok(())
            });
        }
        let child = cmd
            .spawn()
            .context("cannot start pw-loopback (part of PipeWire); is PipeWire installed?")?;
        Ok(LoopbackSource { child })
    }

    /// True while the child process runs.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for LoopbackSource {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Name of the Audient multichannel capture node, from `pw-dump`.
fn find_capture_node() -> Result<String> {
    let out = Command::new("pw-dump").output().context("cannot run pw-dump (part of PipeWire)")?;
    let objects: serde_json::Value = serde_json::from_slice(&out.stdout).context("parsing pw-dump output")?;
    for o in objects.as_array().into_iter().flatten() {
        let props = &o["info"]["props"];
        let name = props["node.name"].as_str().unwrap_or("");
        if props["media.class"] == "Audio/Source" && name.starts_with("alsa_input.usb-Audient") {
            return Ok(name.to_string());
        }
    }
    bail!(
        "the iD24 has no capture device in PipeWire; choose the \"Multichannel Duplex\" or \"Pro Audio\" profile"
    )
}

//! An in-memory stand-in for an iD24, for developing and demonstrating the
//! UI without hardware (`ldmixer gui --mock`).
//!
//! It stores every write so reads return it, answers the monitor section
//! with plausible values, and synthesizes moving meters from the mixer
//! matrix so faders visibly affect them.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use anyhow::Result;

use crate::device::{ControlIo, DeviceInfo};
use crate::protocol::{self, entity, monitor};

pub struct MockDevice {
    info: DeviceInfo,
    regs: Mutex<HashMap<(u8, u8, u8), Vec<u8>>>,
    start: Instant,
}

impl MockDevice {
    pub fn new() -> MockDevice {
        let profile = protocol::profile_for(0x000d).expect("iD24 profile");
        let mut regs = HashMap::new();
        regs.insert((entity::MONITOR, monitor::VOLUME, 0), (-20i16 * 256).to_le_bytes().to_vec());
        regs.insert((entity::MONITOR, monitor::DIM_LEVEL, 0), (-20i16 * 256).to_le_bytes().to_vec());
        regs.insert((entity::MONITOR, monitor::TALKBACK_SOURCE, 0), vec![0x10]);
        regs.insert((entity::SAMPLE_RATE, 0x01, 0), 48000i32.to_le_bytes().to_vec());
        regs.insert((entity::OPTICAL_IN, 0x00, 0), 0i32.to_le_bytes().to_vec());
        regs.insert((entity::OPTICAL_OUT, 0x01, 0), 0i32.to_le_bytes().to_vec());
        // Start from lD Mixer's factory mix so a readback looks like a real,
        // configured device.
        let state = crate::mixer::MixerState::default_for(profile);
        for (cell, raw) in state.matrix().into_iter().enumerate() {
            regs.insert((entity::MIXER, protocol::MIXER_CELL_SELECTOR, cell as u8), raw.to_le_bytes().to_vec());
        }
        for (out, code) in state.routing(profile) {
            regs.insert((entity::ROUTING, protocol::ROUTING_SELECTOR, out), vec![code]);
        }
        MockDevice {
            info: DeviceInfo {
                profile,
                serial: Some("MOCK0001".into()),
                firmware: 0x0112,
                interface: 4,
                mock: true,
            },
            regs: Mutex::new(regs),
            start: Instant::now(),
        }
    }
}

impl Default for MockDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlIo for MockDevice {
    fn info(&self) -> &DeviceInfo {
        &self.info
    }

    fn set(&self, ent: u8, selector: u8, channel: u8, data: &[u8]) -> Result<()> {
        self.regs.lock().unwrap().insert((ent, selector, channel), data.to_vec());
        Ok(())
    }

    fn get(&self, ent: u8, selector: u8, channel: u8, len: usize) -> Result<Vec<u8>> {
        let regs = self.regs.lock().unwrap();
        let mut v = regs.get(&(ent, selector, channel)).cloned().unwrap_or_default();
        v.resize(len, 0);
        Ok(v)
    }

    fn get_mem(&self, ent: u8, offset: u16, len: usize) -> Result<Vec<u8>> {
        let mut out = vec![0u8; len];
        if ent != entity::MIXER || offset != 0 || std::env::var_os("LDMIXER_MOCK_SILENT").is_some() {
            return Ok(out);
        }
        // Fake program material: mics breathe slowly, DAW returns play a
        // busy stereo signal, the digital inputs stay quiet.
        let t = self.start.elapsed().as_secs_f32();
        for node in 0..len / 2 {
            let base = match node {
                0 | 1 => 0.35 + 0.25 * (t * 0.7 + node as f32).sin().abs(),
                10 | 11 => 0.6 + 0.3 * ((t * 5.3 + node as f32 * 1.7).sin() * (t * 1.1).cos()).abs(),
                12 | 13 => 0.3 + 0.2 * (t * 3.1 + node as f32).sin().abs(),
                _ => 0.0,
            };
            if base > 0.0 {
                // int16 in 1/256 dB, like the hardware.
                let db = -60.0 * (1.0 - base.clamp(0.0, 1.0));
                let raw = ((db * 256.0) as i16).to_le_bytes();
                out[node * 2] = raw[0];
                out[node * 2 + 1] = raw[1];
            }
        }
        Ok(out)
    }
}

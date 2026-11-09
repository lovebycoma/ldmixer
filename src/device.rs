//! USB transport to an Audient iD interface.
//!
//! The firmware accepts its class control requests addressed to *any* of its
//! interfaces. lD Mixer claims the spare DFU interface, which no kernel
//! driver binds, so `snd-usb-audio` keeps the audio interfaces and playback
//! continues while the mixer is connected. Nothing here detaches a kernel
//! driver or resets the device.

use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use rusb::{Context, DeviceHandle, UsbContext};

use crate::protocol::{self, entity, Profile, REQ_CUR, REQ_MEM, RT_GET, RT_SET};

const WRITE_TIMEOUT: Duration = Duration::from_millis(250);
const READ_TIMEOUT: Duration = Duration::from_millis(150);

/// USB interface classes the firmware exposes as spare control targets.
const CLASS_APPLICATION: u8 = 0xfe; // DFU
const CLASS_VENDOR: u8 = 0xff;

/// A device found on the bus, before it is opened.
#[derive(Clone, Debug)]
pub struct Found {
    pub product_id: u16,
    pub bus: u8,
    pub address: u8,
    pub model: Option<&'static str>,
    pub supported: bool,
}

/// List every Audient device on the bus.
pub fn scan() -> Result<Vec<Found>> {
    let ctx = Context::new()?;
    let mut out = Vec::new();
    for dev in ctx.devices()?.iter() {
        let Ok(desc) = dev.device_descriptor() else { continue };
        if desc.vendor_id() != protocol::VENDOR_ID {
            continue;
        }
        let pid = desc.product_id();
        out.push(Found {
            product_id: pid,
            bus: dev.bus_number(),
            address: dev.address(),
            model: protocol::model_name(pid),
            supported: protocol::profile_for(pid).is_some(),
        });
    }
    Ok(out)
}

/// An opened device with its control interface claimed.
pub struct Device {
    handle: DeviceHandle<Context>,
    iface: u8,
    pub profile: &'static Profile,
    pub serial: Option<String>,
    pub firmware: u16,
}

/// Errors that mean the device went away (unplugged or suspended).
pub fn is_gone(err: &anyhow::Error) -> bool {
    matches!(
        err.downcast_ref::<rusb::Error>(),
        Some(rusb::Error::NoDevice) | Some(rusb::Error::NotFound)
    )
}

impl Device {
    /// Open the first supported Audient device.
    pub fn open() -> Result<Device> {
        let ctx = Context::new()?;
        let mut unsupported = None;
        for dev in ctx.devices()?.iter() {
            let Ok(desc) = dev.device_descriptor() else { continue };
            if desc.vendor_id() != protocol::VENDOR_ID {
                continue;
            }
            let Some(profile) = protocol::profile_for(desc.product_id()) else {
                unsupported = protocol::model_name(desc.product_id());
                continue;
            };
            let iface = find_control_interface(&dev)
                .ok_or_else(|| anyhow!("{} exposes no spare control interface", profile.name))?;
            let handle = dev.open().map_err(|e| match e {
                rusb::Error::Access => anyhow!(
                    "permission denied opening the {} (USB {:03}/{:03}); see README: USB access",
                    profile.name,
                    dev.bus_number(),
                    dev.address()
                ),
                e => anyhow::Error::from(e).context("opening USB device"),
            })?;
            // Only claim an interface that no kernel driver owns; never detach.
            if handle.kernel_driver_active(iface).unwrap_or(false) {
                bail!(
                    "interface {iface} of the {} is owned by a kernel driver; refusing to detach it",
                    profile.name
                );
            }
            handle
                .claim_interface(iface)
                .with_context(|| format!("claiming interface {iface}"))?;
            let serial = handle.read_serial_number_string_ascii(&desc).ok();
            let v = desc.device_version();
            let firmware = ((v.major() as u16) << 8) | ((v.minor() as u16) << 4) | v.sub_minor() as u16;
            return Ok(Device { handle, iface, profile, serial, firmware });
        }
        match unsupported {
            Some(name) => bail!("found an Audient {name}, which lD Mixer does not support yet"),
            None => bail!("no Audient iD interface found"),
        }
    }

    pub fn interface(&self) -> u8 {
        self.iface
    }

    fn windex(&self, ent: u8) -> u16 {
        ((ent as u16) << 8) | self.iface as u16
    }

    /// SET_CUR on `ent`, `selector`, `channel`.
    pub fn set(&self, ent: u8, selector: u8, channel: u8, data: &[u8]) -> Result<()> {
        let wvalue = ((selector as u16) << 8) | channel as u16;
        let n = self
            .handle
            .write_control(RT_SET, REQ_CUR, wvalue, self.windex(ent), data, WRITE_TIMEOUT)?;
        if n != data.len() {
            bail!("short write ({n} of {})", data.len());
        }
        Ok(())
    }

    /// GET_CUR on `ent`, `selector`, `channel`; returns exactly `len` bytes.
    pub fn get(&self, ent: u8, selector: u8, channel: u8, len: usize) -> Result<Vec<u8>> {
        let wvalue = ((selector as u16) << 8) | channel as u16;
        let mut buf = vec![0u8; len];
        let n = self
            .handle
            .read_control(RT_GET, REQ_CUR, wvalue, self.windex(ent), &mut buf, READ_TIMEOUT)?;
        if n != len {
            bail!("short read ({n} of {len})");
        }
        Ok(buf)
    }

    /// GET_MEM block read (meters).
    pub fn get_mem(&self, ent: u8, offset: u16, len: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; len];
        let n = self
            .handle
            .read_control(RT_GET, REQ_MEM, offset, self.windex(ent), &mut buf, READ_TIMEOUT)?;
        buf.truncate(n);
        Ok(buf)
    }

    // -- typed helpers -----------------------------------------------------

    pub fn get_u8(&self, ent: u8, selector: u8, channel: u8) -> Result<u8> {
        Ok(self.get(ent, selector, channel, 1)?[0])
    }

    pub fn get_i16(&self, ent: u8, selector: u8, channel: u8) -> Result<i16> {
        let b = self.get(ent, selector, channel, 2)?;
        Ok(i16::from_le_bytes([b[0], b[1]]))
    }

    pub fn get_i32(&self, ent: u8, selector: u8, channel: u8) -> Result<i32> {
        let b = self.get(ent, selector, channel, 4)?;
        Ok(i32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn set_u8(&self, ent: u8, selector: u8, channel: u8, v: u8) -> Result<()> {
        self.set(ent, selector, channel, &[v])
    }

    pub fn set_i16(&self, ent: u8, selector: u8, channel: u8, v: i16) -> Result<()> {
        self.set(ent, selector, channel, &v.to_le_bytes())
    }

    pub fn set_i32(&self, ent: u8, selector: u8, channel: u8, v: i32) -> Result<()> {
        self.set(ent, selector, channel, &v.to_le_bytes())
    }

    // -- protocol operations ------------------------------------------------

    pub fn set_mixer_cell(&self, cell: usize, raw: i16) -> Result<()> {
        self.set_i16(entity::MIXER, protocol::MIXER_CELL_SELECTOR, cell as u8, raw)
    }

    pub fn get_mixer_cell(&self, cell: usize) -> Result<i16> {
        self.get_i16(entity::MIXER, protocol::MIXER_CELL_SELECTOR, cell as u8)
    }

    pub fn set_route(&self, out: u8, code: u8) -> Result<()> {
        if out >= self.profile.routing_outputs {
            bail!("routing output {out} out of range");
        }
        self.set_u8(entity::ROUTING, protocol::ROUTING_SELECTOR, out, code)
    }

    pub fn get_route(&self, out: u8) -> Result<u8> {
        self.get_u8(entity::ROUTING, protocol::ROUTING_SELECTOR, out)
    }

    pub fn set_phase(&self, input: u8, on: bool) -> Result<()> {
        self.set_u8(entity::INPUT, protocol::PHASE_SELECTOR, input + 1, on as u8)
    }

    /// Input meters: one level byte per mixer node.
    pub fn read_input_meters(&self, nodes: usize) -> Result<Vec<u8>> {
        let buf = self.get_mem(entity::MIXER, 0, 32)?;
        Ok((0..nodes).map(|i| buf.get(i * 2).copied().unwrap_or(0)).collect())
    }

    pub fn sample_rate(&self) -> Result<i32> {
        self.get_i32(entity::SAMPLE_RATE, 0x01, 0)
    }

    /// Optical port format; `output` false = input port. 0 = ADAT, 1 = S/PDIF.
    pub fn optical_mode(&self, output: bool) -> Result<i32> {
        if output {
            self.get_i32(entity::OPTICAL_OUT, 0x01, 0)
        } else {
            self.get_i32(entity::OPTICAL_IN, 0x00, 0)
        }
    }

    pub fn set_optical_mode(&self, output: bool, mode: i32) -> Result<()> {
        if output {
            self.set_i32(entity::OPTICAL_OUT, 0x01, 0, mode)
        } else {
            self.set_i32(entity::OPTICAL_IN, 0x00, 0, mode)
        }
    }
}

impl Drop for Device {
    fn drop(&mut self) {
        let _ = self.handle.release_interface(self.iface);
    }
}

fn find_control_interface(dev: &rusb::Device<Context>) -> Option<u8> {
    let config = dev.active_config_descriptor().ok()?;
    for iface in config.interfaces() {
        for alt in iface.descriptors() {
            if alt.class_code() == CLASS_APPLICATION || alt.class_code() == CLASS_VENDOR {
                return Some(alt.interface_number());
            }
        }
    }
    None
}

//! The mixer as the user sees it, and its translation into hardware writes.
//!
//! The hardware has a 16 x 6 crosspoint matrix and no notion of faders, pan,
//! mute, solo or stereo links. Like the official iD Mixer, lD Mixer keeps
//! those concepts here and bakes them into matrix gains: every change
//! recomputes the full matrix, and the engine writes only the cells that
//! actually changed.

use serde::{Deserialize, Serialize};

use crate::protocol::{
    self, db_to_fader, fader_to_db, mixer_cell, Bus, NodeKind, Profile, RouteSource, MIXER_STRIDE,
};

/// One channel's settings within one mix (Main, Cue A or Cue B).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Send {
    /// Fader position, 0..=1 (see [`protocol::fader_to_db`]).
    pub fader: f64,
    /// Pan (mono) or balance (stereo pair), -1 = left .. +1 = right.
    pub pan: f64,
    pub mute: bool,
    pub solo: bool,
}

impl Send {
    pub const SILENT: Send = Send { fader: 0.0, pan: 0.0, mute: false, solo: false };
    pub const UNITY: Send = Send { fader: 1.0, pan: 0.0, mute: false, solo: false };
}

/// One mixer input node (one matrix row).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Channel {
    /// User label; empty means the profile's default name.
    #[serde(default)]
    pub label: String,
    /// Linked with the next node as a stereo pair (only meaningful on even
    /// nodes). A linked pair is controlled by this node's sends.
    pub linked: bool,
    /// Polarity invert (analog inputs only).
    #[serde(default)]
    pub phase: bool,
    /// Sends to Main, Cue A, Cue B.
    pub sends: [Send; 3],
}

/// Persistent mixer state for one device model.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MixerState {
    pub model: String,
    pub channels: Vec<Channel>,
    /// Master fader position of each mix.
    pub masters: [f64; 3],
    /// Routing source per output pair, in profile order.
    pub routes: Vec<RouteSource>,
}

/// A strip as drawn in the UI: one node, or a linked pair of nodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Strip {
    /// First node of the strip.
    pub node: usize,
    pub stereo: bool,
}

impl MixerState {
    /// Factory state: the computer (DAW 1+2) plays at unity into every mix,
    /// everything else is down. Nothing else reaches the speakers, so a
    /// first connect can never cause feedback.
    pub fn default_for(profile: &Profile) -> MixerState {
        let channels = profile
            .nodes
            .iter()
            .enumerate()
            .map(|(i, node)| Channel {
                label: String::new(),
                linked: node.default_linked,
                phase: false,
                sends: if node.kind == NodeKind::Daw && i == first_daw(profile) {
                    [Send::UNITY; 3]
                } else {
                    [Send::SILENT; 3]
                },
            })
            .collect();
        MixerState {
            model: profile.name.to_string(),
            channels,
            masters: [1.0; 3],
            routes: profile.outputs.iter().map(|o| o.default).collect(),
        }
    }

    /// Make a loaded state fit the profile (node or output count changes).
    pub fn sanitize(mut self, profile: &Profile) -> MixerState {
        let fresh = MixerState::default_for(profile);
        self.channels.truncate(profile.nodes.len());
        while self.channels.len() < profile.nodes.len() {
            self.channels.push(fresh.channels[self.channels.len()].clone());
        }
        for (i, ch) in self.channels.iter_mut().enumerate() {
            if i % 2 == 1 || i + 1 >= profile.nodes.len() {
                ch.linked = false;
            }
            for s in &mut ch.sends {
                s.fader = s.fader.clamp(0.0, 1.0);
                s.pan = s.pan.clamp(-1.0, 1.0);
            }
        }
        self.routes.truncate(profile.outputs.len());
        while self.routes.len() < profile.outputs.len() {
            self.routes.push(profile.outputs[self.routes.len()].default);
        }
        for (r, out) in self.routes.iter_mut().zip(profile.outputs) {
            if !out.sources.contains(r) {
                *r = out.default;
            }
        }
        self.model = profile.name.to_string();
        self
    }

    /// Strips in display order.
    pub fn strips(&self) -> Vec<Strip> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < self.channels.len() {
            let stereo = i % 2 == 0 && self.channels[i].linked && i + 1 < self.channels.len();
            out.push(Strip { node: i, stereo });
            i += if stereo { 2 } else { 1 };
        }
        out
    }

    /// Display name of a strip, e.g. "DAW 1+2" or a user label.
    pub fn strip_name(&self, profile: &Profile, strip: Strip) -> String {
        let label = &self.channels[strip.node].label;
        if !label.is_empty() {
            return label.clone();
        }
        let a = profile.nodes[strip.node].name;
        if strip.stereo {
            let b = profile.nodes[strip.node + 1].name;
            // "DAW 1" + "DAW 2" -> "DAW 1+2"
            match (a.rsplit_once(' '), b.rsplit_once(' ')) {
                (Some((pa, na)), Some((pb, nb))) if pa == pb => format!("{pa} {na}+{nb}"),
                _ => format!("{a}+{b}"),
            }
        } else {
            a.to_string()
        }
    }

    pub fn send(&self, node: usize, bus: Bus) -> &Send {
        &self.channels[node].sends[bus.index()]
    }

    pub fn send_mut(&mut self, node: usize, bus: Bus) -> &mut Send {
        &mut self.channels[node].sends[bus.index()]
    }

    /// Link or unlink the pair starting at even node `node`. Linking copies
    /// the left node's sends to the right one with the balance centered;
    /// unlinking hard-pans the two halves so the image is unchanged.
    pub fn set_linked(&mut self, node: usize, linked: bool) {
        if node % 2 != 0 || node + 1 >= self.channels.len() {
            return;
        }
        self.channels[node].linked = linked;
        let mut sends = self.channels[node].sends;
        if linked {
            // A mono pan carried over as balance would silence one side.
            for s in &mut sends {
                s.pan = 0.0;
            }
            self.channels[node].sends = sends;
            self.channels[node + 1].sends = sends;
        } else {
            // Spread the balance into hard-panned mono channels.
            for b in 0..3 {
                self.channels[node].sends[b].pan = -1.0;
                self.channels[node + 1].sends[b] = Send { pan: 1.0, ..sends[b] };
            }
        }
    }

    /// The full crosspoint matrix as raw device values.
    pub fn matrix(&self) -> Vec<i16> {
        let mut cells = vec![protocol::RAW_MUTE; self.channels.len() * MIXER_STRIDE];
        for bus in Bus::ALL {
            let strips = self.strips();
            let any_solo = strips.iter().any(|s| self.send(s.node, bus).solo);
            let master = fader_to_db(self.masters[bus.index()]);
            for strip in strips {
                let s = self.send(strip.node, bus);
                if s.mute || (any_solo && !s.solo) {
                    continue;
                }
                let (Some(level), Some(master)) = (fader_to_db(s.fader), master) else { continue };
                let level = level + master;
                // Balance law: centre is unity on both sides, turning away
                // from a side attenuates it linearly down to silence.
                let gl = (1.0 - s.pan).min(1.0);
                let gr = (1.0 + s.pan).min(1.0);
                let db = |g: f64| -> Option<f32> {
                    if g <= 0.0001 { None } else { Some(level + 20.0 * g.log10() as f32) }
                };
                if strip.stereo {
                    cells[mixer_cell(strip.node, bus, 0)] = protocol::db_to_raw(db(gl));
                    cells[mixer_cell(strip.node + 1, bus, 1)] = protocol::db_to_raw(db(gr));
                } else {
                    cells[mixer_cell(strip.node, bus, 0)] = protocol::db_to_raw(db(gl));
                    cells[mixer_cell(strip.node, bus, 1)] = protocol::db_to_raw(db(gr));
                }
            }
        }
        cells
    }

    /// Routing writes as `(output index, code)` pairs.
    pub fn routing(&self, profile: &Profile) -> Vec<(u8, u8)> {
        let mut out = Vec::new();
        for (pair, src) in profile.outputs.iter().zip(&self.routes) {
            for o in [pair.first, pair.first + 1] {
                if let Some(code) = protocol::route_code(profile.route_scheme, o, *src) {
                    out.push((o, code));
                }
            }
        }
        out
    }

    /// Phase switches as `(input index, on)`.
    pub fn phases(&self, profile: &Profile) -> Vec<(u8, bool)> {
        (0..profile.phase_inputs as usize)
            .map(|i| (i as u8, self.channels.get(i).map(|c| c.phase).unwrap_or(false)))
            .collect()
    }

    /// Best-effort import of a matrix read back from the hardware, so a first
    /// start shows what the device is actually doing instead of overwriting
    /// it. Pairs that are currently linked stay linked when the hardware
    /// carries them as a clean stereo pair (no cross-feeds); everything else
    /// becomes mono channels with fader and pan derived from their L/R cells.
    /// Masters stay at unity, so the imported matrix reproduces exactly.
    pub fn import_matrix(&mut self, cells: &[i16]) {
        let db = |node: usize, bus: Bus, side: usize| {
            cells.get(mixer_cell(node, bus, side)).copied().and_then(protocol::raw_to_db)
        };
        let n = self.channels.len();
        let mut node = 0;
        while node < n {
            let pair = node % 2 == 0
                && node + 1 < n
                && self.channels[node].linked
                && Bus::ALL
                    .iter()
                    .all(|&b| db(node, b, 1).is_none() && db(node + 1, b, 0).is_none());
            if pair {
                for bus in Bus::ALL {
                    let send = send_from_cells(db(node, bus, 0), db(node + 1, bus, 1));
                    self.channels[node].sends[bus.index()] = send;
                    self.channels[node + 1].sends[bus.index()] = send;
                }
                node += 2;
            } else {
                self.channels[node].linked = false;
                for bus in Bus::ALL {
                    self.channels[node].sends[bus.index()] =
                        send_from_cells(db(node, bus, 0), db(node, bus, 1));
                }
                node += 1;
            }
        }
        self.masters = [1.0; 3];
    }
}

/// Fader and pan that reproduce a left/right gain pair (inverse of the
/// balance law in [`MixerState::matrix`]).
fn send_from_cells(l: Option<f32>, r: Option<f32>) -> Send {
    let lin = |d: Option<f32>| d.map(|d| 10f64.powf(d.min(0.0) as f64 / 20.0)).unwrap_or(0.0);
    let (gl, gr) = (lin(l), lin(r));
    let peak = gl.max(gr);
    if peak <= 0.0 {
        return Send::SILENT;
    }
    Send {
        fader: db_to_fader(Some(20.0 * peak.log10() as f32)),
        pan: if gl >= gr { gr / gl - 1.0 } else { 1.0 - gl / gr },
        mute: false,
        solo: false,
    }
}

fn first_daw(profile: &Profile) -> usize {
    profile.nodes.iter().position(|n| n.kind == NodeKind::Daw).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{profile_for, RAW_MUTE};

    fn id24() -> &'static Profile {
        profile_for(0x000d).unwrap()
    }

    #[test]
    fn default_plays_daw_only() {
        let s = MixerState::default_for(id24());
        let m = s.matrix();
        // DAW 1+2 is node 10/11, linked: L cell of node 10, R cell of node 11.
        for bus in Bus::ALL {
            assert_eq!(m[mixer_cell(10, bus, 0)], 0);
            assert_eq!(m[mixer_cell(10, bus, 1)], RAW_MUTE);
            assert_eq!(m[mixer_cell(11, bus, 0)], RAW_MUTE);
            assert_eq!(m[mixer_cell(11, bus, 1)], 0);
        }
        // Mics are silent.
        assert!(m[..12].iter().all(|&c| c == RAW_MUTE));
    }

    #[test]
    fn solo_and_mute() {
        let mut s = MixerState::default_for(id24());
        s.send_mut(0, Bus::Main).fader = 1.0;
        s.send_mut(0, Bus::Main).solo = true;
        let m = s.matrix();
        assert_eq!(m[mixer_cell(0, Bus::Main, 0)], 0);
        assert_eq!(m[mixer_cell(10, Bus::Main, 0)], RAW_MUTE, "DAW silenced by solo");
        assert_eq!(m[mixer_cell(10, Bus::CueA, 0)], 0, "solo is per mix");
        s.send_mut(0, Bus::Main).mute = true;
        assert_eq!(s.matrix()[mixer_cell(0, Bus::Main, 0)], RAW_MUTE);
    }

    #[test]
    fn pan_law() {
        let mut s = MixerState::default_for(id24());
        let ch = s.send_mut(0, Bus::Main);
        ch.fader = 1.0;
        ch.pan = 1.0;
        let m = s.matrix();
        assert_eq!(m[0], RAW_MUTE);
        assert_eq!(m[1], 0);
        s.send_mut(0, Bus::Main).pan = -0.5;
        let m = s.matrix();
        assert_eq!(m[0], 0);
        assert_eq!(protocol::raw_to_db(m[1]).map(|d| d.round()), Some(-6.0));
    }

    #[test]
    fn import_keeps_clean_pairs_linked() {
        let s = MixerState::default_for(id24());
        let mut t = MixerState::default_for(id24());
        t.import_matrix(&s.matrix());
        assert!(t.channels[10].linked, "DAW 1+2 stays a stereo pair");
        assert_eq!(t.send(10, Bus::Main).fader, 1.0);
        assert_eq!(t.matrix(), s.matrix());
    }

    #[test]
    fn linking_centers_balance() {
        let mut s = MixerState::default_for(id24());
        s.send_mut(0, Bus::Main).fader = 1.0;
        s.send_mut(0, Bus::Main).pan = -1.0;
        s.set_linked(0, true);
        let m = s.matrix();
        assert_eq!(m[mixer_cell(0, Bus::Main, 0)], 0);
        assert_eq!(m[mixer_cell(1, Bus::Main, 1)], 0, "right half audible");
    }

    #[test]
    fn import_round_trip() {
        let mut s = MixerState::default_for(id24());
        s.set_linked(10, false);
        s.send_mut(0, Bus::CueA).fader = 0.7;
        s.send_mut(0, Bus::CueA).pan = 0.4;
        let m = s.matrix();
        let mut t = MixerState::default_for(id24());
        t.import_matrix(&m);
        assert_eq!(t.matrix(), m);
    }
}

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
    /// Pan, -1 = left .. +1 = right. In a linked pair each channel keeps
    /// its own pan (L100 / R100 by default); fader, mute and solo come from
    /// the pair's first channel.
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
    /// Expose this strip to other apps as its own PipeWire input (Mic,
    /// Digital) or output (DAW) while lD Mixer runs. On a linked pair the
    /// first node's flag counts.
    #[serde(default)]
    pub virtual_device: bool,
    /// Hidden from the mixer window (audio is unaffected). On a linked pair
    /// the first channel's flag counts.
    #[serde(default)]
    pub hidden: bool,
}

/// Current layout of [`MixerState`] files. Version 2: each channel of a
/// linked pair has its own pan (version 1 stored one balance on the pair).
pub const STATE_VERSION: u32 = 2;

/// Persistent mixer state for one device model.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MixerState {
    /// File format version; missing in version 1 files.
    #[serde(default)]
    pub version: u32,
    pub model: String,
    pub channels: Vec<Channel>,
    /// Master fader position of each mix.
    pub masters: [f64; 3],
    /// Routing source per output pair, in profile order.
    pub routes: Vec<RouteSource>,
    /// User names for Main, Cue A and Cue B; empty means the default.
    #[serde(default)]
    pub mix_names: [String; 3],
    /// User colors for Main, Cue A and Cue B as `#rrggbb`; empty means the
    /// default.
    #[serde(default)]
    pub mix_colors: [String; 3],
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
                virtual_device: false,
                hidden: false,
                sends: if node.kind == NodeKind::Daw && i == first_daw(profile) {
                    [Send::UNITY; 3]
                } else {
                    [Send::SILENT; 3]
                },
            })
            .collect();
        MixerState {
            version: STATE_VERSION,
            model: profile.name.to_string(),
            channels,
            masters: [1.0; 3],
            routes: profile.outputs.iter().map(|o| o.default).collect(),
            mix_names: Default::default(),
            mix_colors: Default::default(),
        }
        .with_pair_pans()
    }

    /// Give every linked pair its stereo spread (L100 / R100).
    fn with_pair_pans(mut self) -> MixerState {
        for i in (0..self.channels.len().saturating_sub(1)).step_by(2) {
            if self.channels[i].linked {
                for b in 0..3 {
                    self.channels[i].sends[b].pan = -1.0;
                    self.channels[i + 1].sends[b].pan = 1.0;
                }
            }
        }
        self
    }

    /// Display name of a mix: the user's name or the default.
    pub fn mix_name(&self, bus: Bus) -> String {
        let n = self.mix_names[bus.index()].trim();
        if n.is_empty() { default_mix_name(bus).to_string() } else { n.to_string() }
    }

    /// Display color of a mix as `0xrrggbb`: the user's color or the default.
    pub fn mix_color(&self, bus: Bus) -> u32 {
        parse_color(&self.mix_colors[bus.index()]).unwrap_or_else(|| default_mix_color(bus))
    }

    /// Set a mix's color; the default color is stored as empty.
    pub fn set_mix_color(&mut self, bus: Bus, rgb: u32) {
        let rgb = rgb & 0xff_ffff;
        self.mix_colors[bus.index()] =
            if rgb == default_mix_color(bus) { String::new() } else { format!("#{rgb:06x}") };
    }

    /// Label of a routing source, using the mixes' names.
    pub fn source_label(&self, src: RouteSource) -> String {
        match src {
            RouteSource::Main => self.mix_name(Bus::Main),
            RouteSource::CueA => self.mix_name(Bus::CueA),
            RouteSource::CueB => self.mix_name(Bus::CueB),
            other => other.label(),
        }
    }

    /// Strips shown in the window (not hidden).
    pub fn visible_strips(&self) -> Vec<Strip> {
        self.strips().into_iter().filter(|s| !self.channels[s.node].hidden).collect()
    }

    /// Make a loaded state fit the profile (node or output count changes).
    pub fn sanitize(mut self, profile: &Profile) -> MixerState {
        if self.version < 2 {
            // Version 1 kept a single balance per linked pair; give each
            // channel its own side instead so pairs stay stereo.
            self = self.with_pair_pans();
            self.version = STATE_VERSION;
        }
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
    /// the first channel's level, mute and solo to the second and spreads
    /// the pair L100 / R100; unlinking centers both channels.
    pub fn set_linked(&mut self, node: usize, linked: bool) {
        if node % 2 != 0 || node + 1 >= self.channels.len() {
            return;
        }
        self.channels[node].linked = linked;
        let lead = self.channels[node].sends;
        for b in 0..3 {
            let (l, r) = if linked { (-1.0, 1.0) } else { (0.0, 0.0) };
            self.channels[node].sends[b].pan = l;
            self.channels[node + 1].sends[b] = Send { pan: r, ..lead[b] };
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
                let db = |g: f64| -> Option<f32> {
                    if g <= 0.0001 { None } else { Some(level + 20.0 * g.log10() as f32) }
                };
                // Each channel of the strip is panned by its own pan with the
                // balance law: centre is unity on both sides, turning away
                // from a side attenuates it linearly down to silence.
                let n = if strip.stereo { 2 } else { 1 };
                for node in strip.node..strip.node + n {
                    let pan = self.send(node, bus).pan;
                    let gl = (1.0 - pan).min(1.0);
                    let gr = (1.0 + pan).min(1.0);
                    cells[mixer_cell(node, bus, 0)] = protocol::db_to_raw(db(gl));
                    cells[mixer_cell(node, bus, 1)] = protocol::db_to_raw(db(gr));
                }
            }
        }
        cells
    }

    /// The PipeWire devices the strips ask for, in strip order.
    pub fn virtual_devices(&self, profile: &Profile) -> Vec<crate::vdev::Spec> {
        self.strips()
            .into_iter()
            .filter(|s| self.channels[s.node].virtual_device)
            .map(|s| {
                let first = &profile.nodes[s.node];
                let mut channels = vec![first.stream_channel];
                // Stable node name from the stock names: "DAW 3"+"DAW 4" -> daw_3_4.
                let mut slug = first.name.to_lowercase().replace(' ', "_");
                if s.stereo {
                    let second = &profile.nodes[s.node + 1];
                    channels.push(second.stream_channel);
                    slug.push('_');
                    slug.push_str(second.name.rsplit(' ').next().unwrap_or(""));
                }
                crate::vdev::Spec {
                    node_name: format!("ldmixer_{slug}"),
                    description: format!("{} {}", profile.name, self.strip_name(profile, s)),
                    playback: first.kind.is_playback(),
                    channels,
                }
            })
            .collect()
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

    /// Take over a mix read back from the device. Returns false (and
    /// changes nothing) when the read is unusable: missing, or aliased with
    /// every row identical, which some firmware answers with.
    pub fn apply_readback(&mut self, profile: &Profile, matrix: Option<&[i16]>, routing: &[Option<u8>]) -> bool {
        let Some(m) = matrix else { return false };
        let rows: Vec<&[i16]> = m.chunks(MIXER_STRIDE).collect();
        let aliased = rows.windows(2).all(|w| w[0] == w[1])
            && rows.first().is_some_and(|r| r.iter().any(|&c| c != protocol::RAW_MUTE));
        if aliased || m.len() != self.channels.len() * MIXER_STRIDE {
            return false;
        }
        self.import_matrix(m);
        for (i, pair) in profile.outputs.iter().enumerate() {
            let code = routing.get(pair.first as usize).copied().flatten();
            if let Some(src) = code.and_then(|c| protocol::route_source_from_code(profile.route_scheme, c)) {
                if pair.sources.contains(&src) {
                    self.routes[i] = src;
                }
            }
        }
        true
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
            let mono = |node: usize, bus: Bus| send_from_cells(db(node, bus, 0), db(node, bus, 1));
            // A linked pair stays linked when both channels run at the same
            // level in every mix (each keeps its own pan).
            let pair = node % 2 == 0
                && node + 1 < n
                && self.channels[node].linked
                && Bus::ALL.iter().all(|&b| (mono(node, b).fader - mono(node + 1, b).fader).abs() < 1e-3);
            if pair {
                for bus in Bus::ALL {
                    let (mut l, mut r) = (mono(node, bus), mono(node + 1, bus));
                    if l.fader == 0.0 {
                        // Silent: keep the usual stereo spread.
                        (l.pan, r.pan) = (-1.0, 1.0);
                    }
                    self.channels[node].sends[bus.index()] = l;
                    self.channels[node + 1].sends[bus.index()] = r;
                }
                node += 2;
            } else {
                if node % 2 == 0 {
                    self.channels[node].linked = false;
                }
                for bus in Bus::ALL {
                    self.channels[node].sends[bus.index()] = mono(node, bus);
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

/// Factory color of a mix, `0xrrggbb`.
pub fn default_mix_color(bus: Bus) -> u32 {
    match bus {
        Bus::Main => 0x2dd4bf,
        Bus::CueA => 0xf59e0b,
        Bus::CueB => 0xa78bfa,
    }
}

/// Parse `#rrggbb` (the `#` is optional).
fn parse_color(s: &str) -> Option<u32> {
    let h = s.trim().trim_start_matches('#');
    if h.len() == 6 && h.bytes().all(|b| b.is_ascii_hexdigit()) {
        u32::from_str_radix(h, 16).ok()
    } else {
        None
    }
}

pub fn default_mix_name(bus: Bus) -> &'static str {
    match bus {
        Bus::Main => "Main Mix",
        Bus::CueA => "Cue A",
        Bus::CueB => "Cue B",
    }
}

fn first_daw(profile: &Profile) -> usize {
    profile.nodes.iter().position(|n| n.kind == NodeKind::Daw).unwrap_or(usize::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{profile_for, RouteSource, RAW_MUTE};

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
    fn linking_spreads_and_unlinking_centers() {
        let mut s = MixerState::default_for(id24());
        s.send_mut(0, Bus::Main).fader = 1.0;
        s.set_linked(0, true);
        assert_eq!((s.send(0, Bus::Main).pan, s.send(1, Bus::Main).pan), (-1.0, 1.0));
        let m = s.matrix();
        assert_eq!(m[mixer_cell(0, Bus::Main, 0)], 0);
        assert_eq!(m[mixer_cell(0, Bus::Main, 1)], RAW_MUTE);
        assert_eq!(m[mixer_cell(1, Bus::Main, 1)], 0, "right channel audible");
        // Each channel pans on its own inside the pair.
        s.send_mut(1, Bus::Main).pan = 0.0;
        let m = s.matrix();
        assert_eq!(m[mixer_cell(1, Bus::Main, 0)], 0);
        assert_eq!(m[mixer_cell(1, Bus::Main, 1)], 0);
        s.set_linked(0, false);
        assert_eq!((s.send(0, Bus::Main).pan, s.send(1, Bus::Main).pan), (0.0, 0.0));
        assert_eq!(s.send(1, Bus::Main).fader, 1.0, "level carried over");
    }

    #[test]
    fn virtual_device_specs() {
        let mut s = MixerState::default_for(id24());
        s.channels[0].virtual_device = true; // Mic 1, mono
        s.channels[12].virtual_device = true; // DAW 3+4, linked
        s.channels[12].label = "Discord".into();
        let v = s.virtual_devices(id24());
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].node_name, "ldmixer_mic_1");
        assert_eq!(v[0].channels, vec![0]);
        assert!(!v[0].playback);
        assert_eq!(v[1].node_name, "ldmixer_daw_3_4");
        assert_eq!(v[1].description, "iD24 Discord");
        assert_eq!(v[1].channels, vec![2, 3]);
        assert!(v[1].playback);
    }

    #[test]
    fn version_1_pairs_stay_stereo() {
        let mut old = MixerState::default_for(id24());
        old.version = 0;
        for b in 0..3 {
            old.channels[10].sends[b].pan = 0.0; // v1 balance, centered
            old.channels[11].sends[b].pan = 0.0;
        }
        let s = old.sanitize(id24());
        assert_eq!(s.version, STATE_VERSION);
        assert_eq!((s.send(10, Bus::Main).pan, s.send(11, Bus::Main).pan), (-1.0, 1.0));
    }

    #[test]
    fn mix_names_and_hidden() {
        let mut s = MixerState::default_for(id24());
        assert_eq!(s.mix_name(Bus::CueA), "Cue A");
        s.mix_names[1] = "Drummer".into();
        assert_eq!(s.source_label(RouteSource::CueA), "Drummer");
        let before = s.visible_strips().len();
        s.channels[2].hidden = true; // Digi 1+2
        assert_eq!(s.visible_strips().len(), before - 1);
    }

    #[test]
    fn mix_colors() {
        let mut s = MixerState::default_for(id24());
        assert_eq!(s.mix_color(Bus::Main), default_mix_color(Bus::Main));
        s.set_mix_color(Bus::CueA, 0x112233);
        assert_eq!(s.mix_colors[1], "#112233");
        assert_eq!(s.mix_color(Bus::CueA), 0x112233);
        s.set_mix_color(Bus::CueA, default_mix_color(Bus::CueA));
        assert_eq!(s.mix_colors[1], "");
        s.mix_colors[2] = "not a color".into();
        assert_eq!(s.mix_color(Bus::CueB), default_mix_color(Bus::CueB));
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

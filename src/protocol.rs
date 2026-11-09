//! Audient iD vendor control protocol: constants, value encodings and the
//! per-model device profiles.
//!
//! Everything here is pure data and arithmetic, with no USB access, so it can
//! be unit tested without hardware. The protocol knowledge is collected from
//! prior reverse-engineering work (MixiD, BiD, Monix); see `docs/PROTOCOL.md`
//! for sources and for what is verified versus inferred.

/// USB vendor id shared by every Audient device.
pub const VENDOR_ID: u16 = 0x2708;

/// Class request types on the control pipe (recipient: interface).
pub const RT_SET: u8 = 0x21;
pub const RT_GET: u8 = 0xa1;
/// `bRequest` for current-value get/set (UAC `CUR`).
pub const REQ_CUR: u8 = 0x01;
/// `bRequest` for block memory reads, used for the meters.
pub const REQ_MEM: u8 = 0x03;

/// Unit ("entity") ids. They come from the USB descriptors and are the same
/// across the iD family as far as anyone has checked.
pub mod entity {
    /// Optical input format (selector 0x00): 0 = ADAT, 1 = S/PDIF.
    pub const OPTICAL_IN: u8 = 0x01;
    /// Input feature unit: phase invert (selector 0x0d, channel 1..n).
    pub const INPUT: u8 = 0x0b;
    /// Output feature unit (volume, selector 0x02). Left alone by lD Mixer.
    pub const OUTPUT_FU: u8 = 0x0c;
    /// Optical output format (selector 0x01): 0 = ADAT, 1 = S/PDIF.
    pub const OPTICAL_OUT: u8 = 0x14;
    /// Sample rate (selector 0x01, int32 Hz).
    pub const SAMPLE_RATE: u8 = 0x29;
    /// Output routing (selector 0x06, channel = output index, 1 byte code).
    pub const ROUTING: u8 = 0x33;
    /// Monitor section (volume, dim, cut, alt, talkback, mono, polarity).
    pub const MONITOR: u8 = 0x36;
    /// Mixer matrix (selector 0x01, channel = cell) and meters (GET_MEM).
    pub const MIXER: u8 = 0x3c;
}

/// Selectors on the monitor entity (0x36). All of them answer reads.
pub mod monitor {
    pub const MONO: u8 = 0x00;
    /// Mono mode: 0 = L+R sum, 1 = left only, 2 = right only.
    pub const MONO_MODE: u8 = 0x01;
    pub const POLARITY: u8 = 0x03;
    pub const CUT: u8 = 0x04;
    pub const DIM: u8 = 0x05;
    /// Dim attenuation, int16 in 1/256 dB.
    pub const DIM_LEVEL: u8 = 0x06;
    pub const TALKBACK: u8 = 0x07;
    /// Talkback source: 0x10 = Mic 1, 0x11 = Mic 2, 0x12 = Digital 1.
    pub const TALKBACK_SOURCE: u8 = 0x08;
    pub const ALT: u8 = 0x0c;
    /// Monitor volume, int16 in 1/256 dB. Mirrors the front-panel encoder.
    pub const VOLUME: u8 = 0x12;
}

/// Selector of a mixer matrix crosspoint.
pub const MIXER_CELL_SELECTOR: u8 = 0x01;
/// Selector of a routing destination.
pub const ROUTING_SELECTOR: u8 = 0x06;
/// Selector of an input's phase-invert switch.
pub const PHASE_SELECTOR: u8 = 0x0d;

// ---------------------------------------------------------------------------
// Level encoding
// ---------------------------------------------------------------------------

/// The raw value that means "silence" on every level control.
pub const RAW_MUTE: i16 = i16::MIN; // 0x8000

/// Lowest level written as a real value; anything quieter becomes `RAW_MUTE`.
pub const FLOOR_DB: f32 = -100.0;

/// Encode a level in dB as the device's int16 (1/256 dB, 0 = 0 dB).
/// `None` or anything at or below [`FLOOR_DB`] encodes as mute.
pub fn db_to_raw(db: Option<f32>) -> i16 {
    match db {
        Some(d) if d.is_finite() && d > FLOOR_DB => {
            (d * 256.0).round().clamp(-32767.0, 32767.0) as i16
        }
        _ => RAW_MUTE,
    }
}

/// Decode the device's int16 level into dB (`None` = mute).
pub fn raw_to_db(raw: i16) -> Option<f32> {
    if raw == RAW_MUTE {
        None
    } else {
        Some(raw as f32 / 256.0)
    }
}

/// Fader taper: maps a fader position in `0.0..=1.0` to dB.
///
/// Top of travel is +0 dB, which is the safe maximum we know the matrix
/// accepts. The cubic law (`20*log10(p^3)`) gives a familiar feel: half
/// travel is about -18 dB, a quarter is about -36 dB, and the bottom is mute.
pub fn fader_to_db(pos: f64) -> Option<f32> {
    if pos <= 0.0005 {
        return None;
    }
    let db = 60.0 * pos.clamp(0.0, 1.0).log10();
    if db <= FLOOR_DB as f64 {
        None
    } else {
        Some(db as f32)
    }
}

/// Inverse of [`fader_to_db`].
pub fn db_to_fader(db: Option<f32>) -> f64 {
    match db {
        None => 0.0,
        Some(d) => 10f64.powf(d.min(0.0) as f64 / 60.0),
    }
}

/// Format a level for display, e.g. "-12.0 dB" or "-inf".
pub fn format_db(db: Option<f32>) -> String {
    match db {
        None => "-inf".to_string(),
        Some(d) if d.abs() < 0.05 => "0.0 dB".to_string(),
        Some(d) => format!("{d:+.1} dB"),
    }
}

// ---------------------------------------------------------------------------
// Mixer matrix
// ---------------------------------------------------------------------------

/// The three stereo buses of the hardware mixer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Bus {
    Main,
    CueA,
    CueB,
}

impl Bus {
    pub const ALL: [Bus; 3] = [Bus::Main, Bus::CueA, Bus::CueB];

    pub fn index(self) -> usize {
        match self {
            Bus::Main => 0,
            Bus::CueA => 1,
            Bus::CueB => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Bus::Main => "Main",
            Bus::CueA => "Cue A",
            Bus::CueB => "Cue B",
        }
    }
}

/// Number of cells one matrix row (one mixer input node) spans: L/R of each
/// of the three buses.
pub const MIXER_STRIDE: usize = 6;

/// Flat crosspoint number for `node` feeding `bus`, `side` 0 = left, 1 = right.
pub fn mixer_cell(node: usize, bus: Bus, side: usize) -> usize {
    node * MIXER_STRIDE + bus.index() * 2 + side
}

// ---------------------------------------------------------------------------
// Routing
// ---------------------------------------------------------------------------

/// What a physical output pair plays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RouteSource {
    /// The monitor section's feed: volume knob, dim, cut and mono apply.
    Main,
    /// Alternate speaker feed: silent unless ALT is engaged.
    Alt,
    CueA,
    CueB,
    /// A DAW playback pair, zero-based first channel (0 = DAW 1+2), at full
    /// level, bypassing the monitor section.
    Daw(u8),
    /// A physical input's direct tap, zero-based first channel.
    Input(u8),
}

impl RouteSource {
    pub fn label(self) -> String {
        match self {
            RouteSource::Main => "Main Mix".into(),
            RouteSource::Alt => "Alt Speakers".into(),
            RouteSource::CueA => "Cue A".into(),
            RouteSource::CueB => "Cue B".into(),
            RouteSource::Daw(n) => format!("DAW {}+{}", n + 1, n + 2),
            RouteSource::Input(n) => format!("Input {}+{}", n + 1, n + 2),
        }
    }
}

/// How a model computes routing codes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteScheme {
    /// Codes unknown: never write routing.
    None,
    /// The iD24 formula, ear-verified on hardware by BiD.
    Id24,
    /// The iD14 table inherited from MixiD.
    Id14,
}

/// Routing code for output index `out` (its parity picks the side of the
/// pair) playing `src`. `None` when the scheme cannot express it.
pub fn route_code(scheme: RouteScheme, out: u8, src: RouteSource) -> Option<u8> {
    let side = out & 1;
    match scheme {
        RouteScheme::None => None,
        RouteScheme::Id24 => Some(match src {
            RouteSource::Main => 0x25 + side,
            RouteSource::Alt => 0x27 + side,
            RouteSource::CueA => 0x1e + side,
            RouteSource::CueB => 0x20 + side,
            RouteSource::Daw(n) => n + side,
            RouteSource::Input(n) => 0x10 + n + side,
        }),
        RouteScheme::Id14 => match src {
            RouteSource::Main => Some(0x1b + side),
            RouteSource::Alt => Some(0x1d + side),
            // The iD14 table only has the mono-summed cue codes.
            RouteSource::CueA => Some(0x19),
            RouteSource::CueB => Some(0x1a),
            RouteSource::Daw(n) => Some(n + side),
            RouteSource::Input(_) => None,
        },
    }
}

/// Reverse of [`route_code`] for the iD24 scheme, used to show what the
/// hardware currently plays. Returns the pair source a code belongs to.
pub fn route_source_from_code(scheme: RouteScheme, code: u8) -> Option<RouteSource> {
    match scheme {
        RouteScheme::Id24 => match code {
            0x25 | 0x26 => Some(RouteSource::Main),
            0x27 | 0x28 => Some(RouteSource::Alt),
            0x1e | 0x1f => Some(RouteSource::CueA),
            0x20 | 0x21 => Some(RouteSource::CueB),
            0x00..=0x0f => Some(RouteSource::Daw(code & !1)),
            0x10..=0x1b => Some(RouteSource::Input((code - 0x10) & !1)),
            _ => None,
        },
        RouteScheme::Id14 => match code {
            0x1b | 0x1c => Some(RouteSource::Main),
            0x1d | 0x1e => Some(RouteSource::Alt),
            0x19 => Some(RouteSource::CueA),
            0x1a => Some(RouteSource::CueB),
            0x00..=0x0f => Some(RouteSource::Daw(code & !1)),
            _ => None,
        },
        RouteScheme::None => None,
    }
}

/// A stereo output pair the user can route.
#[derive(Clone, Copy, Debug)]
pub struct OutputPair {
    pub name: &'static str,
    /// Routing index of the left half; the right half is `first + 1`.
    pub first: u8,
    /// Sources offered for this pair.
    pub sources: &'static [RouteSource],
    /// Source used when nothing is saved.
    pub default: RouteSource,
}

// ---------------------------------------------------------------------------
// Device profiles
// ---------------------------------------------------------------------------

/// Kind of a mixer input node, used for labels and colors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind {
    Mic,
    Digital,
    Daw,
}

/// One input node of the hardware mixer (one matrix row).
#[derive(Clone, Copy, Debug)]
pub struct MixerNode {
    pub name: &'static str,
    pub kind: NodeKind,
    /// Whether this node starts linked with the next one as a stereo pair.
    pub default_linked: bool,
    /// Zero-based channel of the USB audio stream this node corresponds to:
    /// a capture channel for Mic and Digital nodes (what the computer
    /// records), a playback channel for DAW nodes (what the computer plays).
    pub stream_channel: u8,
}

impl NodeKind {
    /// DAW nodes are fed by the computer; the others are recorded by it.
    pub fn is_playback(self) -> bool {
        self == NodeKind::Daw
    }
}

/// Everything lD Mixer knows about a model.
#[derive(Debug)]
pub struct Profile {
    pub name: &'static str,
    pub product_id: u16,
    /// Protocol confirmed on real hardware; unverified models stay read-only.
    pub verified: bool,
    pub nodes: &'static [MixerNode],
    pub outputs: &'static [OutputPair],
    pub route_scheme: RouteScheme,
    /// Number of routing destinations the firmware accepts.
    pub routing_outputs: u8,
    pub has_alt: bool,
    pub has_talkback: bool,
    pub has_optical: bool,
    /// Analog inputs with a phase switch (channels 1..=n on entity 0x0b).
    pub phase_inputs: u8,
    /// Capture channels (zero-based) carrying the hardware loopback, if any.
    pub loopback_capture: Option<(u8, u8)>,
}

const fn n(name: &'static str, kind: NodeKind, default_linked: bool, stream_channel: u8) -> MixerNode {
    MixerNode { name, kind, default_linked, stream_channel }
}

const ID24_NODES: &[MixerNode] = &[
    n("Mic 1", NodeKind::Mic, false, 0),
    n("Mic 2", NodeKind::Mic, false, 1),
    n("Digi 1", NodeKind::Digital, true, 2),
    n("Digi 2", NodeKind::Digital, false, 3),
    n("Digi 3", NodeKind::Digital, true, 4),
    n("Digi 4", NodeKind::Digital, false, 5),
    n("Digi 5", NodeKind::Digital, true, 6),
    n("Digi 6", NodeKind::Digital, false, 7),
    n("Digi 7", NodeKind::Digital, true, 8),
    n("Digi 8", NodeKind::Digital, false, 9),
    n("DAW 1", NodeKind::Daw, true, 0),
    n("DAW 2", NodeKind::Daw, false, 1),
    n("DAW 3", NodeKind::Daw, true, 2),
    n("DAW 4", NodeKind::Daw, false, 3),
    n("DAW 5", NodeKind::Daw, true, 4),
    n("DAW 6", NodeKind::Daw, false, 5),
];

const ID24_SPEAKER_SOURCES: &[RouteSource] = &[
    RouteSource::Main,
    RouteSource::CueA,
    RouteSource::CueB,
    RouteSource::Daw(0),
    RouteSource::Daw(2),
    RouteSource::Daw(4),
];
const ID24_LINE_SOURCES: &[RouteSource] = &[
    RouteSource::Main,
    RouteSource::Alt,
    RouteSource::CueA,
    RouteSource::CueB,
    RouteSource::Daw(0),
    RouteSource::Daw(2),
    RouteSource::Daw(4),
];
/// The optical output carries DAW 9+10 (its own playback pair) by default.
const ID24_OPTICAL_SOURCES: &[RouteSource] = &[
    RouteSource::Daw(8),
    RouteSource::Main,
    RouteSource::CueA,
    RouteSource::CueB,
    RouteSource::Daw(0),
    RouteSource::Daw(2),
    RouteSource::Daw(4),
];
const ID24_LOOPBACK_SOURCES: &[RouteSource] = &[
    RouteSource::Main,
    RouteSource::CueA,
    RouteSource::CueB,
    RouteSource::Daw(0),
    RouteSource::Daw(2),
    RouteSource::Daw(4),
    RouteSource::Daw(6),
];

const ID24_OUTPUTS: &[OutputPair] = &[
    OutputPair { name: "Speakers (Out 1+2)", first: 0, sources: ID24_SPEAKER_SOURCES, default: RouteSource::Main },
    OutputPair { name: "Line (Out 3+4)", first: 2, sources: ID24_LINE_SOURCES, default: RouteSource::Alt },
    OutputPair { name: "Headphones", first: 4, sources: ID24_SPEAKER_SOURCES, default: RouteSource::CueA },
    OutputPair { name: "Optical 1+2", first: 8, sources: ID24_OPTICAL_SOURCES, default: RouteSource::Daw(8) },
    OutputPair { name: "Loopback", first: 10, sources: ID24_LOOPBACK_SOURCES, default: RouteSource::Main },
];

pub static PROFILES: &[Profile] = &[Profile {
    name: "iD24",
    product_id: 0x000d,
    verified: true,
    nodes: ID24_NODES,
    outputs: ID24_OUTPUTS,
    route_scheme: RouteScheme::Id24,
    routing_outputs: 16,
    has_alt: true,
    has_talkback: true,
    has_optical: true,
    phase_inputs: 2,
    loopback_capture: Some((10, 11)),
}];

/// Other iD models: recognized so the user gets a clear message, but not
/// driven until someone verifies them on hardware.
pub static KNOWN_UNSUPPORTED: &[(u16, &str)] = &[
    (0x0001, "iD22"),
    (0x0002, "iD14"),
    (0x0003, "iD4"),
    (0x0005, "iD44"),
    (0x0008, "iD14 MKII"),
    (0x0009, "iD4 MKII"),
    (0x000b, "iD44 MKII"),
    (0x0012, "iD48"),
];

pub fn profile_for(product_id: u16) -> Option<&'static Profile> {
    PROFILES.iter().find(|p| p.product_id == product_id)
}

pub fn model_name(product_id: u16) -> Option<&'static str> {
    profile_for(product_id)
        .map(|p| p.name)
        .or_else(|| KNOWN_UNSUPPORTED.iter().find(|(id, _)| *id == product_id).map(|(_, n)| *n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_round_trip() {
        assert_eq!(db_to_raw(Some(0.0)), 0);
        assert_eq!(db_to_raw(None), RAW_MUTE);
        assert_eq!(db_to_raw(Some(-200.0)), RAW_MUTE);
        assert_eq!(db_to_raw(Some(-6.0)), -1536);
        assert_eq!(raw_to_db(-1536), Some(-6.0));
        assert_eq!(raw_to_db(RAW_MUTE), None);
    }

    #[test]
    fn fader_taper() {
        assert_eq!(fader_to_db(0.0), None);
        assert_eq!(fader_to_db(1.0), Some(0.0));
        let half = fader_to_db(0.5).unwrap();
        assert!((half + 18.06).abs() < 0.1, "{half}");
        let p = db_to_fader(Some(-24.0));
        assert!((fader_to_db(p).unwrap() + 24.0).abs() < 1e-3);
    }

    #[test]
    fn id24_routing_codes() {
        let s = RouteScheme::Id24;
        assert_eq!(route_code(s, 0, RouteSource::Main), Some(0x25));
        assert_eq!(route_code(s, 1, RouteSource::Main), Some(0x26));
        assert_eq!(route_code(s, 4, RouteSource::CueA), Some(0x1e));
        assert_eq!(route_code(s, 5, RouteSource::CueB), Some(0x21));
        assert_eq!(route_code(s, 3, RouteSource::Alt), Some(0x28));
        assert_eq!(route_code(s, 10, RouteSource::Daw(6)), Some(6));
        assert_eq!(route_code(s, 11, RouteSource::Daw(6)), Some(7));
        assert_eq!(route_source_from_code(s, 0x1f), Some(RouteSource::CueA));
        assert_eq!(route_source_from_code(s, 0x07), Some(RouteSource::Daw(6)));
    }

    #[test]
    fn output_defaults_are_offered() {
        for p in PROFILES {
            for o in p.outputs {
                assert!(o.sources.contains(&o.default), "{} {}", p.name, o.name);
            }
        }
    }

    #[test]
    fn cell_layout() {
        assert_eq!(mixer_cell(0, Bus::Main, 0), 0);
        assert_eq!(mixer_cell(1, Bus::CueA, 1), 9);
        assert_eq!(mixer_cell(15, Bus::CueB, 1), 95);
    }

    /// The udev rule grants access to exactly the models lD Mixer drives.
    #[test]
    fn udev_rule_matches_profiles() {
        let rules = include_str!("../data/70-ldmixer.rules");
        let mut in_rule: Vec<u16> = rules
            .lines()
            .filter(|l| !l.trim_start().starts_with('#') && l.contains("uaccess"))
            .map(|l| {
                assert!(l.contains(&format!("ATTR{{idVendor}}==\"{:04x}\"", VENDOR_ID)), "{l}");
                let id = l.split("ATTR{idProduct}==\"").nth(1).expect("idProduct in rule");
                u16::from_str_radix(&id[..4], 16).unwrap()
            })
            .collect();
        let mut driven: Vec<u16> = PROFILES.iter().map(|p| p.product_id).collect();
        in_rule.sort();
        driven.sort();
        assert_eq!(in_rule, driven);
    }
}

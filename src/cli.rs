//! Command-line subcommands of `ldmixer`: inspection and experiments.
//!
//! Read-only commands (`scan`, `dump`, `snapshot`, `meters`, `watch`) never
//! write to the device. Commands that write (`restore`, `route`, `cell`,
//! `monitor`) require the `--write` flag, so nothing changes by accident.

use std::collections::BTreeMap;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use ldmixer::device::{self, ControlIo};
use ldmixer::protocol::{self, entity, monitor, MIXER_STRIDE};
use serde::{Deserialize, Serialize};

pub const USAGE: &str = "\
Device commands, read-only:
  scan                       list Audient devices on the bus
  dump                       read every known readable control
  snapshot <file.json>       save routing, matrix and monitor state to a file
  meters [offset] [len]      print the GET_MEM meter block live (Ctrl+C stops)
  watch                      print monitor-section changes live (Ctrl+C stops)
  get <entity> <sel> <ch> <len>
                             raw GET_CUR read (hex ok), e.g. get 0x0c 0x02 1 2

Writes (need --write):
  restore <file.json>        write a snapshot back to the device
  route <out> <code>         write a raw routing code (hex ok: 0x25)
  cell <cell> <dB|mute>      write one mixer crosspoint
  monitor <selector> <value> write one monitor-section byte
  level <entity> <sel> <ch> <dB|mute>
                             write an int16 level, e.g. level 0x36 0x12 0 -30
";

#[derive(Serialize, Deserialize, Default)]
struct Snapshot {
    model: String,
    routing: BTreeMap<u8, u8>,
    matrix: BTreeMap<usize, i16>,
    monitor_u8: BTreeMap<u8, u8>,
    monitor_i16: BTreeMap<u8, i16>,
}

const MONITOR_BYTES: &[(u8, &str)] = &[
    (monitor::MONO, "mono"),
    (monitor::MONO_MODE, "mono mode"),
    (monitor::POLARITY, "polarity"),
    (monitor::CUT, "cut"),
    (monitor::DIM, "dim"),
    (monitor::TALKBACK, "talkback"),
    (monitor::TALKBACK_SOURCE, "talkback source"),
    (monitor::ALT, "alt"),
];
const MONITOR_LEVELS: &[(u8, &str)] = &[(monitor::VOLUME, "volume"), (monitor::DIM_LEVEL, "dim level")];

fn parse_num(s: &str) -> Result<i64> {
    let v = if let Some(h) = s.strip_prefix("0x") {
        i64::from_str_radix(h, 16)?
    } else {
        s.parse()?
    };
    Ok(v)
}

type Device = dyn ControlIo;

/// The device, through the daemon when it runs (it owns the USB
/// connection), directly otherwise.
fn open(mock: bool) -> Result<Box<Device>> {
    if ldmixer::ipc::daemon_running() {
        return Ok(Box::new(ldmixer::ipc::RemoteIo::connect()?));
    }
    device::open(mock)
}

/// Run a device subcommand. `args[0]` is the command name.
pub fn run(mut args: Vec<String>, mock: bool) -> Result<()> {
    let write = if let Some(i) = args.iter().position(|a| a == "--write") {
        args.remove(i);
        true
    } else {
        false
    };
    let Some(cmd) = args.first().cloned() else {
        print!("{USAGE}");
        return Ok(());
    };
    let rest = &args[1..];
    let needs_write = matches!(cmd.as_str(), "restore" | "route" | "cell" | "monitor" | "level");
    if needs_write && !write {
        bail!("`{cmd}` writes to the device; add --write to confirm");
    }
    match cmd.as_str() {
        "scan" => scan(),
        "dump" => dump(&*open(mock)?),
        "snapshot" => {
            let path = rest.first().context("snapshot needs a file name")?;
            let snap = take_snapshot(&*open(mock)?);
            std::fs::write(path, serde_json::to_string_pretty(&snap)?)?;
            println!(
                "saved {} routing, {} matrix, {} monitor values to {path}",
                snap.routing.len(),
                snap.matrix.len(),
                snap.monitor_u8.len() + snap.monitor_i16.len()
            );
            Ok(())
        }
        "restore" => {
            let path = rest.first().context("restore needs a file name")?;
            let snap: Snapshot = serde_json::from_str(&std::fs::read_to_string(path)?)?;
            restore(&*open(mock)?, &snap)
        }
        "meters" => {
            let off = rest.first().map(|s| parse_num(s)).transpose()?.unwrap_or(0) as u16;
            let len = rest.get(1).map(|s| parse_num(s)).transpose()?.unwrap_or(32) as usize;
            meters(&*open(mock)?, off, len)
        }
        "watch" => watch(&*open(mock)?),
        "level" => {
            let usage = "level <entity> <selector> <channel> <dB|mute>";
            let n: Vec<i64> = rest.iter().take(3).map(|s| parse_num(s)).collect::<Result<_>>()?;
            let [ent, sel, ch] = n[..] else { bail!(usage) };
            let v = rest.get(3).context(usage)?;
            let db = if v == "mute" { None } else { Some(v.parse::<f32>()?) };
            open(mock)?.set_i16(ent as u8, sel as u8, ch as u8, protocol::db_to_raw(db))?;
            println!("{ent:#04x}/{sel:#04x}/{ch} <- {}", protocol::format_db(db));
            Ok(())
        }
        "get" => {
            let n: Vec<i64> = rest.iter().map(|s| parse_num(s)).collect::<Result<_>>()?;
            let [ent, sel, ch, len] = n[..] else { bail!("get <entity> <selector> <channel> <len>") };
            let b = open(mock)?.get(ent as u8, sel as u8, ch as u8, len as usize)?;
            let extra = if b.len() == 2 {
                let raw = i16::from_le_bytes([b[0], b[1]]);
                format!("  (int16 {raw} = {})", protocol::format_db(protocol::raw_to_db(raw)))
            } else {
                String::new()
            };
            println!("{}{extra}", hex(&b));
            Ok(())
        }
        "route" => {
            let out = parse_num(rest.first().context("route <out> <code>")?)? as u8;
            let code = parse_num(rest.get(1).context("route <out> <code>")?)? as u8;
            let d = open(mock)?;
            d.set_route(out, code)?;
            println!("output {out} <- code {code:#04x}");
            Ok(())
        }
        "cell" => {
            let cell = parse_num(rest.first().context("cell <n> <dB|mute>")?)? as usize;
            let v = rest.get(1).context("cell <n> <dB|mute>")?;
            let db = if v == "mute" { None } else { Some(v.parse::<f32>()?) };
            let d = open(mock)?;
            d.set_mixer_cell(cell, protocol::db_to_raw(db))?;
            println!("cell {cell} <- {}", protocol::format_db(db));
            Ok(())
        }
        "monitor" => {
            let sel = parse_num(rest.first().context("monitor <selector> <value>")?)? as u8;
            let v = parse_num(rest.get(1).context("monitor <selector> <value>")?)? as u8;
            open(mock)?.set_u8(entity::MONITOR, sel, 0, v)?;
            println!("monitor selector {sel:#04x} <- {v}");
            Ok(())
        }
        _ => bail!("unknown command `{cmd}`; see `ldmixer help`"),
    }
}

fn scan() -> Result<()> {
    let found = device::scan()?;
    if found.is_empty() {
        println!("no Audient devices found");
    }
    for f in found {
        println!(
            "bus {:03} dev {:03}  2708:{:04x}  {}  {}",
            f.bus,
            f.address,
            f.product_id,
            f.model.unwrap_or("unknown model"),
            if f.supported { "(supported)" } else { "(not supported yet)" }
        );
    }
    Ok(())
}

fn show<T: std::fmt::Display>(label: &str, r: Result<T>) {
    match r {
        Ok(v) => println!("  {label:<18} {v}"),
        Err(e) => println!("  {label:<18} <{e}>"),
    }
}

fn dump(d: &Device) -> Result<()> {
    let info = d.info();
    println!(
        "{} on interface {}, firmware {:#06x}, serial {}{}",
        info.profile.name,
        info.interface,
        info.firmware,
        info.serial.as_deref().unwrap_or("?"),
        if info.mock { " (mock)" } else { "" }
    );
    println!("system:");
    show("sample rate", d.sample_rate().map(|v| format!("{v} Hz")));
    show("optical in", d.optical_mode(false).map(|v| format!("{v} (0=ADAT 1=S/PDIF)")));
    show("optical out", d.optical_mode(true).map(|v| format!("{v} (0=ADAT 1=S/PDIF)")));
    println!("monitor:");
    for (sel, name) in MONITOR_BYTES {
        show(name, d.get_u8(entity::MONITOR, *sel, 0));
    }
    for (sel, name) in MONITOR_LEVELS {
        show(
            name,
            d.get_i16(entity::MONITOR, *sel, 0)
                .map(|r| format!("{r:#06x} = {}", protocol::format_db(protocol::raw_to_db(r)))),
        );
    }
    println!("routing (output: code -> source):");
    for out in 0..d.profile().routing_outputs {
        show(
            &format!("out {out:2}"),
            d.get_route(out).map(|c| {
                let src = protocol::route_source_from_code(d.profile().route_scheme, c);
                format!("{c:#04x} {}", src.map(|s| s.label()).unwrap_or_else(|| "?".into()))
            }),
        );
    }
    println!("matrix (dB; rows = nodes, columns = MainL MainR CueAL CueAR CueBL CueBR):");
    for (i, node) in d.profile().nodes.iter().enumerate() {
        let cells: Vec<String> = (0..MIXER_STRIDE)
            .map(|c| match d.get_mixer_cell(i * MIXER_STRIDE + c) {
                Ok(r) => match protocol::raw_to_db(r) {
                    None => "  mute".into(),
                    Some(db) => format!("{db:6.1}"),
                },
                Err(_) => "   err".into(),
            })
            .collect();
        println!("  {:<8} {}", node.name, cells.join(" "));
    }
    println!("meters:");
    show("input block", d.get_mem(entity::MIXER, 0, 32).map(|b| hex(&b)));
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join(" ")
}

fn take_snapshot(d: &Device) -> Snapshot {
    let mut s = Snapshot { model: d.profile().name.into(), ..Default::default() };
    for out in 0..d.profile().routing_outputs {
        if let Ok(c) = d.get_route(out) {
            s.routing.insert(out, c);
        }
    }
    for cell in 0..d.profile().nodes.len() * MIXER_STRIDE {
        if let Ok(v) = d.get_mixer_cell(cell) {
            s.matrix.insert(cell, v);
        }
    }
    for (sel, _) in MONITOR_BYTES {
        if let Ok(v) = d.get_u8(entity::MONITOR, *sel, 0) {
            s.monitor_u8.insert(*sel, v);
        }
    }
    for (sel, _) in MONITOR_LEVELS {
        if let Ok(v) = d.get_i16(entity::MONITOR, *sel, 0) {
            s.monitor_i16.insert(*sel, v);
        }
    }
    s
}

fn restore(d: &Device, s: &Snapshot) -> Result<()> {
    if s.model != d.profile().name {
        bail!("snapshot is for {}, device is {}", s.model, d.profile().name);
    }
    let mut errors = 0;
    for (out, code) in &s.routing {
        errors += d.set_route(*out, *code).is_err() as usize;
    }
    for (cell, v) in &s.matrix {
        errors += d.set_mixer_cell(*cell, *v).is_err() as usize;
    }
    for (sel, v) in &s.monitor_u8 {
        errors += d.set_u8(entity::MONITOR, *sel, 0, *v).is_err() as usize;
    }
    for (sel, v) in &s.monitor_i16 {
        errors += d.set_i16(entity::MONITOR, *sel, 0, *v).is_err() as usize;
    }
    println!("restored with {errors} failed writes");
    Ok(())
}

fn meters(d: &Device, off: u16, len: usize) -> Result<()> {
    loop {
        match d.get_mem(entity::MIXER, off, len) {
            Ok(b) => println!("{}", hex(&b)),
            Err(e) => println!("<{e}>"),
        }
        sleep(Duration::from_millis(200));
    }
}

fn watch(d: &Device) -> Result<()> {
    let start = Instant::now();
    let mut last: BTreeMap<u8, Vec<u8>> = BTreeMap::new();
    println!("watching monitor entity 0x36 selectors 0x00..0x20; turn knobs or press buttons");
    loop {
        for sel in 0u8..0x20 {
            let v = d.get(entity::MONITOR, sel, 0, 2).or_else(|_| d.get(entity::MONITOR, sel, 0, 1));
            let Ok(v) = v else { continue };
            if last.get(&sel) != Some(&v) {
                if last.contains_key(&sel) {
                    println!("{:8.2}s  sel {sel:#04x}: {}", start.elapsed().as_secs_f32(), hex(&v));
                }
                last.insert(sel, v);
            }
        }
        sleep(Duration::from_millis(150));
    }
}

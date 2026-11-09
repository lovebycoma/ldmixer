//! `ld-mixer`: the single command of lD Mixer.
//!
//! `ld-mixer daemon` runs the background service (device, mix, virtual
//! devices, tray), `ld-mixer gui` opens the mixer window (starting the
//! service if needed), `ld-mixer uninstall` removes lD Mixer, and the
//! remaining subcommands inspect or probe the device.

mod cli;
mod gui;

use std::process::ExitCode;

/// The uninstaller, embedded so `ld-mixer uninstall` works however lD Mixer
/// was installed.
const UNINSTALL_SCRIPT: &str = include_str!("../data/uninstall.sh");

fn help() {
    println!(
        "lD Mixer {} - mixer for Audient iD interfaces (lD = Linux iD)

Usage: ld-mixer <command> [options]

  gui                        open the mixer window (starts the service if needed)
  daemon                     run the background service in this terminal
  start                      start the background service in the background
  stop                       stop the background service (and its devices)
  status                     show the service, device and virtual devices
  uninstall [-y] [--keep-user-data]
                             remove lD Mixer and its settings
  help                       show this help
  version                    show the version

{}
Global options:
  --mock                     use a simulated iD24 instead of the hardware",
        env!("CARGO_PKG_VERSION"),
        cli::USAGE
    );
}

fn uninstall(args: &[String]) -> ExitCode {
    let exe = std::env::current_exe().unwrap_or_default();
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(UNINSTALL_SCRIPT)
        .arg("ld-mixer uninstall")
        .args(args)
        .env("LDMIXER_SELF", exe)
        .status();
    match status {
        Ok(s) if s.success() => ExitCode::SUCCESS,
        Ok(s) => ExitCode::from(s.code().unwrap_or(1).clamp(1, 255) as u8),
        Err(e) => {
            eprintln!("ld-mixer: cannot run the uninstaller: {e}");
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mock = if let Some(i) = args.iter().position(|a| a == "--mock") {
        args.remove(i);
        true
    } else {
        false
    };
    if !matches!(args.first().map(String::as_str), Some("gui" | "daemon")) {
        // Exit quietly when output is piped into `head` and the like. The
        // GUI and the service keep Rust's default (ignore SIGPIPE), since a
        // client hanging up must not kill them.
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    }
    match args.first().map(String::as_str) {
        None | Some("help" | "-h" | "--help") => {
            help();
            ExitCode::SUCCESS
        }
        Some("version" | "-V" | "--version") => {
            println!("ld-mixer {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("gui") => {
            if gui::run(mock) == gtk::glib::ExitCode::SUCCESS {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Some("uninstall") => uninstall(&args[1..]),
        Some("daemon") => report(ld_mixer::daemon::run(mock)),
        Some("start") => report(start(mock)),
        Some("stop") => report(stop()),
        Some("status") => report(status()),
        Some(_) => match cli::run(args, mock) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("ld-mixer: {e:#}");
                ExitCode::FAILURE
            }
        },
    }
}

fn report(r: anyhow::Result<()>) -> ExitCode {
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("ld-mixer: {e:#}");
            ExitCode::FAILURE
        }
    }
}

fn start(mock: bool) -> anyhow::Result<()> {
    use ld_mixer::ipc;
    if ipc::daemon_running() {
        println!("lD Mixer is already running");
        return Ok(());
    }
    ipc::connect_or_spawn(false, mock)?;
    println!("lD Mixer started");
    Ok(())
}

fn stop() -> anyhow::Result<()> {
    use ld_mixer::ipc::{self, Connection, Notice, Request};
    if !ipc::daemon_running() {
        println!("lD Mixer is not running");
        return Ok(());
    }
    let mut c = Connection::connect(false)?;
    c.send(&Request::Shutdown)?;
    // Wait until the daemon has closed the socket.
    while let Ok(Some(n)) = c.recv() {
        if matches!(n, Notice::Quitting) {
            break;
        }
    }
    for _ in 0..30 {
        if !ipc::daemon_running() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    println!("lD Mixer stopped");
    Ok(())
}

fn status() -> anyhow::Result<()> {
    use ld_mixer::ipc::{self, Connection, Notice};
    if !ipc::daemon_running() {
        println!("service: not running (start it with `ld-mixer start` or `ld-mixer gui`)");
        return Ok(());
    }
    // A GUI hello returns the full picture, then we hang up.
    let mut c = Connection::connect(true)?;
    println!("service: running");
    let mut got_vdevs = false;
    while !got_vdevs {
        match c.recv()? {
            Some(Notice::Device(Some(d), _)) => println!(
                "device:  {} (firmware {:x}.{:02x}{})",
                d.model,
                d.firmware >> 8,
                d.firmware & 0xff,
                if d.mock { ", mock" } else { "" }
            ),
            Some(Notice::Device(None, reason)) => {
                println!("device:  not connected ({})", reason.unwrap_or_default())
            }
            Some(Notice::VirtualDevices(list)) => {
                got_vdevs = true;
                if list.is_empty() {
                    println!("virtual devices: none");
                }
                for v in list {
                    let kind = if v.spec.playback { "output" } else { "input" };
                    let state = match (&v.error, v.running) {
                        (Some(e), _) => format!("error: {e}"),
                        (None, true) => "running".into(),
                        (None, false) => "starting".into(),
                    };
                    println!("  {:<24} {kind:<6} {state}", v.spec.description);
                }
            }
            Some(_) => {}
            None => break,
        }
    }
    Ok(())
}

//! `ld-mixer`: the single command of lD Mixer.
//!
//! `ld-mixer gui` opens the mixer window, `ld-mixer uninstall` removes lD
//! Mixer, and the remaining subcommands inspect or probe the device.

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

  gui                        open the mixer window
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
        Some(_) => match cli::run(args, mock) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("ld-mixer: {e:#}");
                ExitCode::FAILURE
            }
        },
    }
}

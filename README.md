# lD Mixer

![lD Mixer next to pavucontrol showing its virtual output devices](docs/screenshots/main.avif)

An unofficial Linux replacement for Audient's iD Mixer (**lD** = **L**inux **iD**).

- Main, Cue A and Cue B mixes, the monitor section, output routing, the hardware loopback, meters and presets.
- Audio keeps playing, with no kernel module or driver swap.
- Optional virtual inputs and outputs per strip (for example a "DAW 3+4" speaker just for Discord), which exist only while lD Mixer runs.
- A background service with a tray icon, so the mix keeps working after the window closes.

> [!WARNING]
> **Made with AI, not reviewed by a person.** lD Mixer was built with the help of AI (roughly 90% "vibe coding") and has **not had a proper human code review**. It talks to your interface with unofficial, reverse-engineered commands. **Use it at your own risk**: the author takes no responsibility for problems it causes, including to your hardware, your hearing or your speakers. Keep monitor levels low when trying something new. Not affiliated with Audient.

## Supported hardware

The goal is to support the whole Audient iD family, but the only interface I own is an iD24, so that is the only model I could build and test against. For now only the **iD24** is driven. Other iD models are recognized but not controlled, so nothing is sent to them blindly. If you own another iD model and want to help verify it, please open an issue.

Tested on (everything else is untested):

- Interface: Audient iD24, firmware 1.12
- Distribution: Artix Linux with the CachyOS kernel
- Audio: PipeWire, WirePlumber, EasyEffects
- Desktop: niri (Wayland), quickshell tray, adw-gtk3 theme

Requirements: GTK ≥ 4.18, libadwaita ≥ 1.7, libusb 1.0, Rust ≥ 1.85, and the PipeWire command-line tools for virtual devices.

## Install

```sh
git clone https://github.com/lovebycoma/ldmixer.git && cd ldmixer
```

Arch and derivatives:

```sh
cd packaging/arch
makepkg -si
```

Fedora:

```sh
sudo dnf install cargo rust make rpm-build 'pkgconfig(gtk4)' 'pkgconfig(libadwaita-1)' 'pkgconfig(libusb-1.0)'
packaging/fedora/build-rpm.sh
sudo dnf install packaging/fedora/out/RPMS/*/ldmixer-*.rpm
```

Nix:

```sh
nix profile install .
```

On NixOS, also enable the udev rule in your configuration:

```nix
imports = [ ldmixer.nixosModules.default ];   # ldmixer = this flake
programs.ldmixer.enable = true;
```

Anything else (on Debian/Ubuntu first install `libgtk-4-dev libadwaita-1-dev libusb-1.0-0-dev`):

```sh
make
sudo make install
```

After installing, **replug the interface once** so the udev rule applies.

## Usage

```sh
ldmixer gui          # open the window (or use the application menu)
ldmixer gui --mock   # try the UI without hardware
ldmixer status       # service, interface and virtual devices
ldmixer stop         # stop the background service
ldmixer help         # all commands
```

- The first start reads the mix the interface already has. After that, lD Mixer restores its own saved mix on connect.
- Closing the window keeps the service running in the tray. Quit from the tray, the window menu, or `ldmixer stop`.
- Mixes: Ctrl+1/2/3 switches Main, Cue A, Cue B. Click any dB or pan readout to type a value (`-12`, `-inf`, `C`, `L30`). Right-click a strip to rename or hide it. Click a mix's color dot to change its color, or the pencil to rename it.
- Virtual devices: press 🎤/🔈 on a strip. Inputs and the loopback need the card profile **Multichannel Duplex** or **Pro Audio**.
- Command-line inspection (`scan`, `dump`, `snapshot`, `meters`, `watch`, `get`) only reads. Writing commands need `--write`.

## What it changes on your system

- **Installed files:** the `ldmixer` binary, a desktop entry, icon, metainfo, license, an uninstall manifest, and one udev rule.
- **The udev rule** (`70-ldmixer.rules`) lets the user logged in at the seat open the iD24, and no other device. Linux otherwise only lets root open it:

  ```
  SUBSYSTEM=="usb", ENV{DEVTYPE}=="usb_device", ATTR{idVendor}=="2708", ATTR{idProduct}=="000d", TAG+="uaccess"
  ```

- **Your home folder:** `~/.config/ldmixer/` (mix, settings, presets) and `~/.local/state/ldmixer/daemon.log`.
- **While running:** the background service, a socket in `$XDG_RUNTIME_DIR`, and one `pw-loopback` process per virtual device.
- **The interface** keeps the mix and routing it was sent, even after a power cycle. Use **Menu → Reset Mixer…** before uninstalling if you want defaults back.
- **Never touched:** PipeWire, WirePlumber and ALSA configuration, sample rate and quantum, default devices, kernel modules, groups, system services.

## Uninstall

```sh
ldmixer uninstall    # -y skips the prompt, --keep-user-data keeps ~/.config/ldmixer
```

It stops the service, deletes lD Mixer's files in your home folder, and removes the package through pacman, dnf, Nix or the `make install` manifest (including the udev rule). Left behind: settings stored in the interface, your clone, build tools, and on NixOS the `programs.ldmixer` line.

## Troubleshooting

- **"permission denied opening the iD24":** replug the interface after installing. On NixOS, enable `programs.ldmixer`.
- **"in use by another program":** another lD Mixer, BiD, Monix or MixiD instance holds it. Check `ldmixer status`.
- **No sound:** the front-panel volume knob first, then faders, the mix master and **Outputs** routing.
- **Virtual input error:** set the card profile to Multichannel Duplex or Pro Audio, and install `pw-loopback` and `pw-dump`.
- **Anything else:** read `~/.local/state/ldmixer/daemon.log` (the previous run's log is `daemon.log.old`), or run `ldmixer daemon` in a terminal.

## Development

`cargo test`, `cargo run -- gui --mock`. See [docs/PROTOCOL.md](docs/PROTOCOL.md) for the USB protocol and [AGENTS.md](AGENTS.md) for architecture and project rules.

## Issues and pull requests

Issues and pull requests are open, but this is a personal side project and I do not want to put a lot of time into it. I may not have time to look after them, so replies and merges can be slow, or may not happen at all.

## Credits

lD Mixer would not exist without these open-source projects, whose developers worked out Audient's undocumented USB protocol and shared their findings. A big thank you to all of them:

- [MixiD](https://github.com/TheOnlyJoey/MixiD)
- [BiD](https://github.com/baakhoff/BiD)
- [Monix](https://github.com/sKuhLight/monix)
- [audient-id24-dkms](https://github.com/Magnetar-OS/audient-id24-dkms)

## License

MIT, see [LICENSE](LICENSE).

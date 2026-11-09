# lD Mixer

A mixer and control panel for **Audient iD** USB audio interfaces on Linux,
modeled on Audient's own iD Mixer app. The name is a pun:
**lD** = **L**inux **iD**.

[한국어 README](README.ko.md)

- Main, Cue A and Cue B mixes, the monitor section (volume, dim, cut, alt,
  talkback, mono, polarity), output routing, the hardware loopback, meters
  and presets.
- Audio keeps playing while it runs. No kernel module or driver swap.
- Optional virtual inputs and outputs for other apps (for example a
  "DAW 3+4" speaker just for Discord), which exist only while lD Mixer runs.
- A background service with a tray icon, so the mix keeps working after you
  close the window.

> **Status:** unofficial and not affiliated with Audient. Tested on an
> **iD24**. Other iD models are recognized but not controlled yet.

---

## Contents

1. [Requirements](#requirements)
2. [Install](#install)
3. [First start](#first-start)
4. [What lD Mixer changes on your system](#what-ldmixer-changes-on-your-system)
5. [Uninstall](#uninstall)
6. [Using lD Mixer](#using-ldmixer)
7. [Troubleshooting](#troubleshooting)
8. [For developers](#for-developers)

---

## Requirements

| What | Version | Why |
|------|---------|-----|
| An Audient iD24 | | the interface being controlled |
| Linux with GTK 4 and libadwaita | GTK ≥ 4.18, libadwaita ≥ 1.7 | the window |
| libusb | 1.0 | talking to the interface |
| Rust (cargo) | ≥ 1.85 (edition 2024) | building |
| PipeWire with its command-line tools | any recent | only for virtual inputs/outputs and the loopback input |

Most current distributions (Arch and derivatives, Fedora 42+, NixOS
unstable) meet these. Older LTS releases may ship a libadwaita that is too
old.

---

## Install

Every method installs the **same files** (listed in
[What lD Mixer changes](#what-ldmixer-changes-on-your-system)) and can be
removed with one command (see [Uninstall](#uninstall)).

### 1. Get the source

```sh
git clone https://github.com/lovebycoma/ldmixer.git
cd ldmixer
```

### 2. Build and install for your distribution

**Arch Linux, Manjaro, EndeavourOS, CachyOS, Artix (makepkg)**

```sh
cd packaging/arch
makepkg -si
```

`-s` installs the build tools it needs (cargo, make, …), `-i` installs the
finished package with pacman. The package is built from your clone's
committed state.

**Fedora**

```sh
sudo dnf install cargo rust make rpm-build \
    'pkgconfig(gtk4)' 'pkgconfig(libadwaita-1)' 'pkgconfig(libusb-1.0)'
packaging/fedora/build-rpm.sh
sudo dnf install packaging/fedora/out/RPMS/*/ldmixer-*.rpm
```

**Nix / NixOS**

```sh
nix profile install .
```

On NixOS, USB access needs the udev rule enabled in your configuration:

```nix
imports = [ ldmixer.nixosModules.default ];   # ldmixer = this flake
programs.ldmixer.enable = true;
```

**Anything else (Debian, Ubuntu, openSUSE, …)**

Install Rust, make, pkg-config and the development files for GTK 4,
libadwaita and libusb (on Debian/Ubuntu: `libgtk-4-dev libadwaita-1-dev
libusb-1.0-0-dev`), then:

```sh
make
sudo make install            # installs under /usr/local
```

### 3. Unplug and replug the interface (once)

The install adds a rule that lets your user open the interface. It applies
to devices plugged in after it is installed, so replug the iD24 once (or
reboot).

---

## First start

```sh
ldmixer gui
```

or open **lD Mixer** from your application menu.

- The first start **reads the mix the interface already has** instead of
  overwriting it. After that, lD Mixer restores its own saved mix whenever
  the interface is plugged in.
- Closing the window keeps lD Mixer running in the background (tray icon).
  To quit completely, use the tray menu's **Quit**, the window menu's
  **Quit lD Mixer Completely**, or `ldmixer stop`.
- For inputs and the loopback to reach other apps, set the interface's
  PipeWire profile to **Multichannel Duplex** (or **Pro Audio**) in your
  sound settings. Most systems already use Duplex.

No hardware at hand? `ldmixer gui --mock` shows the whole UI with a
simulated iD24.

---

## What lD Mixer changes on your system

lD Mixer was built so that it **does not change system settings**. This is
the complete list of what it adds or touches.

### Files the package installs (system-wide)

| Path | What it is |
|------|------------|
| `/usr/bin/ldmixer` | the program (the only command) |
| `/usr/share/applications/com.lovebycoma.ldmixer.desktop` | application menu entry |
| `/usr/share/icons/hicolor/scalable/apps/com.lovebycoma.ldmixer.svg` | icon |
| `/usr/share/metainfo/com.lovebycoma.ldmixer.metainfo.xml` | app description for software centers |
| `/usr/share/licenses/ldmixer/LICENSE` | license |
| `/usr/share/ldmixer/manifest` | list of these files, used by the uninstaller |
| `/usr/lib/udev/rules.d/70-ldmixer.rules` | **the only rule that affects the system**, see below |

With `sudo make install` the same files go under `/usr/local`, and the udev
rule goes to `/etc/udev/rules.d/`.

**The udev rule.** Linux only lets root open USB devices by default. This
one-line rule gives the user logged in at the computer access to devices
from Audient (USB vendor `2708`) and nothing else:

```
SUBSYSTEM=="usb", ATTRS{idVendor}=="2708", TAG+="uaccess"
```

The package manager reloads udev when the rule is installed or removed.

### Files lD Mixer creates in your home folder

| Path | What it is |
|------|------------|
| `~/.config/ldmixer/iD24.json` | your mix (faders, routing, names, …) |
| `~/.config/ldmixer/settings.json` | app settings (for example the loopback input) |
| `~/.config/ldmixer/presets/` | presets you save |
| `~/.local/state/ldmixer/daemon.log` | background service log |

### Only while lD Mixer runs (gone when it stops)

| What | Details |
|------|---------|
| Background service `ldmixer daemon` | holds the USB connection, shows the tray icon |
| `$XDG_RUNTIME_DIR/ldmixer.sock` | local socket the window and commands use to reach the service (`/run/user/<uid>/`, readable only by you) |
| Virtual devices | each is a `pw-loopback` process; the device disappears with the process. **No PipeWire or WirePlumber configuration is written.** |

### On the interface itself

lD Mixer sends the interface its mix, routing and monitor settings over
USB, just as Audient's app does. **The interface keeps them** until they
are changed again, and some, such as output routing, survive a power cycle.
Uninstalling lD Mixer does not change them back. To return the mixer to
lD Mixer's defaults first, use **Menu → Reset Mixer…** (only DAW 1+2 plays).

### Never touched

PipeWire, WirePlumber and ALSA configuration, the sample rate and the
quantum (only displayed), your default input and output devices, kernel
modules, user groups and system services.

---

## Uninstall

One command removes lD Mixer however it was installed:

```sh
ldmixer uninstall
```

It shows what it will remove and asks before doing anything (`-y` skips the
question, `--keep-user-data` keeps your settings).

**What it removes**

1. Stops the background service, which also removes all virtual devices.
2. Deletes your lD Mixer files: `~/.config/ldmixer/`,
   `~/.local/state/ldmixer/`, `~/.cache/ldmixer/` and an autostart entry if
   one exists.
3. Removes the program through whatever installed it:
   `pacman -Rns ldmixer`, `dnf remove ldmixer`,
   `nix profile remove ldmixer`, or the files listed in the manifest for
   `make install`. This also deletes the udev rule and reloads udev.

It asks for your password for step 3.

**What stays (and how to remove it)**

| Left behind | How to remove it |
|-------------|------------------|
| Settings stored **in the interface** | run **Reset Mixer** before uninstalling, or reconfigure it with any other tool |
| The `ldmixer` folder you cloned | delete it: `rm -rf ldmixer` |
| Build tools installed by `makepkg -s` or `dnf install` (cargo, …) | remove them with your package manager if you do not need them |
| On NixOS, the `programs.ldmixer` lines | remove them from your configuration and rebuild |

The per-distribution commands (`sudo pacman -Rns ldmixer`,
`sudo dnf remove ldmixer`, `nix profile remove ldmixer`,
`sudo make uninstall`) also work, but they leave `~/.config/ldmixer/` in
place.

---

## Using lD Mixer

### The window

```
┌───────────────────────── channel strips ─────────────────────────┬── Mixes ──┬─ Monitor ─┐
│ Mic 1 │ Mic 2 │ Digi 1+2 │ … │ DAW 1+2 │ DAW 3+4 │ DAW 5+6       │ Main Mix  │ volume    │
│ Ø 🔗 🎤                                                          │ Cue A     │ DIM CUT … │
│ M S                                                              │ Cue B     │ outputs   │
│ pan                                                              │ (knob)    │ system    │
│ -12.0 dB                                                         │           │           │
│ meter │ fader                                                    │           │           │
└──────────────────────────────────────────────────────────────────┴───────────┴───────────┘
```

- **Mixes.** Open Main, Cue A or Cue B (or press Ctrl+1/2/3); the strips
  then show that mix. The open mix has its master level as a knob, shows
  which outputs play it, and can be renamed with the pencil.
- **Strips**, top to bottom:
  - **Ø** polarity (mic inputs), **🔗** link two channels as a stereo pair,
    **🎤/🔈** offer the strip to other apps as its own input or output
  - **M / S** mute and solo in the current mix
  - **pan**: a linked pair has one per channel (L100 / R100 when linked,
    both centered when unlinked)
  - **level** and **meter + fader**
- **Typing values.** Click any dB or pan number and type: `-12`,
  `-6.5 dB`, `-inf`, `C`, `L30`, `R45`.
- **Right-click a strip** to rename or hide it. Hidden strips come back from
  **Menu → Hidden Channels**. Hiding does not change the sound.
- **Monitor column.** Monitor volume and buttons follow the front panel.
  **Outputs** picks what each output plays (main mix, a cue, a DAW pair) and
  what goes to the **loopback**. **System** shows the device, sample rate and
  PipeWire quantum, and sets the optical ports to ADAT or S/PDIF.
- **Presets** are in the main menu.

### Virtual inputs and outputs for other apps

PipeWire shows the iD24 as one 16-channel output and one 12-channel input,
which most apps cannot use channel by channel. Press **🎤/🔈** on a strip and
lD Mixer adds a separate device while it runs:

- **Mic / Digi strips** become inputs, for example "iD24 Mic 1".
- **DAW strips** become outputs, for example "iD24 DAW 3+4".
- A linked strip becomes one stereo device, an unlinked strip a mono one.
- A renamed strip keeps its name ("iD24 Discord").
- **Loopback as App Input** (Outputs section) adds "iD24 Loopback".

Pick these devices in each app, or in your sound settings to make one the
system default. lD Mixer never changes your default device itself.

### Background service and tray

```sh
ldmixer start      # start the background service
ldmixer status     # service, interface and virtual devices
ldmixer stop       # stop it (virtual devices disappear)
ldmixer daemon     # run it in the foreground, e.g. from an autostart entry
```

The tray icon opens the window and has Dim, Cut, Alt, Mono, Talkback and
Quit. It needs a tray (KDE Plasma, most panels, Waybar; on GNOME the
AppIndicator extension). Without one, everything else still works.

### Command line

`ldmixer help` lists everything. Inspection commands only read:
`scan`, `dump`, `snapshot <file>`, `meters`, `watch`, `get`. Commands that
write (`restore`, `route`, `cell`, `monitor`, `level`) need `--write`. While
the service runs, they go through it.

---

## Troubleshooting

| Symptom | What to check |
|---------|---------------|
| "permission denied opening the iD24" | Replug the interface once after installing. On NixOS enable `programs.ldmixer`. |
| "the iD24 is in use by another program" | Another lD Mixer or BiD/Monix/MixiD instance holds it; `ldmixer status`, `ldmixer stop`. |
| No sound | The interface's front volume knob; the strip's fader and the mix master; **Outputs** routes your speakers to the right mix; the app plays to the iD24 (or to the virtual device you picked). |
| A virtual input shows an error | Set the card's profile to Multichannel Duplex or Pro Audio; make sure `pw-loopback` and `pw-dump` are installed (PipeWire tools). |
| No tray icon | Your desktop has no tray host; use `ldmixer gui` and `ldmixer stop`. |
| Something else | `~/.local/state/ldmixer/daemon.log`, or run `ldmixer daemon` in a terminal. |

---

## For developers

```sh
cargo build                   # debug build
cargo test                    # unit tests
cargo test --test vdev_live -- --ignored   # live PipeWire test (needs an iD interface)
cargo run -- gui --mock       # UI against a simulated iD24
```

- [docs/PROTOCOL.md](docs/PROTOCOL.md): the USB protocol and what was
  measured on hardware.
- [AGENTS.md](AGENTS.md): architecture, conventions and project rules.

## Credits

The protocol knowledge builds on
[MixiD](https://github.com/TheOnlyJoey/MixiD),
[BiD](https://github.com/baakhoff/BiD),
[Monix](https://github.com/sKuhLight/monix) and
[audient-id24-dkms](https://github.com/Magnetar-OS/audient-id24-dkms).

## License

MIT, see [LICENSE](LICENSE).

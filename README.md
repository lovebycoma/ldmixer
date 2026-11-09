# lD Mixer

An unofficial mixer and control panel for Audient iD USB audio interfaces on
Linux. The name is a pun: **l**D Mixer = **L**inux iD Mixer.

Work in progress. Developed and tested first on an Audient iD24.

- Main, Cue A and Cue B mixes, output routing and hardware loopback, the monitor
  section (volume, dim, cut, alt, talkback, mono, polarity) and meters.
- Audio keeps playing while lD Mixer is connected: it talks to the device
  through its spare DFU interface, so no kernel module or driver swap is needed.
- lD Mixer never changes system settings (PipeWire, ALSA, sample rate). The
  only system file it ships is a udev rule that lets you open the device.

## Install

Every package installs the same files: the `ld-mixer` command, a desktop
entry, an icon and the udev rule `70-ld-mixer.rules`.

**Arch Linux and derivatives (makepkg)**

```sh
cd packaging/arch
makepkg -si
```

**Fedora**

```sh
sudo dnf install cargo rust make rpm-build 'pkgconfig(gtk4)' 'pkgconfig(libadwaita-1)' 'pkgconfig(libusb-1.0)'
packaging/fedora/build-rpm.sh
sudo dnf install packaging/fedora/out/RPMS/*/ld-mixer-*.rpm
```

**Nix / NixOS**

```sh
nix profile install .        # or: nix run .
```

On NixOS the udev rule must be enabled declaratively:

```nix
imports = [ ld-mixer.nixosModules.default ];
programs.ld-mixer.enable = true;
```

**Anything else (manual)**

Needs Rust, GTK 4, libadwaita and libusb development files.

```sh
make
sudo make install            # PREFIX=/usr/local by default
```

## Usage

```sh
ld-mixer gui                 # open the mixer (also in your application menu)
ld-mixer gui --mock          # try it with a simulated iD24, no hardware needed
ld-mixer help                # all commands
```

### Background service and tray

lD Mixer runs as a small background service that owns the interface: it
restores your mix whenever the iD24 is plugged in, keeps the virtual devices
alive, and shows a tray icon (left click opens the window; the menu has Dim,
Cut, Alt, Mono, Talkback and Quit). Closing the window leaves the service
running. `ld-mixer gui` starts it on demand; you can also manage it directly:

```sh
ld-mixer start               # start the service in the background
ld-mixer status              # service, device and virtual devices
ld-mixer stop                # stop it (removes the virtual devices)
ld-mixer daemon              # run it in the foreground, e.g. from autostart
```

The tray needs a StatusNotifier host (KDE Plasma, most panels, Waybar; GNOME
with the AppIndicator extension). Without one the service still works.

### The mixer window

The window mirrors the official iD Mixer. The column beside the monitor
section lists **Main**,
**Cue A** and **Cue B** as an accordion: opening one makes it the mix the
strips show (as do the header buttons and Ctrl+1/2/3), and holds that mix's
master level as a knob plus the outputs it is heard on. Each strip shows,
top to bottom, its buttons, pan, mute/solo, level readout and a meter next to
its fader; a linked pair takes exactly two strip widths and has a pan per
channel (L100 / R100 when linked, both centered when unlinked). Right-click
a strip to rename or hide it; hidden strips come back from the main menu.
The pencil on each mix renames it everywhere. The buttons at the top of each
strip are:

- **Ø** polarity (analog inputs), **link** to pair two channels as stereo,
- **microphone / speaker**: show that strip to other apps as its own input
  (Mic, Digi) or output (DAW) while lD Mixer runs, e.g. "iD24 DAW 3+4" as
  a speaker for Discord and "iD24 DAW 1+2" for everything else. Linked
  strips become one stereo device, unlinked ones separate mono devices, and a
  renamed strip keeps its name ("iD24 Discord").

Click any dB or pan readout (faders, master, monitor volume) to type a
value: `-12`, `-6.5 dB`, `-inf`, `C`, `L30`, `R45`. Double-click a strip name
to rename it, double-click a fader or knob to reset it.

The side panel holds the monitor section (it follows the front-panel knob
and buttons), output routing including the loopback pair, and the optical
port formats. Presets live in the main menu.

**Loopback.** The Loopback row picks what the iD24 sends back to the
computer (Main, a cue, or a DAW pair); it arrives as inputs 11+12 of the
multichannel capture device. Turn on **Loopback as App Input** to get an
"iD24 Loopback" input for OBS, Discord, browsers and so on.

**Virtual devices touch no configuration.** Each one is a `pw-loopback`
process owned by the service: it exists while lD Mixer runs and disappears
when it stops. lD Mixer never changes your default devices; pick "iD24 DAW
1+2" as the system output in your sound settings if you want that. Inputs
need the card's "Multichannel Duplex" or "Pro Audio" profile.

On first start lD Mixer reads the current mix from the device instead of
overwriting it. Settings live in `~/.config/ld-mixer/`, the service log in
`~/.local/state/ld-mixer/daemon.log`.

Device commands for inspection and experiments (`dump`, `snapshot`,
`restore`, `meters`, `watch`, `get`, ...) are listed by `ld-mixer help`; the
ones that write require `--write`. While the service runs they go through
it.

## Uninstall

One command removes lD Mixer and everything it created, through whichever
package manager installed it (pacman, dnf/rpm, Nix, or the manual-install
manifest), plus your lD Mixer settings and presets:

```sh
ld-mixer uninstall           # add -y to skip the prompt, --keep-user-data to keep settings
```

Equivalent per-distro commands: `sudo pacman -Rns ld-mixer`,
`sudo dnf remove ld-mixer`, `nix profile remove ld-mixer`,
`sudo make uninstall` (these leave `~/.config/ld-mixer` in place).

## USB access

Linux does not let normal users open USB devices by default. The packages
install `70-ld-mixer.rules`, which grants the user logged in at the seat
access to Audient devices (`TAG+="uaccess"`), and it is removed again on
uninstall. For a quick test without installing:

```sh
sudo setfacl -m u:$USER:rw /dev/bus/usb/BBB/DDD   # from `ld-mixer scan`; lost on replug
```

## Protocol

See [docs/PROTOCOL.md](docs/PROTOCOL.md) for what lD Mixer sends and what
was measured on hardware.

## Credits

Protocol knowledge comes from prior reverse-engineering work:
[MixiD](https://github.com/TheOnlyJoey/MixiD),
[BiD](https://github.com/baakhoff/BiD),
[Monix](https://github.com/sKuhLight/monix) and
[audient-id24-dkms](https://github.com/Magnetar-OS/audient-id24-dkms).

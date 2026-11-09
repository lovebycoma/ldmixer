# lD Mixer

An unofficial mixer and control panel for Audient iD USB audio interfaces on
Linux. The name is a pun: **l**D Mixer = **L**inux iD Mixer.

Work in progress. Developed and tested first on an Audient iD24.

- Main, Cue A and Cue B mixes, output routing and loopback, the monitor
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
ld-mixer gui --mock          # try the UI with a simulated iD24, no hardware needed
ld-mixer help                # all commands
```

The window mirrors the official iD Mixer: pick **Main**, **Cue A** or
**Cue B** in the header (Ctrl+1/2/3), and the strips show that mix's
faders, pan, mute and solo. LINK pairs two channels into a stereo strip,
double-click a strip name to rename it, and double-click a fader or knob to
reset it. The side panel holds the monitor section (it follows the
front-panel knob and buttons), output routing including the loopback pair,
and the optical port formats. Presets live in the main menu.

On first start lD Mixer reads the current mix from the device instead of
overwriting it. Afterwards it restores its saved state on every connect.
Settings live in `~/.config/ld-mixer/`.

Device commands for inspection and experiments (`dump`, `snapshot`,
`restore`, `meters`, `watch`, ...) are listed by `ld-mixer help`; the ones
that write require `--write`.

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

## Credits

Protocol knowledge comes from prior reverse-engineering work:
[MixiD](https://github.com/TheOnlyJoey/MixiD),
[BiD](https://github.com/baakhoff/BiD),
[Monix](https://github.com/sKuhLight/monix) and
[audient-id24-dkms](https://github.com/Magnetar-OS/audient-id24-dkms).

# AGENTS.md

Guidance for coding agents (and humans) working on **lD Mixer** (`ldmixer`), an unofficial Linux mixer for Audient iD USB interfaces. Read this before changing code. User-facing documentation is in `README.md`; the USB protocol is in `docs/PROTOCOL.md`.

## What the project is for

lD Mixer replaces Audient's iD Mixer on Linux and targets the iD family in general (profiles per model; unverified models are recognized, not driven). Tested only on the owner's iD24 (firmware 1.12) with Artix Linux, kernel 7.3.1 (CachyOS), PipeWire 1.6.11, WirePlumber 0.5.18, EasyEffects 8.3, niri (Wayland) with a quickshell tray and the adw-gtk3 theme. The project is largely AI-written ("~90% vibe coding") without a proper human review, and the README says so together with a use-at-your-own-risk notice; keep that notice. The README's tested-environment list names components without versions (they change too often); only the interface firmware version is listed.

## Project rules (from the owner; do not break)

1. **Never change the owner's real system.** Do not install packages, edit system or user configuration (PipeWire, WirePlumber, ALSA, udev, gtk), or restart services on their machine. When something like that is needed, explain it and let the owner run it (in Claude Code: `! sudo …`). Reading is fine.
2. **The program never changes system settings either.** No PipeWire or WirePlumber configuration, no default devices, no sample rate or quantum changes (they are display-only), no kernel modules, no groups. The only system file the package installs is the udev rule `70-ldmixer.rules`, which grants `uaccess` only to the product IDs in `PROFILES` (a unit test keeps them in sync; add a line when a model becomes driven). Virtual devices are child processes that disappear with the service.
3. **Ask before writing to the interface during experiments.** Take a snapshot first (`ldmixer snapshot <file>`); read-only probing is fine.
4. **Native packages, no Flatpak.** Arch (`packaging/arch/PKGBUILD`, built with `makepkg`), Fedora (`packaging/fedora/ldmixer.spec`), Nix (`flake.nix`), and `make install`. Every install path must be fully removable with **`ldmixer uninstall`**.
5. **One command, `ldmixer`.** `ldmixer gui` opens the window, `ldmixer uninstall` removes everything; no extra binaries.
6. **Rust.** GTK 4 (gtk4-rs) + libadwaita, rusb, ksni.
7. **US English** for code comments and docs. There is no translated README; keep `README.md` short.
8. **No hard wrapping in Markdown.** One paragraph or list item is one line; break lines only for a real new paragraph, list item, table row or code block. The owner edits these files by hand.
9. **Behave like Audient's iD Mixer**: Main, Cue A, Cue B, monitor section, routing, loopback.
10. Commit messages end with the co-author trailer the session provides. Commit or push only when asked or as part of an agreed task.

## Names and identifiers

| Thing | Value |
|-------|-------|
| Product name | lD Mixer (lD = Linux iD) |
| Command, package, crate | `ldmixer` |
| App ID (GTK, desktop file, icon, tray) | `com.lovebycoma.ldmixer` (`ldmixer::APP_ID`) |
| Repository | `github.com/lovebycoma/ldmixer` (private until published) |
| Config / state | `~/.config/ldmixer/`, `~/.local/state/ldmixer/` |
| Socket | `$XDG_RUNTIME_DIR/ldmixer.sock` (mode 0600) |
| Virtual device nodes | `ldmixer_<strip>` (e.g. `ldmixer_daw_3_4`), `ldmixer_loopback` |

## Architecture

```
            ┌──────────── ldmixer daemon (owns the USB device) ────────────┐
ldmixer gui │  engine thread ── USB (rusb, DFU interface 4)                 │
  (client) ◄┼─ Unix socket ──┤ mixer state (MixerState) ── config files    │
ldmixer CLI │                ├ vdev::Manager ── pw-loopback children        │
  (client) ◄┤                └ tray (ksni StatusNotifierItem)              │
            └───────────────────────────────────────────────────────────────┘
```

- **Daemon** (`src/daemon.rs`): single owner of the device. All inputs (engine events, client requests, tray actions, timer, signals) go through one channel to the main thread, so state needs no locks. Restores the mix on connect (first run: reads the device's mix instead of writing), autosaves one second after the last change, reconciles virtual devices.
- **Engine** (`src/engine.rs`): background thread with the USB handle. Diffs the desired state and writes only changed cells/routes; polls meters (~30 Hz), the monitor section (150 ms), system state (1 s; PipeWire quantum every 5 s). Goes quiet for 3 s when the stream rate changes (control traffic during a rate change can wedge the firmware).
- **IPC** (`src/ipc.rs`): newline-delimited JSON `Request`/`Notice`. `RemoteIo` implements `ControlIo` over the socket so CLI commands work while the daemon runs. `connect_or_spawn` starts the daemon on demand.
- **Device layer** (`src/device.rs`): `ControlIo` trait with typed helpers; `UsbDevice` (real) and `mock::MockDevice` (`--mock`).
- **Protocol tables** (`src/protocol.rs`): profiles (only the iD24 is driven), routing codes, level encoding, fader taper.
- **Mixer model** (`src/mixer.rs`): strips, links, per-mix sends, mix names and colors (`mix_colors`, `#rrggbb`, empty = default), hidden strips; computes the 96-cell matrix (mute/solo/pan/master are baked in, as the hardware has none of them). `STATE_VERSION` = 2; version 1 files are migrated (linked pairs get L100/R100).
- **Virtual devices** (`src/vdev.rs`): one `pw-loopback` per device; `Manager::reconcile` keeps the running set equal to the wanted set and retries failures.
- **Tray** (`src/tray.rs`), **CLI** (`src/cli.rs`), **entry point** (`src/main.rs`, embeds `data/uninstall.sh`).
- **GUI** (`src/gui/mod.rs`, `src/gui/widgets.rs`): pure daemon client. It edits a copy of `MixerState` and sends it (coalesced to ~60/s).

## Hardware and protocol facts that matter

Details and sources in `docs/PROTOCOL.md`. Verified on an iD24 (firmware 1.12):

- Control requests go to the spare **DFU interface** (interface 4), which no kernel driver binds, so audio keeps running. **Never detach a kernel driver or reset the device.**
- Mixer: 16 nodes (Mic 1-2, Digi 1-8, DAW 1-6) x 6 sends, `cell = node*6 + bus*2 + side`. Levels are int16 in 1/256 dB, `0x8000` = silence. Matrix and routing answer GET, so a first start can read them.
- Routing codes follow BiD's ear-verified table: Main `0x25/26`, Alt `0x27/28`, Cue A `0x1e/1f`, Cue B `0x20/21`, DAW n `n`, input n `0x10+n`. Outputs 10/11 are the **loopback**; it comes back as capture channels 11/12 and works (BiD reported it silent).
- **Meters**: GET_MEM offset 0, one int16 (1/256 dB) per node, 0 = silence.
- **Feature unit 0x0c**: reads are junk (unwritten channels read -inf); writing channel 2 attenuates Main R. lD Mixer must leave it alone.
- The interface stores what it is sent (routing survives power cycles), so uninstalling lD Mixer does not revert the hardware.
- The "no sound" incident during development was the owner's front-panel knob, not software; check the knob first.

## PipeWire integration

- Virtual devices: the device-side stream sets `target.object`, `node.dont-move`, `node.dont-fallback`, `node.dont-reconnect`, `node.passive`, `stream.dont-remix` and `state.restore-props/target=false`. Without `node.dont-move`, EasyEffects moved the stream into its own sink and created a feedback cycle (silence). `tests/vdev_live.rs` checks the actual links.
- Children get `PR_SET_PDEATHSIG` and a reset signal mask (`ldmixer::reset_signal_mask`; the daemon blocks SIGTERM/SIGINT/SIGHUP).
- Inputs need the card profile "Multichannel Duplex" or "Pro Audio".

## GUI notes

- The fader is a `SnapArea` (render nodes, GPU); its static scale is a cached texture. A Cairo `DrawingArea` redrawn at meter rate cost ~50% CPU; the render-node version ~1%. Keep frequently redrawn widgets on render nodes, and skip redraws when nothing changed.
- App CSS is registered at `STYLE_PROVIDER_PRIORITY_USER + 100`: the owner uses adw-gtk3, whose user `gtk.css` loads a full libadwaita stylesheet that otherwise overrides app CSS. Strip buttons have a minimum size (`tiny_w()` x `tiny_h()`), fill their row, and drop `text-button`/`image-button`. Mix colors are a second, generated CSS provider (`mix_color_css`); the dot in front of each mix opens the color picker.
- The GUI sets its own (in-process) `gtk-font-rendering` to Manual: GTK 4.24's automatic mode cut the top pixel row off glyphs at fractional scales (1.25). Check text at the owner's real scale through niri (`niri msg action screenshot-window`), not only with `LDMIXER_SCREENSHOT`, which renders at scale 1.
- Sizes are in em, not px: CSS uses `em`, Rust uses `widgets::em()`/`emi()`, measured once at startup from the GTK font and text scale (1 em = 14.67 px at 11 pt / 96 dpi). Only hairlines stay in px.
- Layout: strips (edge to edge, linked pair = exactly 2 x `strip_w()`) | Mixes accordion | Monitor column. Breakpoints (em-based) stack Mixes above Monitor below 90 em and tuck the side column into an overlay sidebar (header button) below 55 em. Strip order (modeled on Audient's iD Mixer): header (name, input type, top line in the open mix's color), Ø/device buttons, pans with L/R, MONO/STEREO badge (click links or unlinks the pair), S | M, level, meter + fader.
- Stock GTK first: colors come from libadwaita named colors (`@view_bg_color`, `@accent_bg_color`, ...) and joined buttons use `.linked`; only state colors (mute, solo, cut, ...) and the drawn fader/knob parts are fixed. Typed values: click any dB/pan readout.

## Testing

```sh
cargo test                                   # unit tests (model, protocol, parsing)
cargo test --test vdev_live -- --ignored     # live PipeWire test, needs an iD interface
cargo run -- gui --mock                      # UI against the simulated iD24
```

Isolated runs that do not disturb the owner's running instance:

- `LDMIXER_CONFIG_DIR=<dir>`: separate lD Mixer settings. **Do not override `XDG_CONFIG_HOME` for GUI checks**; it hides the owner's GTK theme and screenshots stop matching what they see.
- A short `XDG_RUNTIME_DIR` (socket paths are limited to 108 bytes) plus an absolute `WAYLAND_DISPLAY=/run/user/<uid>/wayland-1`.
- `LDMIXER_SCREENSHOT=<png>` renders the window to a file and quits (`LDMIXER_SCREENSHOT_BUS=main|cuea|cueb`, `LDMIXER_SCREENSHOT_DELAY_MS=<ms>`); it also makes the GUI non-unique so it does not hand off to a running window.
- `LDMIXER_MOCK_SILENT=1`: mock meters stay silent (for CPU comparisons).
- CPU: sample `utime+stime` from `/proc/<pid>/stat` over a few seconds.

## Packaging and releases

- Version lives in `Cargo.toml`, `packaging/arch/PKGBUILD` (`pkgver=`) and `packaging/fedora/ldmixer.spec`. `makepkg` rewrites the PKGBUILD's `pkgver` line; do not commit that change (`git checkout` it).
- The PKGBUILD builds from the local clone's committed state (`LDMIXER_SOURCE` overrides).
- `make install` writes a manifest used by `ldmixer uninstall` for manual installs. The uninstaller detects pacman, rpm, Nix or the manifest.

## History of decisions

1. Flatpak was planned first, then dropped for native packages (the owner's call) with a one-command uninstall.
2. Python was considered, then Rust was chosen (the owner's preference).
3. Control over the DFU interface (BiD's approach) instead of a kernel module (Monix) or a patched snd-usb-audio (audient-id24-dkms).
4. Single `ldmixer` binary; background service + tray so the mix and virtual devices outlive the window; GUI and CLI are clients.
5. Renamed from `ld-mixer` to `ldmixer` before any public release; App ID `com.lovebycoma.ldmixer`.

## Open items

- Other iD models (iD4/14/22/44/48): profiles are known from BiD/Monix but unverified; they are recognized and not driven.
- F1-F3 buttons and the alt-speaker trim are not reachable without a capture of the official app.
- Meter blocks at GET_MEM offsets 1-3 (outputs, buses) are unexplored.
- Possible: an `ldmixer autostart on|off` command (one user-level autostart file, removed by the uninstaller).

# Audient iD control protocol: lD Mixer's notes

What lD Mixer relies on, and what has been measured on lD Mixer's own test
iD24 (USB `2708:000d`, firmware 1.12). Most of the protocol was mapped by
[MixiD](https://github.com/TheOnlyJoey/MixiD),
[BiD](https://github.com/baakhoff/BiD) (see its `docs/PROTOCOL.md`) and
[Monix](https://github.com/sKuhLight/monix); this file records where lD
Mixer follows them and what it confirmed or found differently.

## Transport

Class requests on the default control pipe, addressed to the spare DFU
interface (interface 4 on the iD24), which no kernel driver binds:

    SET      bmRequestType 0x21  bRequest 0x01
    GET      bmRequestType 0xa1  bRequest 0x01
    GET_MEM  bmRequestType 0xa1  bRequest 0x03   (meters)
    wValue = (selector << 8) | channel
    wIndex = (entity << 8) | interface

Levels everywhere are little-endian int16 in 1/256 dB: `0x0000` = 0 dB,
`0x8000` = silence.

## Entities used

| Entity | Selector | Channel | What |
|--------|----------|---------|------|
| `0x3c` | `0x01` | cell | Mixer crosspoint, `cell = node * 6 + bus * 2 + side` |
| `0x3c` | GET_MEM offset 0 | - | Input meters |
| `0x33` | `0x06` | output | Routing source code (1 byte) |
| `0x36` | `0x00..0x12` | 0 | Monitor section (see `src/protocol.rs`) |
| `0x0b` | `0x0d` | input + 1 | Phase invert |
| `0x01` / `0x14` | `0x00` / `0x01` | 0 | Optical in / out format, 0 = ADAT, 1 = S/PDIF |
| `0x29` | `0x01` | 0 | Sample rate, int32 Hz (read only; lD Mixer never sets it) |

Mixer nodes on the iD24: Mic 1, Mic 2, Digi 1-8, DAW 1-6. Buses: Main,
Cue A, Cue B.

Routing codes on the iD24 follow BiD's ear-verified table: Main
`0x25/0x26`, Alt `0x27/0x28`, Cue A `0x1e/0x1f`, Cue B `0x20/0x21`, DAW
channel n `n` (zero-based), input n `0x10 + n`.

## Measured by lD Mixer

**Meters.** GET_MEM on `0x3c` offset 0 returns two bytes per node, and the
pair is one int16 in 1/256 dB, the same scale as the level controls
(music on DAW 1/2 read `0xedd0` = -18.2 dB). Digital silence reads `0x0000`.
Earlier projects read only the first byte.

**Loopback works.** Outputs 10/11 of the routing unit are the loopback
pair: whatever they are routed to arrives in the computer as capture
channels 11/12 (`AUX10/AUX11` in PipeWire). Measured with Main routed
there: about -9 dBFS RMS on both channels while music played. The loopback
is not affected by the monitor volume (it read the same level with the
monitor at -40.8 dB). BiD had reported it silent.

**Readback.** Routing (`0x33`) and the matrix (`0x3c`) answer GET with
per-output and per-cell values that match what was written, so lD Mixer
reads the device's mix on first start instead of overwriting it.

**Feature unit `0x0c` reads are not trustworthy.** Channels that were never
written read `0x8000` (-inf), including channel 8, which does not exist and
rejects writes, while channel 7 rejects writes yet reads `0x0000`. After a
write, a channel reads back the written value. Do not conclude anything
from a read of this unit before writing it.

**Feature unit `0x0c` writes are asymmetric.** With Main on the loopback,
setting `0x0c` channel 2 to -inf silenced the right loopback channel, and
-20 dB lowered it by 20 dB. Muting channels 0, 1 and 3-6 one at a time
changed neither loopback channel. This matches the asymmetry the
audient-id24-dkms project describes for ALSA's "Speaker Playback Volume"
(the same unit). lD Mixer leaves this unit alone; on the test device all
channels are at 0 dB, which is neutral.

**Optical out format** has been seen to read 255, which is neither ADAT nor
S/PDIF; lD Mixer then shows nothing rather than guessing.

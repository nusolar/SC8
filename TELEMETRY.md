# NU Solar CAN → XBee Telemetry

End-to-end pipeline that ships CAN bus data from the car to the chase vehicle
over an encrypted XBee radio link.

```
[Car: Raspberry Pi]                                      [Chase: laptop]
can0 (SocketCAN, MCP2515 HAT)                            telemetry_receiver
   │                                                          ▲
   ▼                                                          │ decrypt + decode
can_xbee_bridge ──► batch ──► AES-256-GCM ──► XBee ── RF ──► XBee (USB serial)
```

There is **no Unix socket or other IPC layer**: SocketCAN's `can0` is already a
kernel socket, and every process that opens it receives all frames — the bridge
runs alongside the existing dashboard/Redis pipeline without touching it.

## Repos and crates

| Crate | Where | Runs on | Purpose |
|---|---|---|---|
| `xbee_rust_modem_library` | sibling repo `../xbee_rust_modem_library` | any | Radio library: serial discovery, XBee API mode (AP=2), handshake, AES-256-GCM sessions. **Payload-agnostic** — knows nothing about CAN. |
| `telemetry_types` | this workspace | any | Shared wire format (`CanFrameMsg`, `CanBatch`). The only contract between car and chase sides. |
| `can_xbee_bridge` | this workspace | Pi (Linux only) | Reads both CAN buses (`can0` Kelly + `can1` MPPTs), batches frames, encrypts, transmits. |
| `telemetry_receiver` | this workspace | mac/Windows/Linux | Decrypts batches, prints frames candump-style. |
| `telemetry_gui` | this workspace | mac/Windows/Linux | Chase dashboard — same 800×480 (5:3) layout as the onboard display, decodes signals with the car's own dbc-codegen modules. |
| `solar_rust` | this workspace | Pi | Existing dashboard/Redis pipeline — untouched by telemetry. |

Both repos must be cloned **side by side** (the workspace references the radio
library via `path = "../../xbee_rust_modem_library"`):

```
NUSolar/
├── DanielAntonySC8/          # this workspace
└── xbee_rust_modem_library/
```

`cargo build` at the workspace root builds only the portable crates
(`default-members`), so it works on a dev mac. On the Pi, use
`cargo build --workspace` or `-p can_xbee_bridge`.

## Wire protocol (outer → inner)

1. **XBee API mode (AP=2, escaped)** — 0x10 transmit / 0x90 receive frames;
   handled by `ApiModeTransport`.
2. **COBS framing** — one 0x00-delimited frame per message (`postcard` + COBS).
3. **`WireMsg` enum** — tags every frame as `Handshake` or `Data` so the two
   can never be confused.
4. **`SecureFrame`** — AES-256-GCM ciphertext with sender id, sequence number
   (replay protection), and nonce derived from (sender_id, seq).
5. **`CanBatch`** (the plaintext) — up to 11 CAN frames with a shared base
   timestamp and per-frame millisecond offsets.

One sealed batch always fits one RF packet (the radio's `NP` is 256 bytes);
this is enforced by the `size_budget_fits_one_rf_packet` test in
`telemetry_types`.

## Security model

- Each side has an Ed25519 identity key; sessions are established with a
  signed X25519 handshake (HKDF-SHA256 → AES-256-GCM session key).
- The receiver only answers hellos signed by the key in
  `keys/authorized_sender.pub`; the sender only accepts ServerHellos signed by
  `keys/authorized_receiver.pub`.
- The bridge re-handshakes automatically after a radio reconnect; the receiver
  rekeys transparently mid-stream (`rehandshakes` counter ticks up).
- Telemetry is **lossy by design**: if the radio can't keep up, frames are
  dropped and counted rather than ever blocking the CAN bus reads.

### One-time key provisioning

```bash
# On the Pi (generates keys/sender_ed25519.key + .pub on first run):
./scripts/car_sender.sh            # Ctrl-C after "no XBee" or handshake spam is fine

# On the laptop (generates keys/receiver_ed25519.key + .pub):
./scripts/chase_receiver.sh

# Cross-copy the PUBLIC keys (scp/USB):
#   Pi:     keys/sender_ed25519.pub   → laptop: keys/authorized_sender.pub
#   laptop: keys/receiver_ed25519.pub → Pi:     keys/authorized_receiver.pub
```

Never copy the `.key` (private) files off the device that generated them.

## Radio configuration (XCTU, both modules)

| Setting | Value | Notes |
|---|---|---|
| `AP` | `2` (API escaped) | required |
| `BD` | `7` (115200) | must equal `XBEE_BAUD` (default 115200); at 9600 the UART caps throughput at ~3 batches/s |
| `SH`+`SL` | — | each radio's 64-bit address; give the *car's* SH+SL to the bridge via `XBEE_DEST64` |

The **receiver side needs no destination configured**: it answers on broadcast
and automatically locks onto the car's address once the first authenticated
ClientHello arrives (and re-locks after every re-handshake). Set `XBEE_DEST64`
on the bridge for unicast with ACKs; if unset it broadcasts, which works but
is slower.

## Configuration reference (env vars)

| Var | Used by | Default | Meaning |
|---|---|---|---|
| `CAN_IFACE` | bridge | `can0,can1` | comma-separated SocketCAN interfaces; missing ones are skipped with a warning (`vcan0` for testing) |
| `CAN_FILTER` | bridge | all ids | comma-separated hex CAN ids to whitelist (kernel-side) |
| `XBEE_BAUD` | both | `115200` | UART baud; must equal the radios' `BD` |
| `XBEE_DEST64` | bridge | broadcast | car-side only: peer radio's SH+SL (16 hex digits); also `--xbee-dest64=`. The receiver auto-locks onto the sender. |
| `KEYS_DIR` | both | `keys` | key directory; set absolute when run from systemd |
| `CAN_BITRATE` | car script | `500000` | used when bringing `can0` up |

## Running

Car (Pi):

```bash
./scripts/car_sender.sh                      # brings can0 up, builds, runs
CAN_IFACE=vcan0 ./scripts/car_sender.sh      # test without CAN hardware
```

Chase (mac/Linux):

```bash
./scripts/chase_gui.sh        # dashboard GUI (mirrors the onboard display)
./scripts/chase_receiver.sh   # or: headless candump-style text output
```

Chase (Windows, PowerShell):

```powershell
.\scripts\chase_receiver.ps1                       # text receiver
cargo run --release -p telemetry_gui               # dashboard GUI
```

No destination config is needed on the chase side — the receiver answers on
broadcast and locks onto the car's radio automatically.

Receiver output is candump-style, one line per CAN frame:

```
(1718031234.567) 0CF11E05#DEADBEEF01020304     # extended id
(1718031234.591) 600#0102030405060708          # standard id
```

with a once-per-second stats line on stderr
(`batches / ok / auth_fail / dup-old / decode_fail / rehandshakes`).

## Throughput budget

- CAN at 500 kbps ≈ 3–4k frames/s; the radio carries ~10 batches/s × 11
  frames ≈ **110 frames/s**. If the bus is busy, whitelist what matters:
  `CAN_FILTER=0xCF11E05,0xCF11F05,0x600,0x601,...`
- The bridge flushes a batch when it has 11 frames **or** 100 ms after its
  first frame, whichever comes first.
- The bridge polls **all configured buses** (`CAN_IFACE=can0,can1`) into one
  shared batch stream — Kelly and MPPT ids don't collide (extended 0xCF11Exx
  vs standard 0x6xx), and the GUI decodes per-id regardless of source bus.

## Testing without hardware

```bash
# Library round-trip (handshake + encrypt + replay + rekey), runs anywhere:
cd ../xbee_rust_modem_library && cargo test

# Wire-format and size-budget tests:
cargo test -p telemetry_types

# On the Pi, fake CAN end-to-end (real radios, no car):
sudo ip link add dev vcan0 type vcan && sudo ip link set up vcan0
CAN_IFACE=vcan0 ./scripts/car_sender.sh &
cansend vcan0 123#DEADBEEF          # appears on the chase laptop
cangen vcan0 -g 5                   # burst: exercises batching + drop counters
```

When bringing up real hardware, verify the bare radio link first with the
library's `test_sender`/`test_receiver` bins — that isolates radio config
(AP/BD/dest) from bridge problems.

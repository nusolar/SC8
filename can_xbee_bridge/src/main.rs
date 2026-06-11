//! Car-side bridge: reads CAN frames from a SocketCAN interface, batches
//! them, and sends each batch encrypted over the XBee radio.
//!
//! Configuration (env vars):
//! - `CAN_IFACE`   comma-separated CAN interfaces (default `can0,can1` —
//!                 Kelly + MPPT buses; use `vcan0` to test)
//! - `CAN_FILTER`  comma-separated hex CAN ids to whitelist (default: all)
//! - `XBEE_BAUD`   UART baud, must match the radio's BD setting (default 9600)
//! - `XBEE_DEST64` peer radio's 64-bit address (SH+SL); also `--xbee-dest64=`
//! - `KEYS_DIR`    directory holding sender_ed25519.key + authorized_receiver.pub
//!                 (default `keys`; set an absolute path when run from systemd)
//!
//! Telemetry is lossy by design: if the radio can't keep up or is down,
//! frames are dropped and counted — the bridge never blocks the CAN bus reads
//! indefinitely and never crashes out of its reconnect loop.

use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serialport::{DataBits, StopBits};
use socketcan::{CanFilter, CanFrame, CanSocket, EmbeddedFrame, Frame, Socket, SocketOptions};

use telemetry_types::{CanBatch, CanFrameMsg, EXTENDED_ID_FLAG};
use xbee_rust_modem_library::keys::{
    load_ed25519_public_key, load_or_generate_ed25519_signing_key,
};
use xbee_rust_modem_library::serial::{XBeeDevice, discover_xbee_ports};
use xbee_rust_modem_library::session::SecureSender;
use xbee_rust_modem_library::transport::{ApiModeTransport, xbee_destination};
use xbee_rust_modem_library::{SigningKey, VerifyingKey};

/// Send a partial batch if this much time has passed since its first frame.
const FLUSH_INTERVAL: Duration = Duration::from_millis(100);
const HANDSHAKE_RETRY: Duration = Duration::from_secs(2);
const CAN_READ_TIMEOUT: Duration = Duration::from_millis(10);
const RECONNECT_BACKOFF: Duration = Duration::from_secs(1);

#[derive(Default)]
struct Stats {
    batches_sent: u64,
    frames_sent: u64,
    frames_dropped: u64,
    can_errors: u64,
    reconnects: u64,
}

fn main() {
    let ifaces = env::var("CAN_IFACE").unwrap_or_else(|_| "can0,can1".into());
    let keys_dir = PathBuf::from(env::var("KEYS_DIR").unwrap_or_else(|_| "keys".into()));
    let baud: u32 = env::var("XBEE_BAUD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(115_200);

    let signing_key =
        load_or_generate_ed25519_signing_key(&keys_dir.join("sender_ed25519.key"))
            .expect("cannot load/generate sender key");
    let authorized_receiver =
        load_ed25519_public_key(&keys_dir.join("authorized_receiver.pub")).unwrap_or_else(|e| {
            panic!(
                "missing {}/authorized_receiver.pub (copy the receiver's .pub there): {e}",
                keys_dir.display()
            )
        });

    // Open every requested bus; tolerate missing ones (e.g. vcan0-only test
    // rigs) as long as at least one opens.
    let socks: Vec<CanSocket> = ifaces
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|iface| match CanSocket::open(iface) {
            Ok(sock) => {
                apply_id_filter(&sock);
                println!("Bridge reading CAN frames from {iface}");
                Some(sock)
            }
            Err(e) => {
                eprintln!("warning: cannot open CAN interface {iface}: {e}");
                None
            }
        })
        .collect();
    assert!(!socks.is_empty(), "no CAN interface could be opened ({ifaces})");

    let mut stats = Stats::default();

    // Outer reconnect loop: any serial-side failure lands back here.
    loop {
        let sender = match connect_radio(baud, &signing_key, &authorized_receiver) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("radio connect failed: {e}; retrying");
                std::thread::sleep(RECONNECT_BACKOFF);
                continue;
            }
        };
        println!("Radio session established.");

        if let Err(e) = pump(&socks, sender, &mut stats) {
            stats.reconnects += 1;
            eprintln!("\nradio link lost: {e}; reconnecting");
            std::thread::sleep(RECONNECT_BACKOFF);
        }
    }
}

fn connect_radio(
    baud: u32,
    signing_key: &SigningKey,
    authorized_receiver: &VerifyingKey,
) -> io::Result<SecureSender<ApiModeTransport>> {
    let port_name = discover_xbee_ports()
        .first()
        .cloned()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no XBee serial port found"))?;
    println!("Bridge using radio port: {port_name}");

    let dev = XBeeDevice::new(port_name, baud, StopBits::One, DataBits::Eight)
        .map_err(|e| io::Error::other(format!("{e:?}")))?;
    let (dest64, dest16) = xbee_destination();
    eprintln!("API mode (AP=2): RF dest 64-bit {dest64:#018x}, 16-bit {dest16:#06x}");

    SecureSender::establish(
        ApiModeTransport::new(dev, dest64, dest16),
        signing_key,
        authorized_receiver,
        HANDSHAKE_RETRY,
    )
}

/// Read CAN frames from all buses and ship batches until the radio link
/// errors out.
fn pump(
    socks: &[CanSocket],
    mut sender: SecureSender<ApiModeTransport>,
    stats: &mut Stats,
) -> io::Result<()> {
    let mut batch = CanBatch::default();
    let mut batch_started: Option<Instant> = None;
    let mut last_status = Instant::now();

    loop {
        for sock in socks {
            match sock.read_frame_timeout(CAN_READ_TIMEOUT) {
                Ok(CanFrame::Data(frame)) => {
                    let now_ms = unix_ms();
                    if batch.frames.is_empty() {
                        batch.base_ts_ms = now_ms;
                        batch_started = Some(Instant::now());
                    }
                    let msg = CanFrameMsg {
                        id: frame.raw_id()
                            | if frame.is_extended() { EXTENDED_ID_FLAG } else { 0 },
                        data: heapless::Vec::from_slice(frame.data())
                            .expect("CAN 2.0 data is at most 8 bytes"),
                        ts_offset_ms: (now_ms - batch.base_ts_ms).min(u16::MAX as u64) as u16,
                    };
                    if batch.frames.push(msg).is_err() {
                        stats.frames_dropped += 1; // can't happen: full batches flush below
                    }
                }
                Ok(_) => {} // remote/error frames: not telemetry
                Err(ref e)
                    if e.kind() == io::ErrorKind::TimedOut
                        || e.kind() == io::ErrorKind::WouldBlock => {}
                Err(_) => stats.can_errors += 1,
            }
        }

        let flush_due = batch_started
            .is_some_and(|started| started.elapsed() >= FLUSH_INTERVAL);
        if batch.frames.is_full() || (flush_due && !batch.frames.is_empty()) {
            let mut buf = [0u8; 512];
            let bytes = postcard::to_slice(&batch, &mut buf)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, format!("{e:?}")))?;
            sender.send_payload(bytes)?; // radio failure propagates to reconnect

            stats.batches_sent += 1;
            stats.frames_sent += batch.frames.len() as u64;
            batch.frames.clear();
            batch_started = None;
        }

        if last_status.elapsed() >= Duration::from_secs(1) {
            eprint!(
                "\rTX batches={} frames={} dropped={} can_err={} reconnects={}     ",
                stats.batches_sent,
                stats.frames_sent,
                stats.frames_dropped,
                stats.can_errors,
                stats.reconnects
            );
            io::stderr().flush().ok();
            last_status = Instant::now();
        }
    }
}

/// `CAN_FILTER=0x101,0x202,...` installs a kernel-side id whitelist so the
/// bridge never wakes up for frames it won't send. Default: receive all.
fn apply_id_filter(sock: &CanSocket) {
    let Ok(spec) = env::var("CAN_FILTER") else {
        return;
    };
    let filters: Vec<CanFilter> = spec
        .split(',')
        .filter_map(|s| {
            let s = s.trim().trim_start_matches("0x").trim_start_matches("0X");
            u32::from_str_radix(s, 16).ok()
        })
        .map(|id| CanFilter::new(id, 0x1FFF_FFFF))
        .collect();
    if filters.is_empty() {
        eprintln!("warning: CAN_FILTER set but no valid hex ids parsed; receiving all");
        return;
    }
    sock.set_filters(filters.as_slice())
        .expect("failed to install CAN id filters");
    println!("CAN id whitelist active: {} ids", filters.len());
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_millis() as u64
}

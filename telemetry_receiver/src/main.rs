//! Chase-side receiver: decrypts telemetry batches from the car's bridge and
//! prints each CAN frame candump-style. Builds on any platform (no socketcan).
//!
//! Configuration (env vars):
//! - `XBEE_BAUD`   UART baud, must match the radio's BD setting (default 9600)
//! - `XBEE_DEST64` car radio's 64-bit address (SH+SL); also `--xbee-dest64=`
//! - `KEYS_DIR`    directory holding receiver_ed25519.key + authorized_sender.pub
//!                 (default `keys`)

use std::env;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serialport::{DataBits, StopBits};

use telemetry_types::CanBatch;
use xbee_rust_modem_library::keys::{
    load_ed25519_public_key, load_or_generate_ed25519_signing_key,
};
use xbee_rust_modem_library::serial::{XBeeDevice, discover_xbee_ports};
use xbee_rust_modem_library::session::SecureReceiver;
use xbee_rust_modem_library::transport::{ApiModeTransport, xbee_destination};

fn main() {
    let keys_dir = PathBuf::from(env::var("KEYS_DIR").unwrap_or_else(|_| "keys".into()));
    let baud: u32 = env::var("XBEE_BAUD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(115_200);

    let receiver_sk =
        load_or_generate_ed25519_signing_key(&keys_dir.join("receiver_ed25519.key"))
            .expect("cannot load/generate receiver key");
    let authorized_sender =
        load_ed25519_public_key(&keys_dir.join("authorized_sender.pub")).unwrap_or_else(|e| {
            panic!(
                "missing {}/authorized_sender.pub (copy the car's sender .pub there): {e}",
                keys_dir.display()
            )
        });

    let port_name = discover_xbee_ports()
        .first()
        .cloned()
        .expect("No XBee device found. Check USB connection and permissions.");
    println!("Receiver using port: {port_name}");

    let dev = XBeeDevice::new(port_name, baud, StopBits::One, DataBits::Eight).unwrap();
    let (dest64, dest16) = xbee_destination();
    eprintln!("API mode (AP=2): RF dest 64-bit {dest64:#018x}, 16-bit {dest16:#06x}");
    let dev = ApiModeTransport::new(dev, dest64, dest16);

    println!("Waiting for the car to handshake...");
    let mut receiver = SecureReceiver::establish(dev, receiver_sk, authorized_sender)
        .expect("Handshake failed");
    println!("Session established. Receiving telemetry.");

    let mut batches: u64 = 0;
    let mut bad_batches: u64 = 0;
    let mut last_status = Instant::now();

    loop {
        match receiver.recv_payload(Some(Duration::from_millis(200))) {
            Ok(Some(payload)) => match postcard::from_bytes::<CanBatch>(payload.as_slice()) {
                Ok(batch) => {
                    batches += 1;
                    print_batch(&batch);
                }
                Err(_) => bad_batches += 1,
            },
            Ok(None) => {} // timeout: fall through to the stats line
            Err(e) => eprintln!("serial err: {e:?}"),
        }

        if last_status.elapsed() >= Duration::from_secs(1) {
            let s = &receiver.stats;
            eprint!(
                "\rRX batches={} ok={} auth_fail={} dup/old={} skipped={} decode_fail={} bad_batch={} rehandshakes={}     ",
                batches,
                s.ok,
                s.auth_fail,
                s.dup_or_old_drop,
                s.skipped_packets,
                s.decode_fail,
                bad_batches,
                s.rehandshakes
            );
            io::stderr().flush().ok();
            last_status = Instant::now();
        }
    }
}

/// candump-style: `(unix_seconds.millis) 123#DEADBEEF` (extended ids 8 hex digits).
fn print_batch(batch: &CanBatch) {
    let mut out = io::stdout().lock();
    for frame in &batch.frames {
        let ts_ms = batch.base_ts_ms + frame.ts_offset_ms as u64;
        let hex: String = frame.data.iter().map(|b| format!("{b:02X}")).collect();
        let line = if frame.is_extended() {
            format!("({}.{:03}) {:08X}#{}\n", ts_ms / 1000, ts_ms % 1000, frame.can_id(), hex)
        } else {
            format!("({}.{:03}) {:03X}#{}\n", ts_ms / 1000, ts_ms % 1000, frame.can_id(), hex)
        };
        out.write_all(line.as_bytes()).unwrap();
    }
    out.flush().unwrap();
}

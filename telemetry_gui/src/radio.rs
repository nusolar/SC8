//! Background radio thread: owns the blocking XBee serial link and keeps the
//! shared state updated. The iced UI never blocks — it just reads this state
//! on its tick.

use std::env;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serialport::{DataBits, StopBits};

use telemetry_types::CanBatch;
use xbee_rust_modem_library::keys::{
    load_ed25519_public_key, load_or_generate_ed25519_signing_key,
};
use xbee_rust_modem_library::serial::{XBeeDevice, discover_xbee_ports};
use xbee_rust_modem_library::session::{RxStats, SecureReceiver};
use xbee_rust_modem_library::transport::{ApiModeTransport, xbee_destination};
use xbee_rust_modem_library::{SigningKey, VerifyingKey};

use crate::decode::{self, Telemetry};

const RECONNECT_BACKOFF: Duration = Duration::from_secs(1);

#[derive(Default)]
pub struct LinkShared {
    pub telemetry: Telemetry,
    pub stats: RxStats,
    pub batches: u64,
    pub unknown_frames: u64,
    pub last_batch: Option<Instant>,
    /// Human-readable link state for the UI ("receiving", error text, ...).
    pub status: String,
}

impl LinkShared {
    /// Link counts as up if a batch arrived recently (car sends ≥1 per 100 ms
    /// whenever the bus is alive).
    pub fn link_ok(&self) -> bool {
        self.last_batch
            .is_some_and(|t| t.elapsed() < Duration::from_secs(3))
    }
}

fn set_status(shared: &Arc<Mutex<LinkShared>>, status: impl Into<String>) {
    shared.lock().unwrap().status = status.into();
}

/// Spawn the receiver thread. Panics early (before the window opens) only on
/// key problems, since those need user action and never fix themselves.
pub fn spawn(shared: Arc<Mutex<LinkShared>>) {
    let keys_dir = PathBuf::from(env::var("KEYS_DIR").unwrap_or_else(|_| "keys".into()));
    let receiver_sk = load_or_generate_ed25519_signing_key(&keys_dir.join("receiver_ed25519.key"))
        .expect("cannot load/generate receiver key");
    let authorized_sender = load_ed25519_public_key(&keys_dir.join("authorized_sender.pub"))
        .unwrap_or_else(|e| {
            panic!(
                "missing {}/authorized_sender.pub (copy the car's sender .pub there): {e}",
                keys_dir.display()
            )
        });

    thread::Builder::new()
        .name("xbee-rx".into())
        .spawn(move || run(shared, receiver_sk, authorized_sender))
        .expect("cannot spawn radio thread");
}

fn run(shared: Arc<Mutex<LinkShared>>, receiver_sk: SigningKey, authorized_sender: VerifyingKey) {
    let baud: u32 = env::var("XBEE_BAUD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(115_200);

    loop {
        set_status(&shared, "searching for XBee serial port...");
        let Some(port_name) = discover_xbee_ports().first().cloned() else {
            thread::sleep(RECONNECT_BACKOFF);
            continue;
        };

        let dev = match XBeeDevice::new(port_name.clone(), baud, StopBits::One, DataBits::Eight) {
            Ok(dev) => dev,
            Err(e) => {
                set_status(&shared, format!("cannot open {port_name}: {e:?}"));
                thread::sleep(RECONNECT_BACKOFF);
                continue;
            }
        };
        // Broadcast default: the session locks onto the car once its
        // ClientHello arrives, so no XBEE_DEST64 needed on this side.
        let (dest64, dest16) = xbee_destination();
        let transport = ApiModeTransport::new(dev, dest64, dest16);

        set_status(&shared, format!("{port_name}: waiting for car handshake..."));
        let mut receiver =
            match SecureReceiver::establish(transport, receiver_sk.clone(), authorized_sender) {
                Ok(r) => r,
                Err(e) => {
                    set_status(&shared, format!("handshake failed: {e}"));
                    thread::sleep(RECONNECT_BACKOFF);
                    continue;
                }
            };
        set_status(&shared, "receiving");

        loop {
            match receiver.recv_payload(Some(Duration::from_millis(500))) {
                Ok(Some(payload)) => {
                    let mut s = shared.lock().unwrap();
                    match postcard::from_bytes::<CanBatch>(payload.as_slice()) {
                        Ok(batch) => {
                            for frame in &batch.frames {
                                if !decode::apply(&mut s.telemetry, frame) {
                                    s.unknown_frames += 1;
                                }
                            }
                            s.batches += 1;
                            s.last_batch = Some(Instant::now());
                        }
                        Err(_) => s.unknown_frames += 1,
                    }
                    s.stats = receiver.stats.clone();
                }
                Ok(None) => {
                    shared.lock().unwrap().stats = receiver.stats.clone();
                }
                Err(e) => {
                    set_status(&shared, format!("radio link lost: {e}; reconnecting"));
                    break; // reopen port + re-handshake
                }
            }
        }
        thread::sleep(RECONNECT_BACKOFF);
    }
}

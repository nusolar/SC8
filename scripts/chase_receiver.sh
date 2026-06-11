#!/usr/bin/env bash
# Chase-side telemetry receiver (mac or Linux laptop).
# Plug in the XBee USB adapter, then:
#
#   ./scripts/chase_receiver.sh
#   XBEE_DEST64=<car radio SH+SL> ./scripts/chase_receiver.sh   # unicast
set -euo pipefail

cd "$(dirname "$0")/.."

export XBEE_BAUD="${XBEE_BAUD:-115200}"
export KEYS_DIR="${KEYS_DIR:-$PWD/keys}"

cargo build --release -p telemetry_receiver

if [[ ! -f "$KEYS_DIR/authorized_sender.pub" ]]; then
    echo "WARNING: $KEYS_DIR/authorized_sender.pub missing — copy the car's" >&2
    echo "         sender_ed25519.pub there (see TELEMETRY.md)." >&2
fi

exec ./target/release/telemetry_receiver "$@"

#!/usr/bin/env bash
# Chase-side telemetry dashboard GUI (mac or Linux laptop).
# Same layout as the car's onboard display, fed by the XBee link.
#
#   ./scripts/chase_gui.sh
set -euo pipefail

cd "$(dirname "$0")/.."

export XBEE_BAUD="${XBEE_BAUD:-115200}"
export KEYS_DIR="${KEYS_DIR:-$PWD/keys}"

cargo build --release -p telemetry_gui

if [[ ! -f "$KEYS_DIR/authorized_sender.pub" ]]; then
    echo "WARNING: $KEYS_DIR/authorized_sender.pub missing — copy the car's" >&2
    echo "         sender_ed25519.pub there (see TELEMETRY.md)." >&2
fi

exec ./target/release/telemetry_gui "$@"

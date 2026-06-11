#!/usr/bin/env bash
# Car-side telemetry sender (Raspberry Pi).
# Brings the CAN interface up if needed, builds, and runs the bridge.
#
#   ./scripts/car_sender.sh
#   CAN_IFACE=vcan0 ./scripts/car_sender.sh          # test without CAN hardware
#   XBEE_DEST64=0013a20041aeb54e ./scripts/car_sender.sh
set -euo pipefail

cd "$(dirname "$0")/.."

export CAN_IFACE="${CAN_IFACE:-can0}"
export XBEE_BAUD="${XBEE_BAUD:-115200}"
export KEYS_DIR="${KEYS_DIR:-$PWD/keys}"
CAN_BITRATE="${CAN_BITRATE:-500000}"

# Bring the interface up if it isn't (vcan interfaces have no bitrate).
if ! ip link show "$CAN_IFACE" 2>/dev/null | grep -q "state UP"; then
    echo "Bringing up $CAN_IFACE..."
    if [[ "$CAN_IFACE" == vcan* ]]; then
        sudo ip link add dev "$CAN_IFACE" type vcan 2>/dev/null || true
        sudo ip link set up "$CAN_IFACE"
    else
        sudo ip link set "$CAN_IFACE" up type can bitrate "$CAN_BITRATE"
    fi
fi

cargo build --release -p can_xbee_bridge

if [[ ! -f "$KEYS_DIR/authorized_receiver.pub" ]]; then
    echo "WARNING: $KEYS_DIR/authorized_receiver.pub missing — copy the" >&2
    echo "         laptop's receiver_ed25519.pub there (see TELEMETRY.md)." >&2
fi

exec ./target/release/can_xbee_bridge "$@"

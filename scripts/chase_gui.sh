#!/usr/bin/env bash
# Chase-side telemetry dashboard GUI (mac or Linux laptop).
# Same layout as the car's onboard display, fed by the XBee link.
#
#   ./scripts/chase_gui.sh
set -euo pipefail

cd "$(dirname "$0")/.."

export XBEE_BAUD="${XBEE_BAUD:-115200}"
export KEYS_DIR="${KEYS_DIR:-$PWD/keys}"

case "$(uname -s)" in
    Linux)
        # Avoid wgpu GPU-memory failures on Raspberry Pi desktops. The iced
        # dependency already includes tiny-skia through its default features.
        export ICED_BACKEND="${ICED_BACKEND:-tiny-skia}"
        ;;
esac

if [[ -z "${XDG_RUNTIME_DIR:-}" ]]; then
    runtime_dir="/run/user/$(id -u)"
    if [[ -d "$runtime_dir" ]]; then
        export XDG_RUNTIME_DIR="$runtime_dir"
    fi
fi

if [[ -z "${WAYLAND_DISPLAY:-}" && -z "${WAYLAND_SOCKET:-}" && -z "${DISPLAY:-}" ]]; then
    if [[ -n "${XDG_RUNTIME_DIR:-}" ]]; then
        for wayland_socket in "$XDG_RUNTIME_DIR"/wayland-*; do
            if [[ -S "$wayland_socket" ]]; then
                export WAYLAND_DISPLAY="${wayland_socket##*/}"
                break
            fi
        done
    fi
fi

if [[ -z "${WAYLAND_DISPLAY:-}" && -z "${WAYLAND_SOCKET:-}" && -z "${DISPLAY:-}" ]]; then
    case "$(uname -s)" in
        Linux)
            # Raspberry Pi desktop sessions are commonly reachable as :0 from
            # an SSH shell, matching the onboard dashboard launch command.
            export DISPLAY=:0
            ;;
    esac
fi

if [[ -z "${WAYLAND_DISPLAY:-}" && -z "${WAYLAND_SOCKET:-}" && -z "${DISPLAY:-}" ]]; then
    cat >&2 <<'EOF'
ERROR: no graphical display is available.

telemetry_gui needs a Pi desktop/Wayland/X11 session. Run this script from a
terminal on the Pi's desktop, connect with SSH X forwarding (`ssh -Y pi@...`),
or use `./scripts/chase_receiver.sh` for headless text output.

If a desktop is already running for this same user, try:
  XDG_RUNTIME_DIR=/run/user/$(id -u) WAYLAND_DISPLAY=wayland-0 ./scripts/chase_gui.sh
or:
  DISPLAY=:0 ./scripts/chase_gui.sh
EOF
    exit 1
fi

cargo build --release -p telemetry_gui

if [[ ! -f "$KEYS_DIR/authorized_sender.pub" ]]; then
    echo "WARNING: $KEYS_DIR/authorized_sender.pub missing — copy the car's" >&2
    echo "         sender_ed25519.pub there (see TELEMETRY.md)." >&2
fi

exec ./target/release/telemetry_gui "$@"

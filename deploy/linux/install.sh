#!/usr/bin/env bash
# install.sh — install mossraven-node as a systemd service.
#
# Run as root. Expects:
#   - the mossraven-node binary in the current directory (or pass --binary)
#   - PathOfBuilding-PoE2 already cloned somewhere
#
# Usage:
#   sudo ./install.sh --bearer <secret> --pob-path /path/to/PathOfBuilding-PoE2 \
#                     [--binary ./mossraven-node] [--bind 0.0.0.0:5380]

set -euo pipefail

BEARER=""
POB_PATH=""
BINARY="./mossraven-node"
BIND="0.0.0.0:5380"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --bearer)   BEARER="$2"; shift 2 ;;
    --pob-path) POB_PATH="$2"; shift 2 ;;
    --binary)   BINARY="$2"; shift 2 ;;
    --bind)     BIND="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

[[ -z "$BEARER"   ]] && { echo "--bearer is required"   >&2; exit 2; }
[[ -z "$POB_PATH" ]] && { echo "--pob-path is required" >&2; exit 2; }
[[ ! -x "$BINARY" ]] && { echo "binary not found at $BINARY" >&2; exit 2; }
[[ ! -d "$POB_PATH" ]] && { echo "pob path not found at $POB_PATH" >&2; exit 2; }

if [[ $EUID -ne 0 ]]; then
  echo "run as root (or with sudo)" >&2
  exit 2
fi

# Create dedicated user
if ! id mossraven &>/dev/null; then
  useradd --system --no-create-home --shell /usr/sbin/nologin mossraven
fi

# Install layout
install -d -o mossraven -g mossraven /opt/mossraven
install -d -o root -g root -m 755 /etc/mossraven

install -o mossraven -g mossraven -m 755 "$BINARY" /opt/mossraven/mossraven-node

# Vendored PoB2 — symlink rather than copy so updates are atomic
ln -sfn "$(realpath "$POB_PATH")" /opt/mossraven/PathOfBuilding-PoE2

# Env file (perms 0640 because it holds the bearer)
cat > /etc/mossraven/node.env <<EOF
MOSSRAVEN_NODE_BEARER=$BEARER
MOSSRAVEN_NODE_BIND=$BIND
MOSSRAVEN_POB_PATH=/opt/mossraven/PathOfBuilding-PoE2
RUST_LOG=info
EOF
chown root:mossraven /etc/mossraven/node.env
chmod 0640 /etc/mossraven/node.env

# Install the unit
install -o root -g root -m 644 "$(dirname "$0")/mossraven-node.service" /etc/systemd/system/

systemctl daemon-reload
systemctl enable --now mossraven-node.service

echo
echo "mossraven-node is up. status:"
systemctl --no-pager status mossraven-node.service | sed -n '1,8p'
echo
echo "health check:"
curl -sS "http://localhost:${BIND##*:}/health" || true
echo

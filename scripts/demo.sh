#!/usr/bin/env bash
# Starts the signaling server and two LocalSync app instances (sender +
# receiver) as separate processes on this machine, each with its own
# LOCALSYNC_DATA_DIR so they don't collide - see README.md for the manual
# steps once both windows are up.
set -euo pipefail
cd "$(dirname "$0")/.."

SIGNALING_PORT="${SIGNALING_PORT:-9090}"
WORK_DIR="$(mktemp -d)"
echo "Instance data dirs under: $WORK_DIR"

echo "Starting signaling server on ws://localhost:$SIGNALING_PORT ..."
PORT="$SIGNALING_PORT" node apps/signaling-server/index.js &
SIGNALING_PID=$!

cleanup() {
    echo "Stopping signaling server (pid $SIGNALING_PID) ..."
    kill "$SIGNALING_PID" 2>/dev/null || true
}
trap cleanup EXIT

sleep 1

echo "Starting sender instance ..."
LOCALSYNC_DATA_DIR="$WORK_DIR/sender" cargo run -p localsync-desktop &
SENDER_PID=$!

sleep 2

echo "Starting receiver instance ..."
LOCALSYNC_DATA_DIR="$WORK_DIR/receiver" cargo run -p localsync-desktop &
RECEIVER_PID=$!

echo
echo "Two LocalSync windows should be open now. In the sender window:"
echo "  1. Send tab -> project folder: $(pwd)/sample-project, generate a room code, click Send"
echo "In the receiver window:"
echo "  2. Receive tab -> paste the same room code, click Receive"
echo "  3. Review the diff, click Run"
echo "  4. curl http://localhost:8080/api/notes (port shown in the app once running)"
echo
echo "Ctrl+C to stop everything."

wait "$SENDER_PID" "$RECEIVER_PID"

#!/usr/bin/env bash
# Runs a local coturn TURN server as a Podman container, for interactive/dev
# use of ls-net's TURN fallback (crates/ls-net's ice_config() reads
# LOCALSYNC_TURN_* env vars - see its doc comment). Not used by any
# automated test; this is for a human to point a real client at.
#
# Must use the fully-qualified `docker.io/coturn/coturn` image name - this
# box's rootless Podman has no unqualified-search-registries configured, so
# a short name like `coturn/coturn` fails to resolve.
#
# After running this, connect with:
#   LOCALSYNC_TURN_URL=turn:localhost:3478
#   LOCALSYNC_TURN_USERNAME=localsync
#   LOCALSYNC_TURN_CREDENTIAL=localsync-turn-pw
set -euo pipefail

podman run -d --name localsync-turn --replace \
  -p 3478:3478/udp -p 3478:3478/tcp \
  -p 49160-49200:49160-49200/udp \
  docker.io/coturn/coturn \
  -n --log-file=stdout --lt-cred-mech \
  --realm=localsync.test --user=localsync:localsync-turn-pw \
  --no-cli --min-port=49160 --max-port=49200

echo "started localsync-turn - stop with: podman rm -f localsync-turn"

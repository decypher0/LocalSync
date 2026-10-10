# Self-hosting a Remote relay

**Local network** mode needs nothing extra. The app runs its own relay
(`ls_net::host_ephemeral_relay`, pure Rust) and puts its LAN address in the
14-character code.

**Remote relay** mode is for peers on different networks. The app has **no
built-in default relay**. Both people type the same relay URL into the app
(Settings / the Send wizard / Receive), and the code they share is only a
4-character room id on that relay.

The relay only forwards the WebRTC handshake (SDP). Project bytes always go
peer-to-peer over the data channel.

## Run it

```sh
cd apps/signaling-server
npm ci
PORT=9090 node index.js     # PORT defaults to 9090
```

Then use `ws://<host>:9090` in the app, and open that TCP port in the host's
firewall.

## TLS (`wss://`)

`index.js` speaks plain `ws://`. To get `wss://`, put it behind any reverse
proxy that terminates TLS and forwards WebSocket upgrades (Caddy, nginx,
Cloudflare, or a PaaS's built-in HTTPS). Then use `wss://relay.example.com`
in the app. The app supports `wss://` from the build that enabled
tokio-tungstenite's `native-tls` feature in `crates/ls-net/Cargo.toml`;
older builds only accept `ws://`.

## Limits

- Each message can be at most 64 KiB, and at most 16 messages are queued
  per unpaired room. Real handshakes are a few KB.
- There is no auth, and room ids are 4 characters. Anyone who knows the URL
  can use the relay, and anyone who guesses a live room id can join it.
  Snapshots are signed, and the receiver sees the sender's identity (known
  or new) on the review screen before anything runs. So check that screen,
  and prefer a private relay URL over a public one.

## Strict NATs (TURN)

STUN (`stun:stun.l.google.com:19302`) is built in. If neither peer can be
reached directly, a TURN server is needed. Today TURN can only be set
through environment variables, which must be in the app's environment when
it starts:

```
LOCALSYNC_TURN_URL=turn:turn.example.com:3478
LOCALSYNC_TURN_USERNAME=...
LOCALSYNC_TURN_CREDENTIAL=...
```

All three must be set, or TURN is skipped. On macOS, an app launched from
Finder does not see variables set in your shell profile.

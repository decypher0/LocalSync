// Minimal WebRTC signaling relay.
//
// A client connects to ws://host:port/<room-code>. The server pairs the
// first two clients that join the same room code, then blindly forwards any
// JSON message one sends to the other: {type: "offer"|"answer"|"ice-candidate", data: ...}.
//
// This process never inspects payload contents beyond routing, has no auth,
// and holds nothing in memory beyond the currently-open rooms - it only ever
// carries handshake metadata (SDP/ICE), never project code.
const WebSocket = require('ws');

const PORT = process.env.PORT || 8080;
const wss = new WebSocket.Server({ port: PORT });

// roomCode -> { clients: [ws, ...], queue: [msg, ...] }
// `clients` holds up to 2 sockets. `queue` buffers messages sent by the
// first client before the second has joined, so trickled messages (e.g. ICE
// candidates generated right after connecting) aren't dropped to a race.
const rooms = new Map();

function send(ws, msg) {
  if (ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(msg));
}

function roomCodeFromUrl(url) {
  return decodeURIComponent((url || '/').split('?')[0].slice(1));
}

wss.on('connection', (ws, req) => {
  const room = roomCodeFromUrl(req.url);
  if (!room) {
    ws.close(1008, 'room code required');
    return;
  }

  let entry = rooms.get(room);
  if (!entry) {
    entry = { clients: [], queue: [] };
    rooms.set(room, entry);
  }

  if (entry.clients.length >= 2) {
    ws.close(1008, 'room full');
    return;
  }

  entry.clients.push(ws);
  console.log(`[join] room=${room} peers=${entry.clients.length}`);

  if (entry.clients.length === 2) {
    const [first, second] = entry.clients;
    first.peer = second;
    second.peer = first;
    for (const msg of entry.queue) send(second, msg);
    entry.queue = [];
  }

  ws.on('message', (raw) => {
    let msg;
    try {
      msg = JSON.parse(raw);
    } catch {
      return; // not our problem - ignore anything that isn't JSON
    }
    if (ws.peer) {
      send(ws.peer, msg);
    } else {
      entry.queue.push(msg);
    }
  });

  ws.on('close', () => {
    console.log(`[leave] room=${room}`);
    if (ws.peer) {
      send(ws.peer, { type: 'peer-disconnected' });
      ws.peer.peer = null;
    }
    rooms.delete(room);
  });
});

console.log(`signaling server listening on ws://0.0.0.0:${PORT}`);

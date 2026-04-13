# watch-party — Protocol & Architecture Design

## Overview

`watch-party` is an open-source, P2P encrypted watch party application for physical media.
A host loads a disc into their optical drive and streams it in real time to a closed group
of trusted peers. Identity and trust are anchored to PGP keypairs. No central server.
No subscription. No DRM middleman.

---

## Two-Window Model

- **TUI** (`ratatui`): The "control room." Peer list, connection status, playback controls,
  sync indicators, invite/key management, side-channel chat (future).
- **Video window** (`mpv`): Spawned as a separate process. Controlled via mpv's IPC socket
  interface (`--input-ipc-server`). Host and peers each get their own mpv window.

---

## Identity & Trust

- Each peer has a PGP keypair. Key exchange is manual (out-of-band), consistent with
  the pgp-chat ecosystem.
- Session invitations are PGP-signed and encrypted to each peer's public key.
- The host maintains a local keyring of trusted peers.
- Only keyring members can join a watch party.

---

## Session Manifest

The host performs pregame analysis before any stream data flows. Based on what the
probe yields, it emits one of two manifest types:

### VodManifest
Full analysis available. Enables seeking, progress bar with known endpoint, chapter
display, reconnect-by-chunk-index, drift correction against known timeline.

### LiveManifest
Degraded/unknown duration (damaged disc, no container metadata). Disables seeking.
Progress shows elapsed time only. Peers treat it like a live stream.

The host always attempts VOD first. LiveManifest is a last resort. `allow_live_fallback`
in HostConfig controls whether the host aborts or degrades when duration probe fails.

---

## Chunk Boundaries

Boundaries are **time-derived**, not metadata-derived. This avoids breakage on discs
with no chapter marks, no menu, or incomplete metadata.

```
total_chunks = ceil(duration_secs / (chunk_duration_ms / 1000.0))
```

If `keyframe_snap = true`, each calculated boundary is snapped to the nearest keyframe
within `max_snap_delta_ms`. The `snap_delta_ms` field (signed) records the direction
and magnitude of each snap.

Chapter markers from disc metadata are **annotations** snapped to the nearest chunk
boundary after the fact — display-only, never load-bearing for the protocol.

### ChunkBoundary fields of note

- `byte_offset` / `byte_length`: Reserved for future peer-has-local-disc mode. Populated
  when `VodManifest.byte_map_available = true`.
- `snap_delta_ms`: Signed. Negative = snapped earlier. Positive = snapped later.
- `is_chapter_start` / `chapter_title`: Annotation only.

---

## Pregame Protocol Sequence

```
Host                                    Peer(s)
  |                                        |
  |--- SessionManifest (PGP-signed) ------>|
  |                                        | verify host sig, parse manifest
  |<-- ManifestAck (PGP-signed) -----------|
  |    (receipt confirmation only)         |
  |                                        |
  |--- CapabilityChallenge --------------->|
  |    (echoes media_hash, sets deadline)  |
  |                                        | check local disc, assess bandwidth/player
  |<-- CapabilityResponse (PGP-signed) ----|
  |                                        |
  | host evaluates responses               |
  | builds per-peer role assignments       |
  |                                        |
  |--- SessionRoles (per peer) ----------->|
  |--- StreamReady (stream_start_utc) ---->|
  |                                        |
  |--- StreamChunk seq=0 ---------------->|
  |--- StreamChunk seq=1 ---------------->|
  |          ...                           |
  |--- SyncBeacon (every N ms) ----------->|
  |<-- PeerStatus --------------------------|
```

---

## Peer Roles

| Role | Description |
|------|-------------|
| `StreamReceiver` | Normal mode. Receives encrypted stream chunks from host. |
| `LocalDiscSync` | Peer has verified local disc (media_hash matched). Receives SyncBeacons only; reads from own drive. |

Future: `Relay` — receives stream and rebroadcasts to other peers.

---

## Sync Model

- Host is authoritative.
- `SyncBeacon` broadcasts `host_pts`, `host_chunk_seq`, `playing`, `timestamp_utc` on
  a configurable interval (default 5s).
- Peers respond with `PeerStatus` reporting their current pts and buffer depth.
- Peers drifting beyond `max_drift_ms` receive corrective action via mpv IPC
  (nudge playback rate or jump position).
- `stream_start_utc` in `StreamReady` gives all peers a synchronized wall-clock
  start moment before the beacon loop begins.

---

## Host Configuration (`watch-party.toml`)

```toml
[stream]
chunk_duration_ms = 2000          # target chunk size
max_snap_delta_ms = 500           # max keyframe snap distance
keyframe_snap = true              # snap boundaries to keyframes

[buffer]
min_buffer_chunks = 5             # chunks before playback begins
max_buffer_chunks = 30            # max chunks to buffer ahead

[pregame]
manifest_ack_deadline_ms = 10000          # wait for ack
capability_response_deadline_ms = 15000   # wait for capability check
peer_join_window_ms = 30000               # wait for peers to connect

[sync]
sync_beacon_interval_ms = 5000    # beacon frequency
max_drift_ms = 3000               # drift tolerance before correction

[media]
media_hash_bytes = 10485760       # bytes to hash for identity (10MB)
allow_live_fallback = true        # degrade to live mode if probe fails
```

---

## Wire Format

Each message: `[4-byte LE length] [1-byte type tag] [bincode payload]`

The entire framed message rides inside the encrypted TCP session channel.
Encryption wraps the transport layer; individual fields are not separately encrypted
(except the manifest itself, which is PGP-signed as a standalone artifact).

Serialization: `bincode`
Hash: `blake3`
Integrity: `HMAC-SHA256` on StreamChunk payloads

---

## Project Structure

```
watch-party/
├── src/
│   ├── main.rs
│   ├── core/
│   │   ├── mod.rs
│   │   ├── config.rs      — HostConfig, all tunable fields
│   │   ├── identity.rs    — PeerIdentity, keyring
│   │   ├── manifest.rs    — SessionManifest, VodManifest, LiveManifest, ChunkBoundary
│   │   ├── messages.rs    — all WireMessage variants
│   │   └── session.rs     — HostSessionState, PeerSessionState
│   ├── disc/              — optical drive abstraction, ffprobe bridge
│   ├── stream/            — encode pipeline, chunking, network send/recv
│   ├── sync/              — conductor protocol, drift correction, mpv IPC
│   ├── tui/               — ratatui UI, event loop, TUI state machine
│   └── player/            — mpv process management, IPC socket wrapper
├── docs/
│   └── DESIGN.md          — this document
├── .vscode/
│   ├── settings.json
│   └── extensions.json
├── Cargo.toml
└── .gitignore
```

---

## Next Subsystems to Implement

1. **disc/** — ffprobe analysis pipeline, keyframe scanning, chunk map builder
2. **stream/** — FFmpeg encode pipeline, TCP transport, chunk framing
3. **player/** — mpv process spawn, IPC socket, seek/pause/nudge commands
4. **sync/** — SyncBeacon loop, drift detection, corrective action
5. **tui/** — ratatui layout, host state display, peer status panel

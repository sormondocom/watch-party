//! All wire message types for the watch-party protocol.
//!
//! Wire framing: 4-byte LE length prefix | 1-byte message type tag | bincode payload.
//! The entire framed message rides inside the encrypted TCP session channel.

use serde::{Deserialize, Serialize};
use crate::core::manifest::SessionManifest;

/// Top-level wire message enum.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WireMessage {
    // ── Pregame phase ─────────────────────────────────────────────────────────
    Manifest(SessionManifest),
    ManifestAck(ManifestAck),
    CapabilityChallenge(CapabilityChallenge),
    CapabilityResponse(CapabilityResponse),
    SessionRoles(SessionRoles),
    StreamReady(StreamReady),

    // ── Stream phase ──────────────────────────────────────────────────────────
    Chunk(StreamChunk),
    SyncBeacon(SyncBeacon),
    PeerStatus(PeerStatus),

    // ── Control ───────────────────────────────────────────────────────────────
    Pause(SessionControl),
    Resume(SessionControl),
    Seek(SeekControl),
    End(SessionControl),
}

// ── Pregame messages ──────────────────────────────────────────────────────────

/// Sent by peer immediately after receiving and verifying the SessionManifest.
/// Sole purpose: confirm identity and that the correct manifest was received.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestAck {
    pub session_id: [u8; 32],
    pub peer_fingerprint: String,
    /// blake3 of received manifest bytes.
    /// Host verifies this matches its own manifest hash before proceeding.
    pub manifest_hash: [u8; 32],
    pub timestamp_utc: u64,
}

/// Sent by host to all acked peers to begin capability negotiation.
/// Peers must respond within response_deadline_ms or receive default role assignment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityChallenge {
    pub session_id: [u8; 32],
    /// Echoed from VodManifest so peer knows which disc to check locally.
    pub media_hash: [u8; 32],
    /// How long (ms) the peer has to respond. Configurable via HostConfig::pregame.
    pub response_deadline_ms: u32,
}

/// Peer's self-reported capabilities in response to CapabilityChallenge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityResponse {
    pub session_id: [u8; 32],
    pub peer_fingerprint: String,

    // local disc capability
    /// Peer reports it has a local optical drive with media loaded.
    pub has_local_disc: bool,
    /// Peer hashed its local disc against media_hash from the challenge and it matched.
    /// Host assigns LocalDiscSync role only when this is true.
    pub local_disc_verified: bool,

    // network self-assessment
    /// Peer's honest self-reported available bandwidth in kbps.
    pub estimated_bandwidth_kbps: Option<u32>,
    /// Peer's preferred buffer depth in chunks.
    pub preferred_buffer_chunks: Option<u16>,

    // playback capability
    pub can_seek: bool,
    pub player_type: PlayerType,

    pub timestamp_utc: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PlayerType {
    Mpv,
    External, // reserved for future non-mpv players
    Unknown,
}

/// Host assigns each peer a role based on their CapabilityResponse.
/// Sent individually per peer so roles may differ across the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionRoles {
    pub session_id: [u8; 32],
    pub peer_fingerprint: String,
    pub role: PeerRole,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PeerRole {
    /// Normal mode — receives stream chunks from host.
    StreamReceiver,
    /// Peer has verified local disc — receives SyncBeacons only, reads locally.
    LocalDiscSync,
    // future: Relay — receives stream and rebroadcasts to other peers
}

/// Final pregame message. All peers synchronize playback start to stream_start_utc.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamReady {
    pub session_id: [u8; 32],
    /// Wall-clock UTC Unix timestamp. All peers begin playback at this moment.
    pub stream_start_utc: u64,
}

// ── Stream messages ───────────────────────────────────────────────────────────

/// A single encoded, encrypted chunk of the video stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    pub session_id: [u8; 32],
    /// Zero-based chunk sequence number. Gaps indicate dropped chunks.
    pub sequence: u64,
    /// Which chapter this chunk belongs to. Display use only.
    pub chapter_index: u16,
    /// Presentation timestamp in seconds.
    pub pts: f64,
    /// True if this chunk begins on a keyframe and can be decoded independently.
    pub keyframe: bool,
    /// Encrypted encoded video payload.
    pub payload: Vec<u8>,
    /// HMAC-SHA256 over session_id || sequence || payload.
    pub hmac: [u8; 32],
}

/// Periodic host broadcast. Peers use this to detect and correct drift.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncBeacon {
    pub session_id: [u8; 32],
    pub host_pts: f64,
    pub host_chunk_seq: u64,
    pub playing: bool,
    pub timestamp_utc: u64,
}

/// Peer reports its current playback state in response to SyncBeacon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerStatus {
    pub session_id: [u8; 32],
    pub peer_fingerprint: String,
    pub current_pts: f64,
    /// How many chunks ahead the peer currently has buffered.
    pub buffer_depth_chunks: u16,
    pub state: PeerPlaybackState,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PeerPlaybackState {
    Playing,
    Buffering,
    Paused,
    Error(String),
}

// ── Control messages ──────────────────────────────────────────────────────────

/// Generic host control message (pause, resume, end).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionControl {
    pub session_id: [u8; 32],
    pub timestamp_utc: u64,
}

/// Host-initiated seek. Only valid in VOD sessions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeekControl {
    pub session_id: [u8; 32],
    /// Target chunk sequence index from VodManifest::chunk_map.
    pub target_sequence: u64,
    pub target_pts: f64,
    pub timestamp_utc: u64,
}

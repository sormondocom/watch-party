//! Session manifests — VOD and Live variants.

use serde::{Deserialize, Serialize};

/// Top-level session manifest enum.
///
/// `Vod`  — full pregame analysis available: known duration, chunk map, seeking enabled.
/// `Live` — degraded/unknown duration: stream as it arrives, no seeking, no chunk map.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SessionManifest {
    Vod(VodManifest),
    Live(LiveManifest),
}

impl SessionManifest {
    pub fn session_id(&self) -> &[u8; 32] {
        match self {
            Self::Vod(m) => &m.session_id,
            Self::Live(m) => &m.session_id,
        }
    }

    pub fn host_fingerprint(&self) -> &str {
        match self {
            Self::Vod(m) => &m.host_fingerprint,
            Self::Live(m) => &m.host_fingerprint,
        }
    }

    pub fn chunk_duration_ms(&self) -> u32 {
        match self {
            Self::Vod(m) => m.chunk_duration_ms,
            Self::Live(m) => m.chunk_duration_ms,
        }
    }

    pub fn stream_start_utc(&self) -> u64 {
        match self {
            Self::Vod(m) => m.stream_start_utc,
            Self::Live(m) => m.stream_start_utc,
        }
    }

    pub fn min_buffer_chunks(&self) -> u16 {
        match self {
            Self::Vod(m) => m.min_buffer_chunks,
            Self::Live(m) => m.min_buffer_chunks,
        }
    }
}

// ── VOD Manifest ──────────────────────────────────────────────────────────────

/// Full VOD manifest — complete pregame analysis available.
/// Enables: seeking, progress bar with known endpoint, chapter display,
/// reconnect-by-chunk-index, drift correction against known timeline.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VodManifest {
    // identity
    pub session_id: [u8; 32],
    pub host_fingerprint: String,
    /// blake3 hash of first N bytes of disc/file — identity anchor.
    pub media_hash: [u8; 32],

    // complete timing picture
    pub duration_secs: f64,
    pub chunk_duration_ms: u32,
    pub total_chunks: u64,
    /// Full chunk index with keyframe-snapped boundaries.
    pub chunk_map: Vec<ChunkBoundary>,

    // media properties
    pub video_codec: String,
    pub resolution: (u16, u16),
    pub framerate: f32,
    pub avg_bitrate_kbps: u32,
    pub peak_bitrate_kbps: u32,
    pub audio_tracks: Vec<AudioTrack>,

    // session coordination
    pub stream_start_utc: u64,
    pub sync_beacon_interval_ms: u32,
    pub min_buffer_chunks: u16,
    pub max_buffer_chunks: u16,

    // keyframe snap config echoed back so peers know the constraints used
    pub keyframe_snap: bool,
    pub max_snap_delta_ms: u32,

    /// True when byte_offset/byte_length are populated in chunk_map.
    /// Capability gate for future peer-has-local-disc mode.
    pub byte_map_available: bool,
}

// ── Live Manifest ─────────────────────────────────────────────────────────────

/// Degraded manifest for unknown-duration streams.
/// Disables: seeking, progress bar endpoint, chapter display, reconnect-by-index.
/// Enables: elapsed-only progress, depth-based buffer display, live-position rejoin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveManifest {
    // identity
    pub session_id: [u8; 32],
    pub host_fingerprint: String,
    /// Best-effort — may not be available for damaged/incomplete discs.
    pub media_hash: Option<[u8; 32]>,

    // what we do know
    pub chunk_duration_ms: u32,
    pub video_codec: String,
    /// May not be known until stream starts.
    pub resolution: Option<(u16, u16)>,
    pub framerate: Option<f32>,
    pub estimated_bitrate_kbps: Option<u32>,
    pub audio_tracks: Vec<AudioTrack>,

    // coordination — simplified, no seeking
    pub stream_start_utc: u64,
    pub sync_beacon_interval_ms: u32,
    pub min_buffer_chunks: u16,
    // Note: max_buffer_chunks intentionally absent — meaningless without known duration
}

// ── Shared types ──────────────────────────────────────────────────────────────

/// A single entry in VodManifest::chunk_map.
/// Chunk boundaries are time-derived; chapter markers are annotations snapped
/// to the nearest boundary — never load-bearing for the stream protocol.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkBoundary {
    // ordering and timing
    /// Zero-based chunk sequence number.
    pub sequence: u64,
    /// Actual keyframe-snapped presentation timestamp in seconds.
    pub pts_secs: f64,
    /// Actual duration of this chunk. May vary from target due to keyframe snapping.
    pub duration_secs: f64,

    // local disc support — reserved for future peer-has-disc mode
    /// Byte offset into the disc/file stream. None if byte map not probed.
    pub byte_offset: Option<u64>,
    /// Byte length of this chunk on disc. None if byte map not probed.
    pub byte_length: Option<u64>,

    // keyframe status
    /// True if this boundary was successfully snapped to a keyframe.
    pub keyframe_snapped: bool,
    /// How far (ms) we moved to find the nearest keyframe.
    /// Negative = moved earlier than calculated boundary.
    /// Positive = moved later.
    pub snap_delta_ms: i32,

    // chapter annotation — display only, not structural
    /// True if a chapter boundary falls within this chunk.
    pub is_chapter_start: bool,
    /// Which chapter index begins at this chunk. Display use only.
    pub chapter_index: Option<u16>,
    /// Chapter title from disc metadata, if available.
    pub chapter_title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioTrack {
    pub index: u16,
    pub codec: String,
    pub language: Option<String>,
    pub channels: u8,
}

//! Host configuration — loaded from watch-party.toml

use serde::{Deserialize, Serialize};

/// Full host configuration, persisted to watch-party.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostConfig {
    pub stream: StreamConfig,
    pub buffer: BufferConfig,
    pub pregame: PregameConfig,
    pub sync: SyncConfig,
    pub media: MediaConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamConfig {
    /// Target chunk duration in milliseconds. Default: 2000.
    pub chunk_duration_ms: u32,
    /// Maximum distance in ms to snap a boundary to the nearest keyframe. Default: 500.
    pub max_snap_delta_ms: u32,
    /// Whether to snap chunk boundaries to keyframes. Default: true.
    pub keyframe_snap: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BufferConfig {
    /// Minimum number of chunks a peer should buffer before playing. Default: 5.
    pub min_buffer_chunks: u16,
    /// Maximum number of chunks a peer will buffer ahead. Default: 30.
    pub max_buffer_chunks: u16,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PregameConfig {
    /// How long (ms) to wait for ManifestAck from each peer. Default: 10000.
    pub manifest_ack_deadline_ms: u32,
    /// How long (ms) peers have to respond to CapabilityChallenge. Default: 15000.
    pub capability_response_deadline_ms: u32,
    /// How long (ms) to wait for all invited peers to connect before starting pregame. Default: 30000.
    pub peer_join_window_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    /// How often (ms) the host sends a SyncBeacon. Default: 5000.
    pub sync_beacon_interval_ms: u32,
    /// Maximum drift (ms) before corrective action is taken. Default: 3000.
    pub max_drift_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaConfig {
    /// Number of bytes to read from disc for media_hash identity. Default: 10485760 (10MB).
    pub media_hash_bytes: u64,
    /// If true, fall back to LiveManifest when duration probe fails. Default: true.
    pub allow_live_fallback: bool,
}

impl Default for HostConfig {
    fn default() -> Self {
        Self {
            stream: StreamConfig {
                chunk_duration_ms: 2000,
                max_snap_delta_ms: 500,
                keyframe_snap: true,
            },
            buffer: BufferConfig {
                min_buffer_chunks: 5,
                max_buffer_chunks: 30,
            },
            pregame: PregameConfig {
                manifest_ack_deadline_ms: 10_000,
                capability_response_deadline_ms: 15_000,
                peer_join_window_ms: 30_000,
            },
            sync: SyncConfig {
                sync_beacon_interval_ms: 5_000,
                max_drift_ms: 3_000,
            },
            media: MediaConfig {
                media_hash_bytes: 10_485_760,
                allow_live_fallback: true,
            },
        }
    }
}

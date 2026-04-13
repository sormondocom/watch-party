//! Session state machines for host and peer sides.

/// Host pregame and stream state machine.
///
/// Transitions:
///   Analyzing → Announcing → AwaitingAcks → Challenging → Assigning → Ready → Streaming → Ended
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostSessionState {
    /// Probing disc/file via ffprobe. Building manifest.
    Analyzing,
    /// Manifest built. Waiting for invited peers to connect within peer_join_window_ms.
    Announcing,
    /// Manifest sent to all connected peers. Waiting for ManifestAck from each.
    AwaitingAcks,
    /// All acks received. CapabilityChallenge sent. Countdown timer running.
    Challenging,
    /// Capability responses received (or deadline elapsed). Building role assignments.
    Assigning,
    /// SessionRoles sent to all peers. StreamReady queued. Ready to begin.
    Ready,
    /// Stream is live. Chunks flowing. SyncBeacon loop running.
    Streaming,
    /// Stream ended normally or aborted.
    Ended,
}

/// Peer session state machine.
///
/// Transitions:
///   AwaitingManifest → AssessingCapabilities → AwaitingRole → AwaitingStream
///     → Buffering → Playing ↔ Paused → Ended
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PeerSessionState {
    /// Waiting to receive SessionManifest from host.
    AwaitingManifest,
    /// Manifest received and verified. ManifestAck sent. Assessing local capabilities
    /// (disc hash check, player detection, bandwidth estimate).
    AssessingCapabilities,
    /// CapabilityResponse sent. Waiting for SessionRoles from host.
    AwaitingRole,
    /// Role assigned. Waiting for StreamReady.
    AwaitingStream,
    /// StreamReady received. Filling buffer to min_buffer_chunks before playback.
    Buffering,
    /// Actively playing.
    Playing,
    /// Paused by host SessionControl.
    Paused,
    /// Stream ended or connection lost.
    Ended,
}

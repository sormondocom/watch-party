//! mpv process lifecycle and chunk feeding for the watch-party view window.
//!
//! # View window design
//!
//! The "view window" is a separate mpv process that displays both **video and
//! audio** for the session. It is owned by the player layer; the TUI is the
//! control room, mpv is the theatre screen.
//!
//! ```text
//!   Host                           Peer
//!   ────────────────────────────   ──────────────────────────────
//!   encode_chunk (ffmpeg) ──────►  StreamChunk (TCP, framed)
//!                                     │
//!                                     ▼
//!                                  verify_hmac
//!                                     │
//!                                     ▼
//!                                  MpvPlayer::play_chunk
//!                                     │ write TS bytes to stdin
//!                                     ▼
//!                                  mpv  ◄── IPC socket (pause/seek/query)
//!                                  (video + audio window)
//! ```
//!
//! # Audio intentionally included
//!
//! Both audio and video run through the same TS pipe. Any chunk gaps or
//! ordering problems manifest immediately as audio artifacts — the easiest
//! real-time indicator that stream quality has degraded.
//!
//! # Platform IPC
//! - **Unix** — Unix domain socket at `/tmp/watch-party-<pid>.sock`
//! - **Windows** — named pipe at `\\.\pipe\watch-party-<pid>`
//!
//! See [`ipc`] for the IPC client implementation.

pub mod ipc;

use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::AsyncWriteExt;
use tokio::process::{Child, ChildStdin, Command};
use tracing::{info, warn};

use crate::core::messages::StreamChunk;
use crate::stream::framing::verify_hmac;

pub use ipc::MpvIpc;

// ── PlaybackState ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackState {
    /// mpv is running but has not yet received any chunks.
    Buffering,
    /// mpv is actively playing.
    Playing,
    /// Playback is paused via IPC.
    Paused,
    /// mpv exited or was stopped.
    Stopped,
}

impl PlaybackState {
    pub fn icon(&self) -> &'static str {
        match self {
            Self::Buffering => "⏳",
            Self::Playing => "▶",
            Self::Paused => "⏸",
            Self::Stopped => "■",
        }
    }
}

// ── MpvPlayer ─────────────────────────────────────────────────────────────────

/// A running mpv instance configured for watch-party playback.
///
/// mpv reads media from its **stdin** (continuous MPEG-TS) and accepts
/// control commands on its **IPC socket**. The caller feeds [`StreamChunk`]
/// payloads via [`MpvPlayer::play_chunk`]; the sync module queries
/// [`MpvPlayer::playback_time`] for drift detection.
pub struct MpvPlayer {
    /// The mpv child process. `kill_on_drop(true)` so it is cleaned up if
    /// [`MpvPlayer`] is dropped without an explicit [`stop`].
    process: Child,
    /// Write half of mpv's stdin — receives MPEG-TS chunk payloads.
    stdin: ChildStdin,
    /// IPC handle for pause/resume/seek/query.
    pub ipc: MpvIpc,
    /// Current playback state as tracked by this side (not queried from mpv).
    pub state: PlaybackState,
    /// Session id used to verify incoming chunk HMACs.
    session_id: [u8; 32],
    /// Running count of chunks successfully written to mpv.
    pub chunks_fed: u64,
    /// Running count of chunks rejected due to HMAC failure.
    pub chunks_rejected: u64,
}

impl MpvPlayer {
    /// Spawn mpv and connect to its IPC socket.
    ///
    /// ## mpv arguments
    /// - `--no-terminal` — keep our TUI terminal clean
    /// - `--force-window=yes` — open the video window immediately, before any
    ///   data arrives, so the peer sees the window appear at the same moment
    ///   the stream begins
    /// - `--demuxer-lavf-format=mpegts` — tell libavformat what to expect on
    ///   stdin (avoids probe delay at stream start)
    /// - `--audio-display=no` — show the video window, not an album-art pane
    /// - `--title=Watch Party` — window title visible in the OS taskbar
    /// - `--input-ipc-server=<path>` — control socket
    /// - `-` — read media from stdin
    ///
    /// Returns an error if mpv is not in PATH or fails to start.
    pub async fn spawn(session_id: &[u8; 32]) -> Result<Self> {
        let pid = std::process::id();
        let ipc_path = ipc::ipc_path(pid);

        let mut child = Command::new("mpv")
            // Keep our TUI terminal clean — mpv must not write to stdout/stderr.
            .arg("--no-terminal")
            // Open the video window immediately so the peer has visual feedback
            // before the stream begins arriving.
            .arg("--force-window=yes")
            // Hint the demuxer so mpv doesn't need to probe stdin for the format.
            .arg("--demuxer-lavf-format=mpegts")
            // Don't overlay album art — this is a video session.
            .arg("--audio-display=no")
            // Window title in the OS taskbar.
            .arg("--title=Watch Party")
            // Keep playing even if the data source stalls briefly between chunks.
            .arg("--demuxer-readahead-secs=10")
            // IPC socket path (platform-specific, see ipc::ipc_path).
            .arg(format!("--input-ipc-server={ipc_path}"))
            // Read media from stdin.
            .arg("-")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            // Kill mpv when the Child is dropped (e.g. on panic or early exit).
            .kill_on_drop(true)
            .spawn()
            .context("failed to spawn mpv — is it installed and in PATH?")?;

        let stdin = child
            .stdin
            .take()
            .expect("stdin was piped");

        info!("mpv spawned (pid={}, ipc={})", child.id().unwrap_or(0), ipc_path);

        // Wait for mpv to create the IPC socket. mpv creates it asynchronously
        // after startup; retry for up to 4 seconds.
        let ipc = MpvIpc::connect(&ipc_path, Duration::from_secs(4))
            .await
            .context("mpv IPC socket did not appear — check that mpv started correctly")?;

        Ok(Self {
            process: child,
            stdin,
            ipc,
            state: PlaybackState::Buffering,
            session_id: *session_id,
            chunks_fed: 0,
            chunks_rejected: 0,
        })
    }

    // ── Chunk feeding ─────────────────────────────────────────────────────────

    /// Verify the HMAC on `chunk` and write its MPEG-TS payload to mpv's stdin.
    ///
    /// The payload is raw TS bytes produced by `encode_chunk`. Writing them
    /// sequentially builds a continuous TS stream; mpv demuxes and plays it
    /// with no re-framing required.
    ///
    /// Returns an error on HMAC failure (chunk is dropped, not written) or
    /// on an I/O error writing to stdin (typically means mpv exited).
    pub async fn play_chunk(&mut self, chunk: &StreamChunk) -> Result<()> {
        if !verify_hmac(chunk, &self.session_id) {
            self.chunks_rejected += 1;
            anyhow::bail!(
                "HMAC verification failed — dropping chunk seq={} (total rejected: {})",
                chunk.sequence,
                self.chunks_rejected,
            );
        }

        self.stdin
            .write_all(&chunk.payload)
            .await
            .context("writing chunk payload to mpv stdin")?;

        self.stdin
            .flush()
            .await
            .context("flushing mpv stdin after chunk")?;

        self.chunks_fed += 1;

        // Transition out of Buffering on the first successfully fed chunk.
        if self.state == PlaybackState::Buffering {
            self.state = PlaybackState::Playing;
        }

        Ok(())
    }

    // ── Playback control ──────────────────────────────────────────────────────

    /// Pause playback. No-op if already paused or stopped.
    pub async fn pause(&mut self) -> Result<()> {
        if self.state == PlaybackState::Playing {
            self.ipc.pause().await?;
            self.state = PlaybackState::Paused;
        }
        Ok(())
    }

    /// Resume playback. No-op if not paused.
    pub async fn resume(&mut self) -> Result<()> {
        if self.state == PlaybackState::Paused {
            self.ipc.resume().await?;
            self.state = PlaybackState::Playing;
        }
        Ok(())
    }

    /// Seek to an absolute playback position.
    ///
    /// Only meaningful in VOD sessions (LiveManifest has no known timeline).
    /// After seeking the host will re-key the chunk stream from `target_sequence`;
    /// the sync module calls this in response to a [`SeekControl`] message.
    pub async fn seek(&mut self, pts_secs: f64) -> Result<()> {
        self.ipc.seek_absolute(pts_secs).await
    }

    /// Nudge playback speed to correct drift without a hard seek.
    ///
    /// `ratio` = 1.0 means normal speed. The sync module calls this with a
    /// value slightly above or below 1.0 when drift is within tolerance but
    /// non-zero, reserving hard seeks for larger drift.
    pub async fn nudge_speed(&mut self, ratio: f64) -> Result<()> {
        self.ipc.set_speed(ratio).await
    }

    /// Set audio volume (0–100). Persists until changed or mpv exits.
    pub async fn set_volume(&mut self, pct: u32) -> Result<()> {
        self.ipc.set_volume(pct).await
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    /// Query mpv for the current playback position in seconds.
    ///
    /// Returns `0.0` during initial buffering (before mpv has a valid
    /// position). The sync module calls this to compute drift against the
    /// host's `SyncBeacon::host_pts`.
    pub async fn playback_time(&mut self) -> Result<f64> {
        self.ipc.playback_time().await
    }

    /// Returns `true` if the mpv process has exited.
    ///
    /// Checked each tick by the session layer; a true return triggers cleanup
    /// and a state transition to [`PlaybackState::Stopped`].
    pub fn is_running(&mut self) -> bool {
        match self.process.try_wait() {
            Ok(Some(_)) => false, // exited
            Ok(None) => true,     // still running
            Err(_) => false,      // error querying — treat as stopped
        }
    }

    // ── Lifecycle ─────────────────────────────────────────────────────────────

    /// Gracefully stop mpv.
    ///
    /// Sends `quit` via IPC, then closes stdin (signals EOF to mpv's demuxer),
    /// and waits for the process to exit. Falls back to `kill` if mpv does not
    /// exit within 2 seconds.
    pub async fn stop(&mut self) -> Result<()> {
        // Best-effort IPC quit — ignore errors (mpv may already be exiting).
        if let Err(e) = self.ipc.quit().await {
            warn!("mpv quit IPC failed (process may already be exiting): {e}");
        }

        // Dropping stdin closes it, signalling EOF to mpv's demuxer.
        // We can't move out of &mut self, so we flush and let stop() close it
        // by killing the process below if needed.
        let _ = self.stdin.flush().await;

        // Wait up to 2 seconds for a clean exit.
        let exited = tokio::time::timeout(Duration::from_secs(2), self.process.wait()).await;

        match exited {
            Ok(Ok(status)) => {
                info!("mpv exited cleanly: {status}");
            }
            _ => {
                warn!("mpv did not exit cleanly within 2 s — killing");
                let _ = self.process.kill().await;
            }
        }
        self.state = PlaybackState::Stopped;
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::messages::StreamChunk;
    use crate::stream::framing::compute_hmac;

    #[test]
    fn rejects_tampered_chunk() {
        let session_id = [0xBE; 32];
        let payload = vec![0x47u8; 188]; // one TS packet (0x47 = sync byte)
        let sequence = 3u64;
        let good_hmac = compute_hmac(&session_id, sequence, &payload);

        let mut chunk = StreamChunk {
            session_id,
            sequence,
            chapter_index: 0,
            pts: 6.0,
            keyframe: true,
            payload: payload.clone(),
            hmac: good_hmac,
        };

        // Good HMAC must pass.
        assert!(verify_hmac(&chunk, &session_id));

        // Flip one payload byte → HMAC must fail.
        chunk.payload[0] ^= 0xFF;
        assert!(!verify_hmac(&chunk, &session_id));
    }

    #[test]
    fn wrong_session_id_rejected() {
        let session_id = [0xCA; 32];
        let wrong_id = [0xFE; 32];
        let payload = b"ts-bytes".to_vec();
        let hmac = compute_hmac(&session_id, 0, &payload);

        let chunk = StreamChunk {
            session_id,
            sequence: 0,
            chapter_index: 0,
            pts: 0.0,
            keyframe: true,
            payload,
            hmac,
        };

        // HMAC was computed for session_id, not wrong_id.
        assert!(!verify_hmac(&chunk, &wrong_id));
    }
}

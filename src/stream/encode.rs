//! FFmpeg per-chunk encode pipeline.
//!
//! # Strategy
//! Each chunk is produced by an independent `ffmpeg` invocation using fast
//! seek (`-ss` before `-i`) followed by `-t` to limit duration, with `-c copy`
//! (stream copy, no re-encode) and `-f mpegts -mpegts_copyts 1 pipe:1` to stdout.
//!
//! This yields an **MPEG-TS segment** per chunk. TS segments concatenate
//! naturally — the peer writes them sequentially to mpv's stdin as one
//! unbroken TS stream. Timestamps are preserved (`-mpegts_copyts 1`) so PTS
//! values flow continuously across boundaries, giving mpv accurate timing for
//! drift detection and seeking.
//!
//! # Tradeoffs vs. a single continuous FFmpeg pipe
//! - ✅ Simple: no in-process container parsing required
//! - ✅ Cross-platform: works on Windows (no named pipes needed)
//! - ✅ Correct: each chunk is independently decodable from its first frame
//! - ⚠️  Per-process overhead (~50–100 ms/chunk); acceptable at real-time rates
//!       since the encode rate must only stay slightly ahead of playback
//! - ⚠️  Disc seek per chunk; for sequential reads the OS page cache largely
//!       absorbs this. A future optimisation can use `-f segment` with a pipe.
//!
//! # Caller responsibilities
//! Pace chunk production against peer buffer depth reported via [`PeerStatus`].
//! There is no internal throttle here — the caller's select loop is the pacer.

use std::process::Stdio;

use anyhow::{bail, Context, Result};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use crate::core::manifest::ChunkBoundary;
use crate::core::messages::StreamChunk;
use crate::stream::framing::compute_hmac;

// ── Types ─────────────────────────────────────────────────────────────────────

/// Raw encoded output for one chunk, before it is packed into a [`StreamChunk`].
pub struct EncodedChunk {
    /// Zero-based sequence number (copied from [`ChunkBoundary::sequence`]).
    pub sequence: u64,
    /// Keyframe-snapped presentation timestamp in seconds.
    pub pts_secs: f64,
    /// Actual chunk duration in seconds (may vary due to keyframe snapping).
    pub duration_secs: f64,
    /// True when the boundary was snapped to a keyframe — always true for
    /// independently-decodable segments.
    pub keyframe_snapped: bool,
    /// Chapter index at this boundary. `0` when no chapter begins here.
    pub chapter_index: u16,
    /// Self-contained Matroska segment produced by ffmpeg stdout.
    pub data: Vec<u8>,
}

// ── Encode ────────────────────────────────────────────────────────────────────

/// Encode one chunk from a media source via an ffmpeg subprocess.
///
/// ## FFmpeg command
/// ```text
/// ffmpeg -ss <pts_secs> [input_flags] -i <input_spec>
///        -t <duration_secs> -c copy -map 0 -f matroska pipe:1
/// ```
///
/// Fast seek (`-ss` before `-i`) is appropriate here because boundaries are
/// keyframe-snapped: the seek will land exactly at the intended keyframe.
///
/// `input_flags` and `input_spec` come from [`MediaSource::input_flags`] and
/// [`MediaSource::input_spec`] — e.g. `[]` + `/dev/dvd` or
/// `["-dvd_device", "/dev/dvd"]` + `dvd://`.
///
/// ## Errors
/// Returns an error if ffmpeg fails to spawn, produces no output, or exits
/// with a non-zero status when stdout was empty (a non-zero exit with partial
/// output is tolerated — ffmpeg may exit non-zero at disc end).
pub async fn encode_chunk(
    input_flags: &[&str],
    input_spec: &str,
    boundary: &ChunkBoundary,
) -> Result<EncodedChunk> {
    let pts_str = format!("{:.6}", boundary.pts_secs);
    let dur_str = format!("{:.6}", boundary.duration_secs);

    let mut child = Command::new("ffmpeg")
        // Fast seek before -i — lands on the keyframe this boundary was snapped to.
        .arg("-ss")
        .arg(&pts_str)
        // Source-specific flags (e.g. -dvd_device, -allowed_extensions).
        .args(input_flags)
        .arg("-i")
        .arg(input_spec)
        // Encode exactly this chunk's duration.
        .arg("-t")
        .arg(&dur_str)
        // Stream copy: no re-encode, minimal CPU, preserves source quality.
        .args(["-c", "copy"])
        // Map all streams (video + all audio tracks).
        .args(["-map", "0"])
        // MPEG-TS output to stdout.
        //
        // TS is chosen over Matroska because segments can be concatenated:
        // the peer writes them sequentially to mpv's stdin as a single
        // unbroken TS stream with no re-framing.
        //
        // -mpegts_copyts 1 preserves original source timestamps so PTS values
        // flow continuously across segment boundaries. mpv uses these for
        // drift detection and seeking.
        //
        // ⚠ Lossless audio tracks (Dolby TrueHD, DTS-HD MA) are not MPEG-TS
        // compatible. Audio stream selection for high-def discs is a future
        // concern at the session layer.
        .args(["-f", "mpegts", "-mpegts_copyts", "1", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .context("failed to spawn ffmpeg for chunk encode")?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let mut reader = tokio::io::BufReader::new(stdout);
    let mut data: Vec<u8> = Vec::new();
    reader
        .read_to_end(&mut data)
        .await
        .context("reading ffmpeg chunk output")?;

    let status = child.wait().await.context("waiting for ffmpeg subprocess")?;

    if data.is_empty() {
        // If ffmpeg produced nothing it cannot have succeeded.
        bail!(
            "ffmpeg produced no output for chunk seq={} pts={:.3}s (exit: {status})",
            boundary.sequence,
            boundary.pts_secs,
        );
    }

    // A non-zero exit with non-empty data is acceptable: ffmpeg sometimes exits
    // with code 1 at the natural end of a disc (muxer EOS race). The data is valid.

    Ok(EncodedChunk {
        sequence: boundary.sequence,
        pts_secs: boundary.pts_secs,
        duration_secs: boundary.duration_secs,
        keyframe_snapped: boundary.keyframe_snapped,
        chapter_index: boundary.chapter_index.unwrap_or(0),
        data,
    })
}

// ── Pack ──────────────────────────────────────────────────────────────────────

/// Pack an [`EncodedChunk`] into a [`StreamChunk`] wire message.
///
/// Computes `HMAC-SHA256(session_id, sequence, payload)` and attaches it.
/// Peers call [`crate::stream::framing::verify_hmac`] before passing the
/// payload to their player.
pub fn make_stream_chunk(chunk: EncodedChunk, session_id: &[u8; 32]) -> StreamChunk {
    let hmac = compute_hmac(session_id, chunk.sequence, &chunk.data);
    StreamChunk {
        session_id: *session_id,
        sequence: chunk.sequence,
        chapter_index: chunk.chapter_index,
        pts: chunk.pts_secs,
        keyframe: chunk.keyframe_snapped,
        payload: chunk.data,
        hmac,
    }
}

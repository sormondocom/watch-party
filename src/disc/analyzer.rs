//! Disc analyzer — orchestrates probe, media hash, keyframe scan, and manifest assembly.
//!
//! The top-level entry point is [`analyze`]. It is designed to be called by the
//! session layer, which supplies the session-level fields (`session_id`,
//! `stream_start_utc`, `host_fingerprint`).

use std::process::Stdio;

use anyhow::{anyhow, Context, Result};
use blake3::Hasher;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use crate::core::config::HostConfig;
use crate::core::manifest::{LiveManifest, SessionManifest, VodManifest};
use crate::disc::chunk_map::build_chunk_map;
use crate::disc::keyframes::{scan_keyframes, ScanProgress};
use crate::disc::probe::{probe_source, ProbeResult};
use crate::disc::source::MediaSource;

/// Assemble a [`SessionManifest`] from already-computed probe data and keyframes.
///
/// Pure function — no I/O. Call after [`probe_source`] and optionally
/// [`scan_keyframes`] when you need per-step control (e.g. TUI progress).
/// Pass `media_hash = None` to omit identity (disables `LocalDiscSync` role).
pub fn assemble_manifest(
    probe: &ProbeResult,
    keyframes: &[f64],
    media_hash: Option<[u8; 32]>,
    config: &HostConfig,
    session_id: [u8; 32],
    stream_start_utc: u64,
) -> Result<SessionManifest> {
    if let Some(duration_secs) = probe.duration_secs {
        let chunk_map = build_chunk_map(duration_secs, keyframes, &config.stream);
        let total_chunks = chunk_map.len() as u64;
        Ok(SessionManifest::Vod(VodManifest {
            session_id,
            host_fingerprint: String::new(),
            media_hash: media_hash.unwrap_or([0u8; 32]),
            duration_secs,
            chunk_duration_ms: config.stream.chunk_duration_ms,
            total_chunks,
            chunk_map,
            video_codec: probe.video.codec.clone(),
            resolution: (probe.video.width, probe.video.height),
            framerate: probe.video.framerate,
            avg_bitrate_kbps: probe.avg_bitrate_kbps.unwrap_or(0),
            peak_bitrate_kbps: probe.video.avg_bitrate_kbps.unwrap_or(0),
            audio_tracks: probe.audio_tracks.clone(),
            stream_start_utc,
            sync_beacon_interval_ms: config.sync.sync_beacon_interval_ms,
            min_buffer_chunks: config.buffer.min_buffer_chunks,
            max_buffer_chunks: config.buffer.max_buffer_chunks,
            keyframe_snap: config.stream.keyframe_snap,
            max_snap_delta_ms: config.stream.max_snap_delta_ms,
            byte_map_available: false,
        }))
    } else if config.media.allow_live_fallback {
        Ok(SessionManifest::Live(LiveManifest {
            session_id,
            host_fingerprint: String::new(),
            media_hash,
            chunk_duration_ms: config.stream.chunk_duration_ms,
            video_codec: probe.video.codec.clone(),
            resolution: Some((probe.video.width, probe.video.height)),
            framerate: Some(probe.video.framerate),
            estimated_bitrate_kbps: probe.avg_bitrate_kbps,
            audio_tracks: probe.audio_tracks.clone(),
            stream_start_utc,
            sync_beacon_interval_ms: config.sync.sync_beacon_interval_ms,
            min_buffer_chunks: config.buffer.min_buffer_chunks,
        }))
    } else {
        Err(anyhow!("media duration unknown and allow_live_fallback = false"))
    }
}

/// Analyze a media source and produce a [`SessionManifest`].
///
/// ## What this does
/// 1. Runs ffprobe to get codec, resolution, duration, audio tracks.
/// 2. Hashes the first `config.media.media_hash_bytes` bytes of re-muxed output
///    via ffmpeg pipe (works uniformly for drive letters, device nodes, ISOs).
/// 3. If the source supports it and config enables it, scans keyframe timestamps
///    for chunk boundary snapping.
/// 4. Builds the chunk map and assembles a [`VodManifest`].
///    Falls back to [`LiveManifest`] when duration is unknown.
///
/// ## Caller responsibilities
/// - Supply `session_id` (random [u8; 32]) and `stream_start_utc` (Unix ms).
/// - Fill in `host_fingerprint` on the returned manifest after PGP signing.
///
/// ## Keyframe scan channels
/// Pass `Some((progress_tx, cancel_rx))` to enable progress reporting and
/// cancellation from the TUI. Pass `None` to skip keyframe snapping entirely
/// (useful for testing or when the host wants to start immediately).
pub async fn analyze(
    source: &dyn MediaSource,
    config: &HostConfig,
    session_id: [u8; 32],
    stream_start_utc: u64,
    keyframe_channels: Option<(mpsc::Sender<ScanProgress>, oneshot::Receiver<()>)>,
) -> Result<SessionManifest> {
    let caps = source.capabilities();

    info!("probing '{}'", source.display_name());
    let probe = probe_source(source.input_flags(), source.input_spec()).await
        .context("media probe failed")?;

    // Resolve duration — None triggers live fallback below.
    let duration = if caps.has_known_duration {
        match probe.duration_secs {
            Some(d) => {
                info!("  duration:  {:.1}s", d);
                Some(d)
            }
            None => {
                warn!("source declares has_known_duration but ffprobe found none");
                None
            }
        }
    } else {
        None
    };

    info!("  video:     {}  {}x{}  {:.3}fps",
        probe.video.codec, probe.video.width, probe.video.height, probe.video.framerate);
    info!("  audio:     {} track(s)", probe.audio_tracks.len());

    // Hash media identity via ffmpeg pipe.
    // Using -c copy -f matroska means the hash is derived from container bytes,
    // not raw device bytes — consistent across drive letters, device nodes, ISOs.
    let media_hash = if caps.has_stable_identity {
        match hash_via_ffmpeg(
            source.input_flags(),
            source.input_spec(),
            config.media.media_hash_bytes,
        )
        .await
        {
            Ok(h) => {
                info!("  media hash: {}...", hex_prefix(&h));
                Some(h)
            }
            Err(e) => {
                warn!("media hash failed (LocalDiscSync unavailable): {e}");
                None
            }
        }
    } else {
        None
    };

    // ── VOD path ──────────────────────────────────────────────────────────────

    let keyframes = if let Some(duration_secs) = duration {
        match keyframe_channels {
            Some((tx, rx)) if caps.supports_keyframe_scan && config.stream.keyframe_snap => {
                info!("scanning keyframes for '{}'", source.display_name());
                match scan_keyframes(source.input_flags(), source.input_spec(), duration_secs, tx, rx).await {
                    Ok(kf) => kf,
                    Err(e) => {
                        warn!("keyframe scan failed, proceeding without snapping: {e}");
                        vec![]
                    }
                }
            }
            _ => vec![],
        }
    } else {
        vec![]
    };

    let manifest = assemble_manifest(&probe, &keyframes, media_hash, config, session_id, stream_start_utc)?;
    if let SessionManifest::Vod(ref v) = manifest {
        info!("  chunks:    {} × {}ms target", v.total_chunks, config.stream.chunk_duration_ms);
    } else {
        warn!("falling back to LiveManifest for '{}'", source.display_name());
    }
    Ok(manifest)
}

/// Hash the first `n_bytes` of a media source by piping ffmpeg re-muxed output
/// through blake3.
///
/// `-c copy -f matroska pipe:1` re-muxes without re-encoding, so the output is
/// deterministic given the same input content. Works for drive letters, device
/// nodes, ISOs, and container files without needing to understand their
/// filesystem layouts.
async fn hash_via_ffmpeg(
    input_flags: &[&str],
    input_spec: &str,
    n_bytes: u64,
) -> Result<[u8; 32]> {
    let mut child = Command::new("ffmpeg")
        .args(input_flags)
        .args(["-i", input_spec])
        .args(["-c", "copy", "-map", "0", "-f", "matroska", "pipe:1"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
        .context("failed to spawn ffmpeg for media hash")?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let mut reader = tokio::io::BufReader::new(stdout);
    let mut hasher = Hasher::new();
    let mut buf = vec![0u8; 65536]; // 64 KiB read chunks
    let mut remaining = n_bytes;

    while remaining > 0 {
        let to_read = (remaining as usize).min(buf.len());
        match reader.read(&mut buf[..to_read]).await? {
            0 => break, // EOF before n_bytes — hash whatever we got
            n => {
                hasher.update(&buf[..n]);
                remaining -= n as u64;
            }
        }
    }

    // Kill ffmpeg — we only needed the first n_bytes.
    let _ = child.kill().await;

    Ok(*hasher.finalize().as_bytes())
}

fn hex_prefix(b: &[u8; 32]) -> String {
    b.iter().take(4).map(|x| format!("{x:02x}")).collect()
}

//! ffprobe bridge — parse container metadata into [`ProbeResult`].
//!
//! Runs: `ffprobe -v quiet -print_format json -show_streams -show_format <input>`
//! Parses the JSON output into typed Rust structs.

use std::process::Stdio;

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use tokio::process::Command;

use crate::core::manifest::AudioTrack;

/// Parsed container and stream metadata from ffprobe.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    pub video: VideoStreamInfo,
    pub audio_tracks: Vec<AudioTrack>,
    /// Total duration in seconds. `None` for live or unknown-duration sources.
    pub duration_secs: Option<f64>,
    /// Container format name as reported by ffprobe (e.g. `"matroska,webm"`, `"dvd"`).
    pub format_name: String,
    /// Container-level average bitrate in kbps, if reported.
    pub avg_bitrate_kbps: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct VideoStreamInfo {
    /// Codec short name (e.g. `"h264"`, `"mpeg2video"`).
    pub codec: String,
    pub width: u16,
    pub height: u16,
    /// Frames per second, decoded from ffprobe's fractional string (`"24000/1001"` → 23.976).
    pub framerate: f32,
    /// Stream-level average bitrate in kbps, if reported.
    pub avg_bitrate_kbps: Option<u32>,
}

/// Run ffprobe on the given source and return parsed media properties.
pub async fn probe_source(input_flags: &[&str], input_spec: &str) -> Result<ProbeResult> {
    let output = Command::new("ffprobe")
        .args(["-v", "quiet", "-print_format", "json", "-show_streams", "-show_format"])
        .args(input_flags)
        .arg(input_spec)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .await
        .context("failed to spawn ffprobe — is it installed and on PATH?")?;

    if !output.status.success() {
        return Err(anyhow!("ffprobe exited with {}", output.status));
    }

    let raw: FfprobeOutput = serde_json::from_slice(&output.stdout)
        .context("failed to parse ffprobe JSON output")?;

    parse_output(raw)
}

fn parse_output(raw: FfprobeOutput) -> Result<ProbeResult> {
    let vs = raw.streams.iter()
        .find(|s| s.codec_type.as_deref() == Some("video"))
        .ok_or_else(|| anyhow!("no video stream found in media"))?;

    let video = VideoStreamInfo {
        codec: vs.codec_name.clone()
            .ok_or_else(|| anyhow!("video stream has no codec_name"))?,
        width: vs.width
            .ok_or_else(|| anyhow!("video stream has no width"))?,
        height: vs.height
            .ok_or_else(|| anyhow!("video stream has no height"))?,
        framerate: parse_rate(
            vs.avg_frame_rate.as_deref()
                .or(vs.r_frame_rate.as_deref())
                .unwrap_or("0/1"),
        ),
        avg_bitrate_kbps: vs.bit_rate.as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .map(|bps| (bps / 1000) as u32),
    };

    let audio_tracks = raw.streams.iter()
        .filter(|s| s.codec_type.as_deref() == Some("audio"))
        .enumerate()
        .map(|(i, s)| AudioTrack {
            index: i as u16,
            codec: s.codec_name.clone().unwrap_or_else(|| "unknown".into()),
            language: s.tags.as_ref().and_then(|t| t.language.clone()),
            channels: s.channels.unwrap_or(2),
        })
        .collect();

    Ok(ProbeResult {
        video,
        audio_tracks,
        duration_secs: raw.format.duration.as_deref()
            .and_then(|s| s.parse::<f64>().ok())
            .filter(|&d| d > 0.0),
        avg_bitrate_kbps: raw.format.bit_rate.as_deref()
            .and_then(|s| s.parse::<u64>().ok())
            .map(|bps| (bps / 1000) as u32),
        format_name: raw.format.format_name.unwrap_or_else(|| "unknown".into()),
    })
}

/// Parse ffprobe's fractional frame rate string into f32.
/// Handles `"24000/1001"` → 23.976, `"30/1"` → 30.0, `"0/0"` → 0.0.
fn parse_rate(s: &str) -> f32 {
    let mut parts = s.splitn(2, '/');
    let n: f32 = parts.next().and_then(|x| x.parse().ok()).unwrap_or(0.0);
    let d: f32 = parts.next().and_then(|x| x.parse().ok()).unwrap_or(1.0);
    if d == 0.0 { 0.0 } else { n / d }
}

// ── ffprobe JSON deserialization shapes ───────────────────────────────────────
// These mirror the raw ffprobe JSON structure. Internal only — not part of the
// public API. Field names match ffprobe output exactly (snake_case).

#[derive(Deserialize)]
struct FfprobeOutput {
    #[serde(default)]
    streams: Vec<FfprobeStream>,
    format: FfprobeFormat,
}

#[derive(Deserialize)]
struct FfprobeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    width: Option<u16>,
    height: Option<u16>,
    r_frame_rate: Option<String>,
    avg_frame_rate: Option<String>,
    bit_rate: Option<String>,
    channels: Option<u8>,
    #[serde(default)]
    tags: Option<FfprobeTags>,
}

#[derive(Deserialize, Default)]
struct FfprobeTags {
    language: Option<String>,
}

#[derive(Deserialize)]
struct FfprobeFormat {
    duration: Option<String>,
    bit_rate: Option<String>,
    format_name: Option<String>,
}

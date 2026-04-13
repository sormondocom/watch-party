//! Keyframe timestamp scanning via ffprobe packet inspection.
//!
//! No video decoding is performed — only packet headers are read.
//! For a 2-hour disc at 24fps this processes ~175k packet header lines.
//! Expect 1–3 minutes for optical media over USB.
//!
//! The scan supports progress reporting and cancellation so the TUI can show
//! a progress bar and let the host abort (falling back to time-derived
//! chunk boundaries without keyframe snapping).

use std::process::Stdio;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::{mpsc, oneshot};
use tracing::info;

/// Progress snapshot sent during a keyframe scan.
#[derive(Debug, Clone)]
pub struct ScanProgress {
    /// Number of keyframes found so far.
    pub keyframes_found: usize,
    /// Estimated scan completion in [0.0, 1.0], based on last keyframe PTS
    /// relative to total duration. Stays at 0.0 if `duration_secs` was 0.
    pub fraction: f32,
}

/// Scan `input_spec` for keyframe presentation timestamps using ffprobe
/// packet-level inspection (no decoding).
///
/// # Arguments
/// - `input_flags` — extra flags prepended to the ffprobe command (see [`MediaSource::input_flags`])
/// - `input_spec`  — the ffprobe input specifier
/// - `duration_secs` — total media duration; used only to compute `fraction` in progress reports
/// - `progress_tx` — receives [`ScanProgress`] roughly every 500 keyframes and once at completion
/// - `cancel_rx`   — send `()` on the paired sender to abort the scan early
///
/// # Cancellation
/// When cancelled, keyframes found up to that point are returned. These are
/// valid for the portion of the timeline scanned; the chunk map builder uses
/// them for early chunks and falls back to time-derived positions beyond.
pub async fn scan_keyframes(
    input_flags: &[&str],
    input_spec: &str,
    duration_secs: f64,
    progress_tx: mpsc::Sender<ScanProgress>,
    mut cancel_rx: oneshot::Receiver<()>,
) -> Result<Vec<f64>> {
    // ffprobe packet inspection:
    //   -show_entries packet=pts_time,flags  — one CSV line per packet
    //   -of csv=p=0                          — suppress [PACKET] section headers
    //
    // Output lines: "pts_time,flags"  e.g. "0.000000,K__" or "0.041708,___"
    // The first character of the flags field is 'K' for keyframe packets.
    let mut child = Command::new("ffprobe")
        .args([
            "-v", "quiet",
            "-select_streams", "v:0",
            "-show_entries", "packet=pts_time,flags",
            "-of", "csv=p=0",
        ])
        .args(input_flags)
        .arg(input_spec)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn ffprobe for keyframe scan")?;

    let stdout = child.stdout.take().expect("stdout was piped");
    let mut lines = BufReader::new(stdout).lines();
    let mut keyframes: Vec<f64> = Vec::new();

    'scan: loop {
        tokio::select! {
            // Check cancel first (biased) so a pending cancel isn't masked by
            // a flood of incoming lines.
            biased;

            _ = &mut cancel_rx => {
                info!("keyframe scan cancelled — returning {} partial keyframes", keyframes.len());
                break 'scan;
            }

            line = lines.next_line() => {
                match line.context("reading ffprobe packet output")? {
                    None => break 'scan, // EOF — scan complete

                    Some(line) => {
                        let mut fields = line.splitn(2, ',');
                        let pts_str = match fields.next() { Some(s) => s, None => continue };
                        let flags   = match fields.next() { Some(s) => s, None => continue };

                        if flags.starts_with('K') {
                            if let Ok(pts) = pts_str.parse::<f64>() {
                                keyframes.push(pts);

                                if keyframes.len() % 500 == 0 {
                                    let fraction = if duration_secs > 0.0 {
                                        (pts / duration_secs).clamp(0.0, 1.0) as f32
                                    } else {
                                        0.0
                                    };
                                    // try_send: if the TUI channel is full, drop the report —
                                    // the next one will arrive shortly.
                                    let _ = progress_tx.try_send(ScanProgress {
                                        keyframes_found: keyframes.len(),
                                        fraction,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // Final progress update (fraction = 1.0 regardless of cancel — the TUI
    // uses this to know the scan loop has exited).
    let _ = progress_tx.try_send(ScanProgress {
        keyframes_found: keyframes.len(),
        fraction: 1.0,
    });

    // Kill ffprobe if we cancelled before EOF; no-op if it already exited.
    let _ = child.kill().await;

    info!("keyframe scan done: {} keyframes", keyframes.len());
    Ok(keyframes)
}

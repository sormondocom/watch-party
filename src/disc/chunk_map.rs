//! Chunk boundary map builder.
//!
//! Boundaries are **time-derived** (not metadata-derived), ensuring correctness
//! on discs with missing, damaged, or incomplete chapter marks.
//!
//! Optionally each calculated boundary is snapped to the nearest keyframe
//! within `max_snap_delta_ms`. Boundaries with no keyframe in range are left
//! at their time-derived position with `keyframe_snapped = false`.
//!
//! Chapter annotations (`is_chapter_start`, `chapter_title`) are left unset here.
//! The session layer annotates them from disc metadata after the map is built.

use crate::core::config::StreamConfig;
use crate::core::manifest::ChunkBoundary;

/// Build the full chunk boundary map.
///
/// # Arguments
/// - `duration_secs` — total media duration from ffprobe
/// - `keyframes`     — keyframe PTS values in ascending order; empty = no snapping
/// - `config`        — stream config (chunk size, snap window, snap enabled flag)
pub fn build_chunk_map(
    duration_secs: f64,
    keyframes: &[f64],
    config: &StreamConfig,
) -> Vec<ChunkBoundary> {
    let chunk_dur_s = config.chunk_duration_ms as f64 / 1000.0;
    let max_snap_s  = config.max_snap_delta_ms as f64 / 1000.0;
    let total       = (duration_secs / chunk_dur_s).ceil() as u64;

    // First pass: compute PTS for each chunk boundary.
    let mut map: Vec<ChunkBoundary> = (0..total)
        .map(|seq| {
            let target = seq as f64 * chunk_dur_s;
            let (pts, snap_delta_ms, keyframe_snapped) =
                if config.keyframe_snap && !keyframes.is_empty() {
                    snap_to_keyframe(target, keyframes, max_snap_s)
                } else {
                    (target, 0, false)
                };

            ChunkBoundary {
                sequence: seq,
                pts_secs: pts,
                duration_secs: 0.0, // filled in second pass
                byte_offset: None,
                byte_length: None,
                keyframe_snapped,
                snap_delta_ms,
                is_chapter_start: false,
                chapter_index: None,
                chapter_title: None,
            }
        })
        .collect();

    // Second pass: actual duration = next chunk's PTS − this chunk's PTS.
    // The last chunk runs to the end of media.
    for i in 0..map.len() {
        let next_pts = if i + 1 < map.len() {
            map[i + 1].pts_secs
        } else {
            duration_secs
        };
        map[i].duration_secs = (next_pts - map[i].pts_secs).max(0.0);
    }

    map
}

/// Find the nearest keyframe to `target` within `max_snap` seconds.
/// Returns `(snapped_pts, snap_delta_ms, was_snapped)`.
///
/// Uses binary search so this is O(log n) per boundary.
fn snap_to_keyframe(target: f64, keyframes: &[f64], max_snap: f64) -> (f64, i32, bool) {
    // partition_point gives us the index of the first keyframe >= target
    let idx = keyframes.partition_point(|&k| k < target);

    // Check the keyframe just before and just after target
    let best = [
        idx.checked_sub(1).map(|i| keyframes[i]),
        keyframes.get(idx).copied(),
    ]
    .into_iter()
    .flatten()
    .min_by(|a, b| {
        (a - target).abs().partial_cmp(&(b - target).abs()).unwrap()
    });

    match best {
        Some(kf) if (kf - target).abs() <= max_snap => {
            let delta_ms = ((kf - target) * 1000.0).round() as i32;
            (kf, delta_ms, true)
        }
        _ => (target, 0, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::config::StreamConfig;

    fn cfg(snap: bool) -> StreamConfig {
        StreamConfig {
            chunk_duration_ms: 2000,
            max_snap_delta_ms: 500,
            keyframe_snap: snap,
        }
    }

    #[test]
    fn no_keyframes_time_derived() {
        let map = build_chunk_map(10.0, &[], &cfg(true));
        assert_eq!(map.len(), 5);
        assert_eq!(map[0].pts_secs, 0.0);
        assert!((map[0].duration_secs - 2.0).abs() < 1e-9);
        assert!(!map[0].keyframe_snapped);
    }

    #[test]
    fn snaps_within_window() {
        // Target at 2.0s; keyframe at 2.3s (within 500ms window)
        let keyframes = vec![0.0, 2.3, 4.0, 6.0, 8.0, 10.0];
        let map = build_chunk_map(10.0, &keyframes, &cfg(true));
        assert_eq!(map[1].pts_secs, 2.3);
        assert!(map[1].keyframe_snapped);
        assert_eq!(map[1].snap_delta_ms, 300);
    }

    #[test]
    fn no_snap_outside_window() {
        // Target at 2.0s; nearest keyframe at 2.6s (outside 500ms window)
        let keyframes = vec![0.0, 2.6, 4.0];
        let map = build_chunk_map(6.0, &keyframes, &cfg(true));
        assert_eq!(map[1].pts_secs, 2.0);
        assert!(!map[1].keyframe_snapped);
    }

    #[test]
    fn durations_sum_to_total() {
        let keyframes = vec![0.0, 1.9, 4.1, 6.0, 8.0];
        let map = build_chunk_map(10.0, &keyframes, &cfg(true));
        let total: f64 = map.iter().map(|c| c.duration_secs).sum();
        assert!((total - 10.0).abs() < 1e-9);
    }
}

//! OpticalDisc — wraps a disc drive, ISO image, or disc directory.

use std::path::PathBuf;

use crate::disc::source::{MediaSource, SourceCapabilities};

/// An optical disc drive, ISO image, or disc directory (VIDEO_TS, BDMV).
///
/// Pass any path that ffmpeg accepts as an input specifier:
///
/// | Input                     | Platform        |
/// |---------------------------|-----------------|
/// | `D:\`  or  `D:\VIDEO_TS`  | Windows drive   |
/// | `/dev/sr0`  or  `/dev/dvd`| Linux drive     |
/// | `/path/to/disc.iso`       | ISO image       |
/// | `/path/to/movie.mkv`      | Container file  |
///
/// ffmpeg handles all of these natively via its input demuxer.
pub struct OpticalDisc {
    /// Path to the disc, device node, ISO, disc directory, or container file.
    pub path: PathBuf,
    /// Volume label from disc metadata (display-only; not used by the protocol).
    pub label: Option<String>,
}

impl OpticalDisc {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), label: None }
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl MediaSource for OpticalDisc {
    fn display_name(&self) -> &str {
        self.label.as_deref()
            .unwrap_or_else(|| self.path.to_str().unwrap_or("optical disc"))
    }

    fn input_spec(&self) -> &str {
        self.path.to_str().unwrap_or("")
    }

    // No extra input flags needed — ffmpeg auto-detects disc formats.

    fn capabilities(&self) -> SourceCapabilities {
        SourceCapabilities {
            has_stable_identity: true,
            // Best-effort: may degrade to false on damaged or incomplete discs
            // when ffprobe cannot determine duration.
            has_known_duration: true,
            supports_keyframe_scan: true,
            supports_seeking: true,
        }
    }
}

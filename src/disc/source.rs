//! MediaSource trait — the abstraction boundary between the disc module and
//! the session layer.
//!
//! Adding a new input kind (VCR, capture card, RTSP stream) means implementing
//! this trait. The analyzer asks for capabilities and dispatches accordingly —
//! no hardcoded `if optical` branches anywhere.

/// What a media source can offer before streaming begins.
/// These fields drive whether the session produces a [`VodManifest`] or [`LiveManifest`].
#[derive(Debug, Clone)]
pub struct SourceCapabilities {
    /// Source has stable bytes that can be hashed for peer identity verification.
    /// `true` for files, ISOs, and disc paths.
    /// `false` for live capture devices — a VCR feed has no stable identity.
    pub has_stable_identity: bool,

    /// ffprobe can determine total duration before streaming starts.
    /// `false` for live/unknown-duration sources (VCR, damaged discs).
    /// When `false`, the session falls back to [`LiveManifest`].
    pub has_known_duration: bool,

    /// Keyframe timestamps can be scanned in full before the stream begins.
    /// `false` for live capture — there is no pre-stream content to scan.
    pub supports_keyframe_scan: bool,

    /// Source supports host-initiated seeking during active playback.
    /// `false` for live capture.
    pub supports_seeking: bool,
}

/// Abstraction over all video input sources the host can stream from.
///
/// Implemented today by [`OpticalDisc`].
/// Planned: `CaptureSource` (VCR via capture card, V4L2, DirectShow, SDI).
///
/// The trait exposes only what [`analyze`] needs: an ffmpeg input specifier,
/// optional extra input flags, and a capability declaration.
pub trait MediaSource: Send + Sync {
    /// Human-readable label shown in the TUI (e.g. `"BLADE_RUNNER_2049"`, `"D:\\disc.iso"`).
    fn display_name(&self) -> &str;

    /// Primary argument passed to ffmpeg/ffprobe as the input source.
    ///
    /// Examples:
    /// - `/dev/sr0` or `/dev/dvd`           (Linux optical drive)
    /// - `D:\` or `D:\VIDEO_TS`             (Windows optical drive)
    /// - `/path/to/movie.iso`               (ISO image)
    /// - `/dev/video0`                      (V4L2 capture — future)
    fn input_spec(&self) -> &str;

    /// Extra ffmpeg/ffprobe flags that must appear *before* `-i <input_spec>`.
    ///
    /// Example: `&["-f", "v4l2"]` for a Video4Linux capture device.
    /// Most sources return an empty slice.
    fn input_flags(&self) -> &[&str] {
        &[]
    }

    /// Declare what pregame analysis this source type supports.
    fn capabilities(&self) -> SourceCapabilities;
}

//! Media source abstraction and pregame analysis pipeline.
//!
//! [`MediaSource`] is the central trait. Today: [`OpticalDisc`].
//! Planned: `CaptureSource` for VCR, capture cards, SDI — anything ffmpeg
//! can open as a live input. The trait surface is designed so both types
//! share the same [`analyze`] path; the manifest type (VOD vs Live) falls
//! out of the source's declared [`SourceCapabilities`].
//!
//! ## Typical call sequence
//! ```text
//! probe_source()       → ProbeResult       (fast, ~1s)
//! scan_keyframes()     → Vec<f64>          (slow, 1–3 min; progress + cancellable)
//! analyze()            → SessionManifest   (assembles everything)
//! ```
//! `analyze` calls the others internally. Call them separately only when you
//! need per-step progress feedback in the TUI.

mod analyzer;
mod chunk_map;
pub mod keyframes;
mod optical;
mod probe;
pub mod source;

pub use analyzer::{analyze, assemble_manifest};
pub use keyframes::{scan_keyframes, ScanProgress};
pub use optical::OpticalDisc;
pub use probe::{probe_source, ProbeResult, VideoStreamInfo};
pub use source::{MediaSource, SourceCapabilities};

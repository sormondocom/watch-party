//! stream — FFmpeg encode pipeline, wire framing, and TCP transport.
//!
//! # Modules
//! - [`encode`]    — per-chunk FFmpeg subprocess encode; produces [`EncodedChunk`]
//! - [`framing`]   — wire frame read/write: `[4-byte LE len][1-byte tag][bincode]`
//! - [`transport`] — TCP host listener ([`HostListener`]) and peer connection ([`PeerConn`])
//!
//! # Host streaming flow
//! ```text
//! VodManifest::chunk_map
//!   └─► encode_chunk(input_flags, input_spec, boundary)    // ffmpeg subprocess
//!         └─► make_stream_chunk(encoded, session_id)       // pack + HMAC
//!               └─► PeerConn::send(&WireMessage::Chunk(_)) // framed TCP write
//! ```
//!
//! # Peer receive flow
//! ```text
//! OwnedReadHalf  (from connect_to_host)
//!   └─► read_message(&mut read_half)     // framed TCP read
//!         └─► WireMessage::Chunk(chunk)
//!               └─► verify_hmac(&chunk, session_id)  // integrity check
//!                     └─► feed payload to mpv via pipe / IPC
//! ```

pub mod encode;
pub mod framing;
pub mod transport;

pub use encode::{encode_chunk, make_stream_chunk, EncodedChunk};
pub use framing::{compute_hmac, read_message, verify_hmac, write_message};
pub use transport::{connect_to_host, HostListener, PeerConn};

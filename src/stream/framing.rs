//! Wire framing: `[4-byte LE length][1-byte type tag][bincode payload]`
//!
//! The length field covers tag + payload (not the length field itself).
//! Type tags are stable on-wire identifiers; they let the decoder dispatch
//! without fully deserializing the bincode envelope first.
//!
//! # HMAC
//! [`compute_hmac`] / [`verify_hmac`] cover [`StreamChunk`] payloads as
//! specified by the design: HMAC-SHA256 keyed on the session_id over
//! `session_id || sequence (LE u64) || payload`.

use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::core::manifest::SessionManifest;
use crate::core::messages::{
    CapabilityChallenge, CapabilityResponse, ManifestAck, PeerStatus, SeekControl, SessionControl,
    SessionRoles, StreamChunk, StreamReady, SyncBeacon, WireMessage,
};

// ── Type tags ─────────────────────────────────────────────────────────────────
// Stable, never reused. Gaps are intentional — leave room to add variants
// within each phase group without renumbering.

// Pregame
const TAG_MANIFEST: u8 = 0x01;
const TAG_MANIFEST_ACK: u8 = 0x02;
const TAG_CAPABILITY_CHALLENGE: u8 = 0x03;
const TAG_CAPABILITY_RESPONSE: u8 = 0x04;
const TAG_SESSION_ROLES: u8 = 0x05;
const TAG_STREAM_READY: u8 = 0x06;

// Stream
const TAG_CHUNK: u8 = 0x10;
const TAG_SYNC_BEACON: u8 = 0x11;
const TAG_PEER_STATUS: u8 = 0x12;

// Control
const TAG_PAUSE: u8 = 0x20;
const TAG_RESUME: u8 = 0x21;
const TAG_SEEK: u8 = 0x22;
const TAG_END: u8 = 0x23;

// ── Write ─────────────────────────────────────────────────────────────────────

/// Serialize and write a [`WireMessage`] to `w`.
///
/// Frame layout: `[4-byte LE length][1-byte tag][bincode payload]`
/// where length = 1 + payload.len().
///
/// Flushes after writing so callers do not need to flush separately.
pub async fn write_message<W: AsyncWrite + Unpin>(w: &mut W, msg: &WireMessage) -> Result<()> {
    let (tag, payload) = encode_inner(msg)?;
    let frame_len = (1u64 + payload.len() as u64) as u32;

    w.write_all(&frame_len.to_le_bytes())
        .await
        .context("writing frame length")?;
    w.write_all(&[tag]).await.context("writing type tag")?;
    w.write_all(&payload).await.context("writing payload")?;
    w.flush().await.context("flushing frame")?;
    Ok(())
}

/// Serialize the inner struct of a [`WireMessage`] to `(tag, bincode_bytes)`.
///
/// The bincode payload encodes only the inner struct — not the outer enum
/// discriminant — so the type tag is the sole dispatch key on the wire.
fn encode_inner(msg: &WireMessage) -> Result<(u8, Vec<u8>)> {
    let (tag, bytes) = match msg {
        WireMessage::Manifest(m) => (TAG_MANIFEST, bincode::serialize(m)?),
        WireMessage::ManifestAck(m) => (TAG_MANIFEST_ACK, bincode::serialize(m)?),
        WireMessage::CapabilityChallenge(m) => (TAG_CAPABILITY_CHALLENGE, bincode::serialize(m)?),
        WireMessage::CapabilityResponse(m) => (TAG_CAPABILITY_RESPONSE, bincode::serialize(m)?),
        WireMessage::SessionRoles(m) => (TAG_SESSION_ROLES, bincode::serialize(m)?),
        WireMessage::StreamReady(m) => (TAG_STREAM_READY, bincode::serialize(m)?),
        WireMessage::Chunk(m) => (TAG_CHUNK, bincode::serialize(m)?),
        WireMessage::SyncBeacon(m) => (TAG_SYNC_BEACON, bincode::serialize(m)?),
        WireMessage::PeerStatus(m) => (TAG_PEER_STATUS, bincode::serialize(m)?),
        WireMessage::Pause(m) => (TAG_PAUSE, bincode::serialize(m)?),
        WireMessage::Resume(m) => (TAG_RESUME, bincode::serialize(m)?),
        WireMessage::Seek(m) => (TAG_SEEK, bincode::serialize(m)?),
        WireMessage::End(m) => (TAG_END, bincode::serialize(m)?),
    };
    Ok((tag, bytes))
}

// ── Read ──────────────────────────────────────────────────────────────────────

/// Maximum accepted frame body size: 16 MiB.
///
/// StreamChunks are the largest message (~2s of encoded video). Even at high
/// bitrates (40 Mbps peak), a 2-second chunk is ~10 MB. 16 MiB leaves headroom
/// while guarding against allocation bombs from corrupt or malicious frames.
const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// Read and deserialize the next [`WireMessage`] from `r`.
///
/// Reads exactly `length` bytes after the 4-byte length prefix.
/// Returns an error on unknown type tags, oversized frames, or deserialization
/// failures. EOF mid-frame is surfaced as an `UnexpectedEof` IO error.
pub async fn read_message<R: AsyncRead + Unpin>(r: &mut R) -> Result<WireMessage> {
    // 4-byte LE length prefix
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf)
        .await
        .context("reading frame length")?;
    let frame_len = u32::from_le_bytes(len_buf);

    if frame_len == 0 {
        bail!("received zero-length frame");
    }
    if frame_len > MAX_FRAME_BYTES {
        bail!("frame too large: {frame_len} bytes (max {MAX_FRAME_BYTES})");
    }

    // tag + payload in one allocation
    let mut frame = vec![0u8; frame_len as usize];
    r.read_exact(&mut frame)
        .await
        .context("reading frame body")?;

    let tag = frame[0];
    let payload = &frame[1..];
    decode_inner(tag, payload)
}

fn decode_inner(tag: u8, payload: &[u8]) -> Result<WireMessage> {
    let msg = match tag {
        TAG_MANIFEST => WireMessage::Manifest(
            bincode::deserialize::<SessionManifest>(payload)
                .context("deserializing Manifest")?,
        ),
        TAG_MANIFEST_ACK => WireMessage::ManifestAck(
            bincode::deserialize::<ManifestAck>(payload)
                .context("deserializing ManifestAck")?,
        ),
        TAG_CAPABILITY_CHALLENGE => WireMessage::CapabilityChallenge(
            bincode::deserialize::<CapabilityChallenge>(payload)
                .context("deserializing CapabilityChallenge")?,
        ),
        TAG_CAPABILITY_RESPONSE => WireMessage::CapabilityResponse(
            bincode::deserialize::<CapabilityResponse>(payload)
                .context("deserializing CapabilityResponse")?,
        ),
        TAG_SESSION_ROLES => WireMessage::SessionRoles(
            bincode::deserialize::<SessionRoles>(payload)
                .context("deserializing SessionRoles")?,
        ),
        TAG_STREAM_READY => WireMessage::StreamReady(
            bincode::deserialize::<StreamReady>(payload)
                .context("deserializing StreamReady")?,
        ),
        TAG_CHUNK => WireMessage::Chunk(
            bincode::deserialize::<StreamChunk>(payload).context("deserializing Chunk")?,
        ),
        TAG_SYNC_BEACON => WireMessage::SyncBeacon(
            bincode::deserialize::<SyncBeacon>(payload)
                .context("deserializing SyncBeacon")?,
        ),
        TAG_PEER_STATUS => WireMessage::PeerStatus(
            bincode::deserialize::<PeerStatus>(payload)
                .context("deserializing PeerStatus")?,
        ),
        TAG_PAUSE => WireMessage::Pause(
            bincode::deserialize::<SessionControl>(payload).context("deserializing Pause")?,
        ),
        TAG_RESUME => WireMessage::Resume(
            bincode::deserialize::<SessionControl>(payload).context("deserializing Resume")?,
        ),
        TAG_SEEK => WireMessage::Seek(
            bincode::deserialize::<SeekControl>(payload).context("deserializing Seek")?,
        ),
        TAG_END => WireMessage::End(
            bincode::deserialize::<SessionControl>(payload).context("deserializing End")?,
        ),
        _ => bail!("unknown wire type tag: 0x{tag:02x}"),
    };
    Ok(msg)
}

// ── HMAC helpers ─────────────────────────────────────────────────────────────

type HmacSha256 = Hmac<Sha256>;

/// Compute HMAC-SHA256 over `sequence (LE u64) || payload`, keyed on `session_id`.
///
/// The session_id serves as the HMAC key — it is a shared 32-byte secret
/// embedded in the manifest and never retransmitted in plaintext.
/// Including the sequence in the authenticated data prevents chunk replay attacks.
pub fn compute_hmac(session_id: &[u8; 32], sequence: u64, payload: &[u8]) -> [u8; 32] {
    let mut mac =
        HmacSha256::new_from_slice(session_id).expect("HMAC-SHA256 accepts any key length");
    mac.update(&sequence.to_le_bytes());
    mac.update(payload);
    let result = mac.finalize().into_bytes();
    let mut out = [0u8; 32];
    out.copy_from_slice(&result);
    out
}

/// Verify the HMAC on a received [`StreamChunk`] in constant time.
///
/// Returns `false` if the chunk's `hmac` field does not match the expected
/// value computed from `session_id`, `sequence`, and `payload`.
pub fn verify_hmac(chunk: &StreamChunk, session_id: &[u8; 32]) -> bool {
    let mut mac =
        HmacSha256::new_from_slice(session_id).expect("HMAC-SHA256 accepts any key length");
    mac.update(&chunk.sequence.to_le_bytes());
    mac.update(&chunk.payload);
    mac.verify_slice(&chunk.hmac).is_ok()
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::messages::SyncBeacon;
    use tokio::io::BufWriter;

    #[tokio::test]
    async fn roundtrip_sync_beacon() {
        let msg = WireMessage::SyncBeacon(SyncBeacon {
            session_id: [0xAB; 32],
            host_pts: 42.5,
            host_chunk_seq: 21,
            playing: true,
            timestamp_utc: 1_700_000_000,
        });

        let mut buf: Vec<u8> = Vec::new();
        {
            let mut w = BufWriter::new(&mut buf);
            write_message(&mut w, &msg).await.unwrap();
        }

        let mut cursor = std::io::Cursor::new(&buf);
        let decoded = read_message(&mut cursor).await.unwrap();

        match decoded {
            WireMessage::SyncBeacon(b) => {
                assert_eq!(b.host_chunk_seq, 21);
                assert!(b.playing);
            }
            other => panic!("expected SyncBeacon, got {other:?}"),
        }
    }

    #[test]
    fn hmac_compute_and_verify() {
        let session_id = [0x1A; 32];
        let payload = b"hello chunk";
        let sequence = 7u64;

        let tag = compute_hmac(&session_id, sequence, payload);

        let chunk = StreamChunk {
            session_id,
            sequence,
            chapter_index: 0,
            pts: 0.0,
            keyframe: true,
            payload: payload.to_vec(),
            hmac: tag,
        };

        assert!(verify_hmac(&chunk, &session_id));

        // Tampered payload must fail.
        let mut tampered = chunk.clone();
        tampered.payload[0] ^= 0xFF;
        assert!(!verify_hmac(&tampered, &session_id));
    }

    #[tokio::test]
    async fn rejects_oversized_frame() {
        // Craft a frame claiming to be MAX_FRAME_BYTES + 1.
        let oversized_len = (MAX_FRAME_BYTES + 1).to_le_bytes();
        let mut cursor = std::io::Cursor::new(oversized_len.to_vec());
        let err = read_message(&mut cursor).await.unwrap_err();
        assert!(err.to_string().contains("frame too large"));
    }
}

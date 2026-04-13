//! TCP transport — host listener and peer connection primitives.
//!
//! # Design
//!
//! The transport layer is intentionally thin: it provides framed send/receive
//! over a split [`TcpStream`] and leaves all session logic to callers.
//!
//! Each accepted connection is split into:
//! - A [`PeerConn`] (write half) — owned by the sender, used to push messages.
//! - An [`OwnedReadHalf`] — returned to the caller to drive in a separate task.
//!
//! This separation lets the host's main loop call `conn.send(msg)` while a
//! per-peer reader task runs concurrently in `tokio::select!`.
//!
//! # Encryption
//! The transport is currently unencrypted. Session-level encryption (Noise
//! protocol or TLS) is planned as a future layer sitting between the TCP stream
//! and the framing. The [`write_message`] / [`read_message`] calls in this file
//! are the natural insertion point.
//!
//! # Example — host side
//! ```no_run
//! # use std::net::SocketAddr;
//! # use watch_party::stream::transport::{HostListener, PeerConn};
//! # use watch_party::stream::framing::read_message;
//! # async fn example() -> anyhow::Result<()> {
//! let mut listener = HostListener::bind("0.0.0.0:7878".parse()?).await?;
//! let (mut conn, mut read_half) = listener.accept().await?;
//!
//! // Spawn a reader task.
//! tokio::spawn(async move {
//!     loop {
//!         match read_message(&mut read_half).await {
//!             Ok(msg) => { /* handle PeerStatus, etc. */ }
//!             Err(_) => break,
//!         }
//!     }
//! });
//!
//! // Send on the write half.
//! // conn.send(&some_wire_message).await?;
//! # Ok(())
//! # }
//! ```

use std::net::SocketAddr;

use anyhow::{Context, Result};
use tokio::io::BufWriter;
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpListener, TcpStream};

use crate::core::messages::WireMessage;
use crate::stream::framing::write_message;

// ── PeerConn ─────────────────────────────────────────────────────────────────

/// Outbound send handle for one peer's TCP connection.
///
/// Wraps the write half of a split [`TcpStream`] with a [`BufWriter`] so that
/// the length prefix, type tag, and bincode payload are emitted in a single
/// syscall batch rather than three separate writes.
///
/// The read half is returned separately by [`HostListener::accept`] and
/// [`connect_to_host`] so the caller can drive it in an independent task.
pub struct PeerConn {
    /// Remote address of this peer.
    pub addr: SocketAddr,
    writer: BufWriter<OwnedWriteHalf>,
}

impl PeerConn {
    fn new(addr: SocketAddr, write_half: OwnedWriteHalf) -> Self {
        Self {
            addr,
            writer: BufWriter::new(write_half),
        }
    }

    /// Serialize and send a [`WireMessage`] to this peer.
    ///
    /// Internally calls [`write_message`], which flushes the buffer before
    /// returning, so the bytes are on the wire when this future resolves.
    pub async fn send(&mut self, msg: &WireMessage) -> Result<()> {
        write_message(&mut self.writer, msg)
            .await
            .with_context(|| format!("sending to peer {}", self.addr))
    }
}

// ── Host side ─────────────────────────────────────────────────────────────────

/// Bound TCP listener for the host side of a watch-party session.
///
/// Bind once with [`HostListener::bind`], then call [`HostListener::accept`]
/// in a loop to obtain one [`PeerConn`] per connecting peer.
pub struct HostListener {
    listener: TcpListener,
}

impl HostListener {
    /// Bind a TCP listener on `addr`.
    ///
    /// Logs the local address so the TUI can display it to the host.
    pub async fn bind(addr: SocketAddr) -> Result<Self> {
        let listener = TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding TCP listener on {addr}"))?;
        tracing::info!("host transport listening on {}", listener.local_addr()?);
        Ok(Self { listener })
    }

    /// The local address this listener is actually bound to.
    ///
    /// Useful when binding on port 0 (OS-assigned port).
    pub fn local_addr(&self) -> Result<SocketAddr> {
        self.listener.local_addr().map_err(Into::into)
    }

    /// Accept the next incoming peer connection.
    ///
    /// Returns `(PeerConn, read_half)`. The caller should spawn a task to
    /// drive `read_half` with [`crate::stream::framing::read_message`] while
    /// using the [`PeerConn`] for outbound sends.
    pub async fn accept(&mut self) -> Result<(PeerConn, OwnedReadHalf)> {
        let (stream, addr) = self
            .listener
            .accept()
            .await
            .context("accepting TCP connection")?;
        tracing::debug!("accepted peer connection from {addr}");
        Ok(split_stream(stream, addr))
    }
}

// ── Peer side ─────────────────────────────────────────────────────────────────

/// Connect to the host's TCP listener.
///
/// Returns `(PeerConn, read_half)`. Same ownership split as the host side:
/// spawn a reader task for `read_half` and keep [`PeerConn`] for sends.
pub async fn connect_to_host(addr: SocketAddr) -> Result<(PeerConn, OwnedReadHalf)> {
    let stream = TcpStream::connect(addr)
        .await
        .with_context(|| format!("connecting to host at {addr}"))?;
    tracing::info!("connected to host at {addr}");
    Ok(split_stream(stream, addr))
}

// ── Internal ──────────────────────────────────────────────────────────────────

fn split_stream(stream: TcpStream, addr: SocketAddr) -> (PeerConn, OwnedReadHalf) {
    let (read_half, write_half) = stream.into_split();
    (PeerConn::new(addr, write_half), read_half)
}

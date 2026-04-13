//! Cross-platform mpv IPC client.
//!
//! mpv exposes a line-delimited JSON socket for control and property queries:
//! - **Unix**: a Unix domain socket at a configurable path.
//! - **Windows**: a named pipe (`\\.\pipe\<name>`).
//!
//! Both platforms use the same JSON wire format:
//! - **Command** (send): `{"command": ["verb", arg…]}\n`
//! - **Response** (receive): `{"data": <value>, "error": "success", "request_id": N}\n`
//! - **Event** (receive, unsolicited): `{"event": "...", …}\n`
//!
//! [`MpvIpc::send_command`] is fire-and-forget (no response read).
//! [`MpvIpc::get_property`] sends a request_id-tagged request and reads
//! lines until the matching response arrives, skipping any events.
//!
//! # Connection retry
//! mpv creates the socket asynchronously after process start. [`MpvIpc::connect`]
//! retries with 100 ms backoff for up to 4 seconds before giving up.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

// ── Platform IPC stream ───────────────────────────────────────────────────────

/// Construct the IPC path for a given process id.
///
/// - **Unix**: `/tmp/watch-party-<pid>.sock`
/// - **Windows**: `\\.\pipe\watch-party-<pid>`
pub fn ipc_path(pid: u32) -> String {
    #[cfg(windows)]
    return format!(r"\\.\pipe\watch-party-{pid}");

    #[cfg(not(windows))]
    return format!("/tmp/watch-party-{pid}.sock");
}

/// Open a platform-specific IPC connection to mpv.
///
/// Returns `(reader, writer)` boxed as `AsyncRead`/`AsyncWrite` trait objects
/// so the rest of the code is platform-agnostic.
async fn platform_connect(
    path: &str,
) -> Result<(
    BufReader<Box<dyn AsyncRead + Unpin + Send>>,
    Box<dyn AsyncWrite + Unpin + Send>,
)> {
    #[cfg(not(windows))]
    {
        let stream = tokio::net::UnixStream::connect(path)
            .await
            .with_context(|| format!("connecting to mpv Unix socket at {path}"))?;
        let (r, w) = tokio::io::split(stream);
        return Ok((BufReader::new(Box::new(r)), Box::new(w)));
    }

    #[cfg(windows)]
    {
        // Named pipes may not exist yet — the retry loop in MpvIpc::connect
        // handles that. Here we just attempt the open.
        let pipe = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(path)
            .with_context(|| format!("opening mpv named pipe at {path}"))?;
        let (r, w) = tokio::io::split(pipe);
        return Ok((BufReader::new(Box::new(r)), Box::new(w)));
    }
}

// ── MpvIpc ───────────────────────────────────────────────────────────────────

/// Async client for mpv's JSON IPC socket.
pub struct MpvIpc {
    reader: BufReader<Box<dyn AsyncRead + Unpin + Send>>,
    writer: Box<dyn AsyncWrite + Unpin + Send>,
    /// Monotonically increasing request_id for tagged get_property calls.
    next_id: u32,
}

impl MpvIpc {
    /// Connect to an mpv IPC socket, retrying until it appears or the timeout
    /// expires.
    ///
    /// Tries every 100 ms for up to `timeout` total. Pass
    /// `Duration::from_secs(4)` as a reasonable default for process startup.
    pub async fn connect(path: &str, timeout: Duration) -> Result<Self> {
        let deadline = tokio::time::Instant::now() + timeout;
        let mut last_err = anyhow::anyhow!("mpv IPC not yet available");

        while tokio::time::Instant::now() < deadline {
            match platform_connect(path).await {
                Ok((reader, writer)) => {
                    tracing::debug!("mpv IPC connected: {path}");
                    return Ok(Self {
                        reader,
                        writer,
                        next_id: 1,
                    });
                }
                Err(e) => {
                    last_err = e;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
        Err(last_err).context(format!("mpv IPC socket did not appear at {path} within {timeout:?}"))
    }

    // ── Commands ──────────────────────────────────────────────────────────────

    /// Send a command to mpv without waiting for a response.
    ///
    /// `args` are the command verb and its arguments, e.g.
    /// `&[json!("set_property"), json!("pause"), json!(true)]`.
    pub async fn send_command(&mut self, args: &[Value]) -> Result<()> {
        let msg = json!({ "command": args });
        self.write_line(&serde_json::to_string(&msg)?).await
    }

    /// Set an mpv property (fire-and-forget).
    pub async fn set_property(&mut self, name: &str, value: Value) -> Result<()> {
        self.send_command(&[json!("set_property"), json!(name), value])
            .await
    }

    // ── Queries ───────────────────────────────────────────────────────────────

    /// Query an mpv property and return its value.
    ///
    /// Sends a tagged `get_property` request and reads lines from mpv until the
    /// matching response arrives. Event lines (which contain `"event"`) are
    /// skipped silently.
    pub async fn get_property(&mut self, name: &str) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;

        let req = json!({
            "command": ["get_property", name],
            "request_id": id,
        });
        self.write_line(&serde_json::to_string(&req)?).await?;

        // Read lines until we find the response for our request_id.
        let mut line_buf = String::new();
        loop {
            line_buf.clear();
            let n = self
                .reader
                .read_line(&mut line_buf)
                .await
                .context("reading mpv IPC response")?;
            if n == 0 {
                bail!("mpv IPC connection closed while waiting for response to get_property({name})");
            }
            let v: Value = match serde_json::from_str(line_buf.trim()) {
                Ok(v) => v,
                Err(_) => continue, // malformed line — skip
            };
            // Skip unsolicited event messages.
            if v.get("event").is_some() {
                continue;
            }
            // Check request_id match.
            if v.get("request_id").and_then(Value::as_u64) == Some(id as u64) {
                match v.get("error").and_then(Value::as_str) {
                    Some("success") => return Ok(v["data"].clone()),
                    Some(err) => bail!("mpv get_property({name}) error: {err}"),
                    None => bail!("mpv response missing error field"),
                }
            }
            // Response for a different request_id (shouldn't happen in our
            // single-requester design, but skip rather than error).
        }
    }

    // ── Convenience wrappers ──────────────────────────────────────────────────

    /// Pause playback.
    pub async fn pause(&mut self) -> Result<()> {
        self.set_property("pause", json!(true)).await
    }

    /// Resume playback.
    pub async fn resume(&mut self) -> Result<()> {
        self.set_property("pause", json!(false)).await
    }

    /// Seek to an absolute playback position in seconds.
    pub async fn seek_absolute(&mut self, pts_secs: f64) -> Result<()> {
        // "absolute" seek mode jumps directly to the given timestamp.
        self.send_command(&[json!("seek"), json!(pts_secs), json!("absolute")])
            .await
    }

    /// Query the current playback position in seconds.
    ///
    /// Returns `0.0` if mpv does not yet have a valid position (e.g. during
    /// buffering). Called by the sync module for drift detection.
    pub async fn playback_time(&mut self) -> Result<f64> {
        match self.get_property("playback-time").await {
            Ok(v) => Ok(v.as_f64().unwrap_or(0.0)),
            // "playback-time" is unavailable during buffering — treat as 0.
            Err(_) => Ok(0.0),
        }
    }

    /// Adjust playback speed as a ratio (1.0 = normal). Used by the sync
    /// module to nudge a drifting peer back on track without a hard seek.
    pub async fn set_speed(&mut self, ratio: f64) -> Result<()> {
        self.set_property("speed", json!(ratio)).await
    }

    /// Set audio volume (0–100).
    pub async fn set_volume(&mut self, pct: u32) -> Result<()> {
        self.set_property("volume", json!(pct)).await
    }

    /// Quit the mpv process via IPC.
    pub async fn quit(&mut self) -> Result<()> {
        self.send_command(&[json!("quit")]).await
    }

    // ── Internal ──────────────────────────────────────────────────────────────

    async fn write_line(&mut self, s: &str) -> Result<()> {
        let mut buf = s.to_owned();
        buf.push('\n');
        self.writer
            .write_all(buf.as_bytes())
            .await
            .context("writing mpv IPC command")?;
        self.writer.flush().await.context("flushing mpv IPC writer")?;
        Ok(())
    }
}

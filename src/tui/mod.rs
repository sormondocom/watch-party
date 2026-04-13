//! Terminal UI — event loop, application state, and background task orchestration.

use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::{mpsc, oneshot};
use tracing::warn;

use crate::core::config::HostConfig;
use crate::core::manifest::SessionManifest;
use crate::disc::source::MediaSource;
use crate::disc::{assemble_manifest, probe_source, scan_keyframes, OpticalDisc, ProbeResult, ScanProgress};

pub mod render;

// ── Application state ─────────────────────────────────────────────────────────

pub enum TuiState {
    /// Waiting for the user to provide a disc path.
    Idle,

    /// ffprobe is running against the source.
    Probing { source_name: String },

    /// Keyframe scan is running; user can press [s] to skip.
    ScanningKeyframes {
        source_name: String,
        probe: ProbeResult,
        keyframes_found: usize,
        fraction: f32,
        started_at: Instant,
    },

    /// Analysis complete — manifest is ready to present to peers.
    ManifestReady {
        source_name: String,
        manifest: SessionManifest,
        probe: ProbeResult,
    },

    /// An error occurred during analysis.
    Error { message: String },
}

// ── Messages from the background analysis task ────────────────────────────────

pub enum AppUpdate {
    /// Probe started (sent immediately before spawning ffprobe).
    Probing,
    /// ffprobe returned successfully.
    ProbeComplete(ProbeResult),
    /// Keyframe scan started; holds the cancel sender so the TUI can abort it.
    ScanStarted(oneshot::Sender<()>),
    /// Progress snapshot from the keyframe scanner.
    ScanProgress(ScanProgress),
    /// Everything done — manifest assembled.
    ManifestReady(SessionManifest, ProbeResult),
    /// Fatal error during analysis.
    Error(String),
}

// ── App ───────────────────────────────────────────────────────────────────────

pub struct App {
    pub state: TuiState,
    pub source_path: Option<PathBuf>,
    pub config: HostConfig,
    /// Receives state updates from the background analysis task.
    update_rx: mpsc::Receiver<AppUpdate>,
    /// Cloned to spawn background tasks.
    update_tx: mpsc::Sender<AppUpdate>,
    /// Held while a keyframe scan is running; dropped or sent to cancel.
    scan_cancel: Option<oneshot::Sender<()>>,
    pub should_quit: bool,
    /// Monotonically incrementing tick counter, used for spinner animation.
    pub tick: usize,
}

impl App {
    pub fn new(source_path: Option<PathBuf>, config: HostConfig) -> Self {
        let (update_tx, update_rx) = mpsc::channel(32);
        Self {
            state: TuiState::Idle,
            source_path,
            config,
            update_rx,
            update_tx,
            scan_cancel: None,
            should_quit: false,
            tick: 0,
        }
    }

    /// Spawn the background analysis task for `self.source_path`.
    pub fn start_analysis(&mut self) {
        let path = match &self.source_path {
            Some(p) => p.clone(),
            None => return,
        };
        let tx = self.update_tx.clone();
        let config = self.config.clone();
        tokio::spawn(analysis_task(path, config, tx));
    }

    /// Apply an update received from the background analysis task.
    pub fn apply_update(&mut self, update: AppUpdate) {
        match update {
            AppUpdate::Probing => {
                let name = self.source_display_name();
                self.state = TuiState::Probing { source_name: name };
            }
            AppUpdate::ProbeComplete(probe) => {
                // Hold the probe result until scan starts or manifest is ready.
                // We store it in the state so the scan screen can show media info.
                if let TuiState::Probing { source_name } = &self.state {
                    let name = source_name.clone();
                    self.state = TuiState::ScanningKeyframes {
                        source_name: name,
                        probe,
                        keyframes_found: 0,
                        fraction: 0.0,
                        started_at: Instant::now(),
                    };
                }
            }
            AppUpdate::ScanStarted(cancel_tx) => {
                self.scan_cancel = Some(cancel_tx);
            }
            AppUpdate::ScanProgress(p) => {
                if let TuiState::ScanningKeyframes { keyframes_found, fraction, .. } = &mut self.state {
                    *keyframes_found = p.keyframes_found;
                    *fraction = p.fraction;
                }
            }
            AppUpdate::ManifestReady(manifest, probe) => {
                let name = self.source_display_name();
                self.state = TuiState::ManifestReady { source_name: name, manifest, probe };
                self.scan_cancel = None;
            }
            AppUpdate::Error(msg) => {
                self.state = TuiState::Error { message: msg };
                self.scan_cancel = None;
            }
        }
    }

    /// Send the cancel signal to the in-progress keyframe scan.
    pub fn skip_keyframe_scan(&mut self) {
        if let Some(tx) = self.scan_cancel.take() {
            let _ = tx.send(());
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('a') => {
                if matches!(
                    self.state,
                    TuiState::Idle | TuiState::ManifestReady { .. } | TuiState::Error { .. }
                ) {
                    self.start_analysis();
                }
            }
            KeyCode::Char('s') => {
                if matches!(self.state, TuiState::ScanningKeyframes { .. }) {
                    self.skip_keyframe_scan();
                }
            }
            _ => {}
        }
    }

    fn source_display_name(&self) -> String {
        self.source_path
            .as_deref()
            .and_then(|p| p.to_str())
            .unwrap_or("disc")
            .to_string()
    }
}

// ── Background analysis task ──────────────────────────────────────────────────

async fn analysis_task(path: PathBuf, config: HostConfig, tx: mpsc::Sender<AppUpdate>) {
    let source = OpticalDisc::new(&path);
    let _ = tx.send(AppUpdate::Probing).await;

    // Step 1: Probe
    let probe = match probe_source(source.input_flags(), source.input_spec()).await {
        Ok(p) => p,
        Err(e) => {
            let _ = tx.send(AppUpdate::Error(format!("Probe failed: {e}"))).await;
            return;
        }
    };
    let _ = tx.send(AppUpdate::ProbeComplete(probe.clone())).await;

    // Step 2: Keyframe scan (when config calls for it and duration is known)
    let keyframes = if config.stream.keyframe_snap {
        if let Some(dur) = probe.duration_secs {
            let (progress_tx, mut progress_rx) = mpsc::channel::<ScanProgress>(16);
            let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
            let _ = tx.send(AppUpdate::ScanStarted(cancel_tx)).await;

            // Relay progress reports to the TUI.
            let tx2 = tx.clone();
            tokio::spawn(async move {
                while let Some(p) = progress_rx.recv().await {
                    let _ = tx2.send(AppUpdate::ScanProgress(p)).await;
                }
            });

            match scan_keyframes(source.input_flags(), source.input_spec(), dur, progress_tx, cancel_rx).await {
                Ok(kf) => kf,
                Err(e) => {
                    warn!("keyframe scan error: {e}");
                    vec![]
                }
            }
        } else {
            vec![]
        }
    } else {
        vec![]
    };

    // Step 3: Assemble manifest (synchronous, instant)
    // Zeroed session_id and stream_start_utc — the session layer fills these in.
    let manifest = match assemble_manifest(&probe, &keyframes, None, &config, [0u8; 32], 0) {
        Ok(m) => m,
        Err(e) => {
            let _ = tx.send(AppUpdate::Error(format!("Manifest error: {e}"))).await;
            return;
        }
    };

    let _ = tx.send(AppUpdate::ManifestReady(manifest, probe)).await;
}

// ── Terminal lifecycle ────────────────────────────────────────────────────────

/// RAII guard: restores the terminal when dropped, even on panic.
pub struct TerminalGuard;

impl TerminalGuard {
    fn setup() -> Result<(Terminal<CrosstermBackend<io::Stdout>>, Self)> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok((terminal, TerminalGuard))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

// ── Main event loop ───────────────────────────────────────────────────────────

pub async fn run(app: &mut App) -> Result<()> {
    let (mut terminal, _guard) = TerminalGuard::setup()?;
    let mut events = EventStream::new();

    // Auto-start analysis if a path was provided on the command line.
    if app.source_path.is_some() {
        app.start_analysis();
    }

    loop {
        app.tick = app.tick.wrapping_add(1);
        terminal.draw(|f| render::draw(f, app))?;

        tokio::select! {
            maybe_event = events.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) => app.handle_key(key),
                    Some(Err(e)) => return Err(e.into()),
                    _ => {}
                }
            }
            Some(update) = app.update_rx.recv() => {
                app.apply_update(update);
            }
            // Tick at 10Hz so the spinner animates even without events.
            _ = tokio::time::sleep(Duration::from_millis(100)) => {}
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

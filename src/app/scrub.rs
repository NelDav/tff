use anyhow::Context;

use super::{App, Focus, Mode, TextTarget};
use crate::ffmpeg;
use crate::graph::{FilterName, ModifierKind, NodeId, Resolved, Target};

/// Which player is behind the active scrub session, and however each one
/// needs to be reached -- see `App::start_scrub`, which picks one based on
/// whether mpv is installed (preferred: full remote control either way)
/// and whether a display is available (only relevant to mpv's own
/// rendering choice, and to whether ffplay can run at all).
enum ScrubBackend {
    /// mpv, fully remote-controllable over its IPC socket either way --
    /// `headless` only affects how it renders (its own window vs
    /// `--vo=tct` into this terminal, see `ffmpeg::spawn_scrub_mpv`) and
    /// so whether `main.rs` needs to divert into its dedicated
    /// headless-scrub loop for the session (see `scrub_is_headless_mpv`).
    Mpv { socket_path: String, headless: bool },
    /// ffplay in its own window -- the fallback when mpv isn't installed,
    /// only possible at all when a display is (ffplay can't render
    /// without one, and has no terminal mode of its own). No remote
    /// control exists for it (see `ffmpeg::spawn_scrub_ffplay`'s doc
    /// comment) -- `position` is filled in passively, by scraping its
    /// stderr on a background thread, and is the only way tff knows where
    /// it currently is.
    Ffplay { position: std::sync::Arc<std::sync::Mutex<Option<f64>>> },
}

/// The live player process behind an active `Mode::Scrub` session --
/// launched by `App::start_scrub` on the real file feeding a Trim
/// modifier's input, so the user can find exact trim points by eye.
pub struct ScrubSession {
    modifier_id: NodeId,
    /// Kept so `App::restart_ffplay_at` ('g' on the ffplay backend) can
    /// relaunch on the same file without re-resolving the graph.
    path: String,
    child: std::process::Child,
    backend: ScrubBackend,
}

impl Drop for ScrubSession {
    /// Kills the player and (for the mpv backend) removes its socket file
    /// -- runs whenever a session ends, whether via `App::close_scrub` or
    /// (as a safety net) `App` itself being dropped with one still open,
    /// e.g. quitting tff without closing the scrub session first. ffplay's
    /// position-reader thread needs no explicit stop signal: killing the
    /// child closes its stderr pipe, which is what that thread is blocked
    /// reading, so it exits on its own right after this returns.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let ScrubBackend::Mpv { socket_path, .. } = &self.backend {
            let _ = std::fs::remove_file(socket_path);
        }
    }
}

impl App {
    /// 's': focus a Trim modifier with a connected source and start a
    /// scrub session on the *original* file feeding it (not a rendered
    /// preview), so exact start/end timestamps can be found by eye, then
    /// drops into `Mode::Scrub` for the dedicated playback/mark keys (see
    /// `main.rs`). Picks a backend in this order:
    /// - mpv in its own window, if one's installed and a display is
    ///   available -- the best of both worlds (full remote control, real
    ///   video).
    /// - mpv rendering `--vo=tct` into this terminal, if one's installed
    ///   but there's no display -- still fully remote-controlled, just
    ///   without a separate window (see `main.rs`'s dedicated
    ///   `run_headless_scrub` loop, needed since mpv's video and ratatui's
    ///   own drawing can't share one terminal at the same time).
    /// - ffplay in its own window, if mpv isn't installed but a display
    ///   is -- no remote control at all (see `ScrubBackend::Ffplay`'s doc
    ///   comment), but better than nothing.
    /// - otherwise (no mpv, no display), scrubbing just isn't available:
    ///   ffplay can't render without a display either.
    ///
    /// Refuses a source that traces back through a `ModifierKind::Concat`
    /// node regardless of backend: there's no single file to hand either
    /// player for a still-virtual concatenated timeline (see
    /// `Resolved::Concat`) -- trim each segment before concatenating
    /// instead.
    pub fn start_scrub(&mut self) {
        let Focus::Modifier(i) = self.focus else {
            self.log.push("focus a trim node first, then 's' to scrub".to_string());
            return;
        };
        let Some(m) = self.graph.modifiers.get(i) else { return };
        let ModifierKind::Filter { name: FilterName::Trim, fields } = &m.kind else {
            self.log.push("only a trim node can be scrubbed".to_string());
            return;
        };
        let mid = m.id;
        let Some(wire) = self.graph.wires.iter().find(|w| w.to == Target::ModifierIn(mid)) else {
            self.log.push("connect a stream to this node first, then 's' to scrub".to_string());
            return;
        };
        let Some(resolved) = self.graph.resolve(wire.from) else {
            self.log.push("broken chain -- fix the connection first".to_string());
            return;
        };
        let Resolved::Stream { from_node, from_stream_idx, .. } = &resolved else {
            self.log.push(
                "can't scrub a concatenated source -- trim each segment before concatenating instead".to_string(),
            );
            return;
        };
        let Some(input) = self.graph.input(*from_node) else { return };
        if input.streams.get(*from_stream_idx).is_none() {
            return;
        }
        let path = input.path.clone();

        // Re-opening a scrub session on a Trim node that already has a
        // start marked picks up near there instead of at 0:00.
        let start_secs = fields.get("start").and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);

        let has_display = ffmpeg::has_display();
        if !ffmpeg::mpv_is_installed() {
            if !has_display {
                self.log.push(
                    "can't scrub -- no display available and mpv isn't installed (ffplay can't render \
                     without a display either)"
                        .to_string(),
                );
                return;
            }
            self.start_scrub_ffplay(mid, path, start_secs);
            return;
        }

        // Captures this terminal's window as the one to reactivate --
        // called before the player exists, so it's *this* window that
        // gets recorded, not the player's own once it opens and (by
        // default, on most window managers) steals focus for itself. A
        // no-op (and pointless) for the headless case, since mpv renders
        // into this same terminal there rather than a separate window.
        if has_display {
            ffmpeg::try_refocus_terminal();
        }
        let socket_path = std::env::temp_dir().join(format!("tff-scrub-{mid}.sock")).to_string_lossy().into_owned();
        match ffmpeg::spawn_scrub_mpv(&path, &socket_path, start_secs, !has_display) {
            Ok(child) => {
                self.scrub =
                    Some(ScrubSession { modifier_id: mid, path, child, backend: ScrubBackend::Mpv { socket_path, headless: !has_display } });
                self.mode = Mode::Scrub;
                self.log.push(if has_display {
                    "scrubbing in mpv -- if its window takes focus, click back into this terminal to use \
                     these keys: space play/pause, h/l or \u{2190}/\u{2192} step a frame, Shift+\u{2190}/\u{2192} \
                     seek 1s, g jump to time, i/o mark start/end, Esc to close"
                        .to_string()
                } else {
                    "scrubbing in mpv (no display available) -- space play/pause, h/l or \u{2190}/\u{2192} \
                     step a frame, Shift+\u{2190}/\u{2192} seek 1s, g jump to time, i/o mark start/end, \
                     Esc to close"
                        .to_string()
                });
            }
            Err(e) => {
                self.log.push(format!("couldn't start scrubbing: {e:#}"));
            }
        }
    }

    fn start_scrub_ffplay(&mut self, mid: NodeId, path: String, start_secs: f64) {
        ffmpeg::try_refocus_terminal();
        match ffmpeg::spawn_scrub_ffplay(&path, start_secs) {
            Ok(mut child) => {
                let stderr = child.stderr.take().expect("spawn_scrub_ffplay pipes stderr");
                let position = std::sync::Arc::new(std::sync::Mutex::new(None));
                ffmpeg::spawn_ffplay_position_reader(stderr, position.clone());
                self.scrub = Some(ScrubSession { modifier_id: mid, path, child, backend: ScrubBackend::Ffplay { position } });
                self.mode = Mode::Scrub;
                self.log.push(
                    "mpv isn't installed -- scrubbing in ffplay instead, which has no remote control: use \
                     its own window (space pause, left/right seek 10s, s step forward, no reverse step). \
                     Back in this terminal: g jump to time (restarts ffplay), i/o mark start/end, Esc to close"
                        .to_string(),
                );
            }
            Err(e) => {
                self.log.push(format!("couldn't start scrubbing: {e:#}"));
            }
        }
    }

    /// Runs `f` against the active session's mpv socket, or (for the
    /// ffplay backend, which has no remote control at all) logs that this
    /// key isn't relayed -- shared by every `Mode::Scrub` key that sends a
    /// fire-and-forget playback command, since those only differ in which
    /// command they'd send.
    fn run_scrub_command(&mut self, f: impl FnOnce(&str) -> anyhow::Result<()>) {
        let Some(session) = &self.scrub else { return };
        match &session.backend {
            ScrubBackend::Mpv { socket_path, .. } => {
                let socket_path = socket_path.clone();
                if let Err(e) = f(&socket_path) {
                    self.log.push(format!("scrub command failed: {e:#}"));
                }
            }
            ScrubBackend::Ffplay { .. } => self.log_ffplay_has_no_remote_control(),
        }
    }

    fn log_ffplay_has_no_remote_control(&mut self) {
        self.log.push(
            "ffplay has no remote control -- use its own window: space pause, left/right seek, s step forward"
                .to_string(),
        );
    }

    /// Space in `Mode::Scrub`: toggles mpv's play/pause.
    pub fn scrub_play_pause(&mut self) {
        self.run_scrub_command(ffmpeg::mpv_toggle_pause);
    }

    /// h/l or \u{2190}/\u{2192} in `Mode::Scrub`: steps mpv one video frame
    /// forward/back -- the finest unit that makes sense for a video trim
    /// point (not an audio sample; see the feature's own discussion of that
    /// tradeoff).
    pub fn scrub_step_frame(&mut self, forward: bool) {
        self.run_scrub_command(move |socket| ffmpeg::mpv_frame_step(socket, forward));
    }

    /// Shift+\u{2190}/\u{2192} in `Mode::Scrub`: seeks mpv by roughly a
    /// second, to cover ground faster than frame-stepping.
    pub fn scrub_seek_relative(&mut self, forward: bool) {
        let delta = if forward { 1.0 } else { -1.0 };
        self.run_scrub_command(move |socket| ffmpeg::mpv_seek_relative(socket, delta));
    }

    /// 'g' in `Mode::Scrub`: prompts for an absolute time (same
    /// `HH:MM:SS`/seconds grammar as Trim's own fields) to jump to.
    pub fn start_scrub_seek(&mut self) {
        if self.scrub.is_none() {
            return;
        }
        self.mode = super::text_input::text_input_mode(TextTarget::ScrubSeek, String::new(), Vec::new());
    }

    /// Confirming the `ScrubSeek` text input (see `text_input`'s
    /// `confirm_text_input`): mpv gets seeked there directly over IPC;
    /// ffplay, with no remote seek of any kind, gets killed and relaunched
    /// fresh at that position instead (see `restart_ffplay_at`) -- a
    /// visible window flash, but the only way to move it externally at all.
    pub fn scrub_seek_absolute(&mut self, secs: f64) {
        let Some(session) = &self.scrub else { return };
        match &session.backend {
            ScrubBackend::Mpv { socket_path, .. } => {
                let socket_path = socket_path.clone();
                if let Err(e) = ffmpeg::mpv_seek_absolute(&socket_path, secs) {
                    self.log.push(format!("scrub command failed: {e:#}"));
                }
            }
            ScrubBackend::Ffplay { .. } => self.restart_ffplay_at(secs),
        }
    }

    fn restart_ffplay_at(&mut self, secs: f64) {
        let Some(session) = &mut self.scrub else { return };
        let path = session.path.clone();
        let _ = session.child.kill();
        let _ = session.child.wait();
        match ffmpeg::spawn_scrub_ffplay(&path, secs) {
            Ok(mut child) => {
                let stderr = child.stderr.take().expect("spawn_scrub_ffplay pipes stderr");
                let position = std::sync::Arc::new(std::sync::Mutex::new(None));
                ffmpeg::spawn_ffplay_position_reader(stderr, position.clone());
                session.child = child;
                session.backend = ScrubBackend::Ffplay { position };
                self.log.push(format!("jumped to {}", crate::graph::format_time(secs)));
            }
            Err(e) => self.log.push(format!("couldn't restart ffplay: {e:#}")),
        }
    }

    /// 'i'/'o' in `Mode::Scrub`: reads the player's current position
    /// (queried live over IPC for mpv, or the last value scraped from
    /// ffplay's stderr -- see `ScrubBackend`) and writes it into the
    /// scrubbed Trim node's `start`/`end` field (`field` is one of those
    /// two key names) -- stored as plain seconds, same convention every
    /// other Trim-time write uses (see `text_input`'s `ModifierFilterValue`
    /// handling).
    pub fn mark_scrub_point(&mut self, field: &str) {
        let Some(session) = &self.scrub else { return };
        let modifier_id = session.modifier_id;
        let result: anyhow::Result<f64> = match &session.backend {
            ScrubBackend::Mpv { socket_path, .. } => ffmpeg::mpv_get_time_pos(socket_path),
            ScrubBackend::Ffplay { position } => position
                .lock()
                .ok()
                .and_then(|guard| *guard)
                .context("no position reported by ffplay yet -- give it a moment to start playing"),
        };
        match result {
            Ok(secs) => {
                if let Some(m) = self.graph.modifier_mut(modifier_id)
                    && let ModifierKind::Filter { fields, .. } = &mut m.kind
                {
                    fields.insert(field.to_string(), secs.to_string());
                    self.log.push(format!("{field} marked at {}", crate::graph::format_time(secs)));
                }
            }
            Err(e) => {
                self.log.push(format!("couldn't read the current position: {e:#}"));
            }
        }
    }

    /// Esc/'q' in `Mode::Scrub`: ends the session (see `ScrubSession`'s
    /// `Drop`, which does the actual player-kill/socket-cleanup) and
    /// returns to Normal.
    pub fn close_scrub(&mut self) {
        self.scrub = None;
        self.mode = Mode::Normal;
    }

    /// Whether the active scrub session (if any) is the headless mpv
    /// backend -- `main.rs`'s main loop checks this to divert into its own
    /// dedicated interactive loop instead of the normal draw-then-read-key
    /// cycle, since a headless mpv session shares this same terminal with
    /// mpv's own `--vo=tct` video output (see `ffmpeg::spawn_scrub_mpv`'s
    /// doc comment) -- every other backend/mode needs no such thing, since
    /// it draws into its own separate window (or doesn't draw at all).
    pub fn scrub_is_headless_mpv(&self) -> bool {
        matches!(&self.scrub, Some(ScrubSession { backend: ScrubBackend::Mpv { headless: true, .. }, .. }))
    }
}

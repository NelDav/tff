use super::{App, Focus, Mode, TextTarget};
use crate::ffmpeg;
use crate::graph::{FilterName, ModifierKind, NodeId, Resolved, Target};

/// The live mpv process behind an active `Mode::Scrub` session -- launched
/// by `App::start_scrub` on the real file feeding a Trim modifier's input,
/// and driven from then on entirely through its IPC socket (seek,
/// frame-step, query position), so the user can find exact trim points by
/// eye without leaving tff's own keybindings.
pub struct ScrubSession {
    modifier_id: NodeId,
    child: std::process::Child,
    socket_path: String,
}

impl Drop for ScrubSession {
    /// Kills mpv and removes its socket file -- runs whenever a session
    /// ends, whether via `App::close_scrub` or (as a safety net) `App`
    /// itself being dropped with one still open, e.g. quitting tff without
    /// closing the scrub session first.
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl App {
    /// 's': focus a Trim modifier with a connected source and start a
    /// scrub session -- launches mpv on the *original* file feeding it (not
    /// a rendered preview) so exact start/end timestamps can be found by
    /// eye, then drops into `Mode::Scrub` for the dedicated playback/mark
    /// keys (see `main.rs`). Refuses a source that traces back through a
    /// `ModifierKind::Concat` node: there's no single file to hand mpv for
    /// a still-virtual concatenated timeline (see `Resolved::Concat`) --
    /// trim each segment before concatenating instead.
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
        let socket_path =
            std::env::temp_dir().join(format!("tff-scrub-{mid}.sock")).to_string_lossy().into_owned();

        // Captures this terminal's window as the one to reactivate --
        // called before mpv exists, so it's *this* window that gets
        // recorded, not mpv's own once it opens and (by default, on most
        // window managers) steals focus for itself.
        ffmpeg::try_refocus_terminal();

        match ffmpeg::spawn_scrub_mpv(&path, &socket_path, start_secs) {
            Ok(child) => {
                self.scrub = Some(ScrubSession { modifier_id: mid, child, socket_path });
                self.mode = Mode::Scrub;
                self.log.push(format!(
                    "scrubbing {path} in mpv -- if its window takes focus, click back into this \
                     terminal to use these keys: space play/pause, h/l or \u{2190}/\u{2192} step a frame, \
                     Shift+\u{2190}/\u{2192} seek 1s, g jump to time, i/o mark start/end, Esc to close"
                ));
            }
            Err(e) => {
                self.log.push(format!("couldn't start scrubbing: {e:#}"));
            }
        }
    }

    /// Runs an mpv IPC command against the active scrub session's socket,
    /// logging a failure -- shared by every `Mode::Scrub` key that doesn't
    /// need mpv's reply itself (playback control), since those only differ
    /// in which command they send.
    fn run_scrub_command(&mut self, f: impl FnOnce(&str) -> anyhow::Result<()>) {
        let Some(socket_path) = self.scrub.as_ref().map(|s| s.socket_path.clone()) else { return };
        if let Err(e) = f(&socket_path) {
            self.log.push(format!("scrub command failed: {e:#}"));
        }
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
    /// `HH:MM:SS`/seconds grammar as Trim's own fields) to jump mpv to.
    pub fn start_scrub_seek(&mut self) {
        if self.scrub.is_none() {
            return;
        }
        self.mode = super::text_input::text_input_mode(TextTarget::ScrubSeek, String::new(), Vec::new());
    }

    /// Confirming the `ScrubSeek` text input (see `text_input`'s
    /// `confirm_text_input`): parses the typed time and seeks mpv there.
    pub fn scrub_seek_absolute(&mut self, secs: f64) {
        self.run_scrub_command(move |socket| ffmpeg::mpv_seek_absolute(socket, secs));
    }

    /// 'i'/'o' in `Mode::Scrub`: reads mpv's current position and writes it
    /// into the scrubbed Trim node's `start`/`end` field (`field` is one of
    /// those two key names) -- stored as plain seconds, same convention
    /// every other Trim-time write uses (see `text_input`'s
    /// `ModifierFilterValue` handling).
    pub fn mark_scrub_point(&mut self, field: &str) {
        let Some(session) = &self.scrub else { return };
        let (socket_path, modifier_id) = (session.socket_path.clone(), session.modifier_id);
        match ffmpeg::mpv_get_time_pos(&socket_path) {
            Ok(secs) => {
                if let Some(m) = self.graph.modifier_mut(modifier_id)
                    && let ModifierKind::Filter { fields, .. } = &mut m.kind
                {
                    fields.insert(field.to_string(), secs.to_string());
                    self.log.push(format!("{field} marked at {}", crate::graph::format_time(secs)));
                }
            }
            Err(e) => {
                self.log.push(format!("couldn't read mpv's position: {e:#}"));
            }
        }
    }

    /// Esc/'q' in `Mode::Scrub`: ends the session (see `ScrubSession`'s
    /// `Drop`, which does the actual mpv-kill/socket-cleanup) and returns
    /// to Normal.
    pub fn close_scrub(&mut self) {
        self.scrub = None;
        self.mode = Mode::Normal;
    }
}

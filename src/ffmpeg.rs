use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::graph::{Chapter, StreamInfo, StreamKind};

/// Resolves which `ffmpeg`/`ffprobe`/`ffplay` binary to actually run.
/// Normally just `name` itself, resolved via `PATH` as always -- but with
/// `TFF_FFMPEG_DIR` set, joins that directory with `name` instead, so a
/// specific ffmpeg installation can be picked when several are installed
/// side by side (e.g. to try a newer/older build without touching `PATH`).
/// Not persisted anywhere: this only takes effect for as long as the env
/// var is set, same as any other env var -- by design, for now, per the
/// user's request to leave persistence as a later decision.
fn binary(name: &str) -> String {
    binary_from(name, std::env::var_os("TFF_FFMPEG_DIR"))
}

/// The actual decision logic behind `binary`, taking the env var's value
/// directly so a test can exercise both branches deterministically instead
/// of depending on whatever the machine running the test happens to have
/// `TFF_FFMPEG_DIR` set to (nothing, ordinarily).
pub(crate) fn binary_from(name: &str, ffmpeg_dir: Option<std::ffi::OsString>) -> String {
    match ffmpeg_dir {
        Some(dir) => {
            let file_name = if cfg!(windows) { format!("{name}.exe") } else { name.to_string() };
            std::path::Path::new(&dir).join(file_name).to_string_lossy().into_owned()
        }
        None => name.to_string(),
    }
}

/// What `probe` reports about a file: its real media streams (empty for a
/// chapters-only FFMETADATA text file added as an input) and its chapters
/// (empty for a file with none) -- see `probe`'s doc comment.
pub struct ProbeResult {
    pub streams: Vec<StreamInfo>,
    pub chapters: Vec<Chapter>,
}

#[derive(Deserialize)]
struct ProbeOutput {
    #[serde(default)]
    streams: Vec<ProbeStream>,
    #[serde(default)]
    chapters: Vec<ProbeChapter>,
}

#[derive(Deserialize)]
struct ProbeStream {
    index: usize,
    codec_type: String,
    codec_name: Option<String>,
    #[serde(default)]
    tags: Option<ProbeTags>,
}

#[derive(Deserialize)]
struct ProbeTags {
    language: Option<String>,
}

#[derive(Deserialize)]
struct ProbeChapter {
    start_time: String,
    end_time: String,
    #[serde(default)]
    tags: Option<ProbeChapterTags>,
}

#[derive(Deserialize)]
struct ProbeChapterTags {
    title: Option<String>,
}

/// Run ffprobe on a file and return the streams and chapters it reports.
///
/// Verified against a real ffprobe build that this also works, unmodified,
/// for a plain FFMETADATA text file (the same format `chapters_ffmetadata`
/// writes): ffprobe autodetects it from content (no explicit `-f` needed),
/// reports an empty `streams` array and a populated `chapters` one, exit
/// code 0 -- so a chapters-only text file added as an input needs no
/// special-case handling here at all, it's just a file whose `streams`
/// happens to come back empty. A real chaptered media file reports both
/// arrays populated, same shape. Only a genuinely unreadable/invalid file
/// fails outright, which is when this still bails as before.
pub fn probe(path: &str) -> Result<ProbeResult> {
    let output = Command::new(binary("ffprobe"))
        .args([
            "-v", "error",
            "-show_streams",
            "-show_chapters",
            "-of", "json",
            path,
        ])
        .output()
        .context("failed to run ffprobe (is it installed and on PATH?)")?;

    if !output.status.success() {
        bail!(
            "ffprobe failed for '{path}': {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let parsed: ProbeOutput =
        serde_json::from_slice(&output.stdout).context("failed to parse ffprobe output")?;

    if parsed.streams.is_empty() && parsed.chapters.is_empty() {
        bail!("ffprobe found no streams or chapters in '{path}'");
    }

    let streams = parsed
        .streams
        .into_iter()
        .map(|s| StreamInfo {
            index: s.index,
            kind: match s.codec_type.as_str() {
                "video" => StreamKind::Video,
                "audio" => StreamKind::Audio,
                "subtitle" => StreamKind::Subtitle,
                _ => StreamKind::Other,
            },
            codec: s.codec_name.unwrap_or_else(|| "?".to_string()),
            lang: s.tags.and_then(|t| t.language),
        })
        .collect();

    // ffprobe already reports start_time/end_time as decimal seconds
    // (converted from whatever the source's own TIMEBASE is), so there's no
    // manual timebase math to do here, unlike this app's own FFMETADATA
    // writer which has to construct that fraction itself.
    let chapters = parsed
        .chapters
        .into_iter()
        .filter_map(|c| {
            let start_secs = c.start_time.trim().parse::<f64>().ok()?;
            let end_secs = c.end_time.trim().parse::<f64>().ok()?;
            let title = c.tags.and_then(|t| t.title).unwrap_or_default();
            Some(Chapter::new(start_secs, end_secs, title))
        })
        .collect();

    Ok(ProbeResult { streams, chapters })
}

/// Ask ffmpeg which encoders it was actually built with, so the codec picker
/// can offer the real, complete list instead of a guessed-at curated one.
/// Parses `ffmpeg -encoders` output, e.g.:
///   " V....D a64multi             Multicolor charset for Commodore 64 ..."
pub fn list_encoders() -> Result<Vec<(String, StreamKind)>> {
    let output = Command::new(binary("ffmpeg"))
        .args(["-hide_banner", "-encoders"])
        .output()
        .context("failed to run ffmpeg -encoders")?;
    if !output.status.success() {
        bail!("ffmpeg -encoders exited with an error");
    }
    Ok(parse_encoders(&String::from_utf8_lossy(&output.stdout)))
}

/// The "---" divider line between an `-encoders`/`-muxers` listing's legend
/// and its actual entries. Its width scales with however many flag columns
/// that particular listing has (verified against a real ffmpeg build where
/// muxers' 2-column D/E legend gets a 2-dash divider while encoders' 6-column
/// one gets 6), so this only checks for *a* dash, never a specific count.
fn is_listing_divider(line: &str) -> bool {
    line.trim_start().starts_with('-')
}

/// Each entry's flags and name are just its first two whitespace-separated
/// fields, regardless of how many flag letters/columns a given ffmpeg build
/// prints (that's changed across versions, e.g. the "frame-level
/// multithreading"/"slice-level multithreading" columns are relatively
/// recent additions) -- so this only relies on the media-type flag (V/A/S)
/// being the first character of that first field, never on the flag
/// block's exact width.
pub(crate) fn parse_encoders(text: &str) -> Vec<(String, StreamKind)> {
    let mut started = false;
    let mut encoders = Vec::new();
    for line in text.lines() {
        if !started {
            if is_listing_divider(line) {
                started = true;
            }
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(flags) = fields.next() else { continue };
        let kind = match flags.as_bytes().first() {
            Some(b'V') => StreamKind::Video,
            Some(b'A') => StreamKind::Audio,
            Some(b'S') => StreamKind::Subtitle,
            _ => continue,
        };
        if let Some(name) = fields.next() {
            encoders.push((name.to_string(), kind));
        }
    }
    encoders
}

/// Ask ffmpeg which muxers (container formats) it was actually built with.
/// Parses `ffmpeg -muxers` output, e.g.:
///   "  E  3g2             3GP2 (3GPP2 file format)"
pub fn list_muxers() -> Result<Vec<String>> {
    let output = Command::new(binary("ffmpeg"))
        .args(["-hide_banner", "-muxers"])
        .output()
        .context("failed to run ffmpeg -muxers")?;
    if !output.status.success() {
        bail!("ffmpeg -muxers exited with an error");
    }
    Ok(parse_muxers(&String::from_utf8_lossy(&output.stdout)))
}

/// Same field-based approach as `parse_encoders`: the muxing flag and name
/// are just the first two whitespace-separated fields, whatever the flag
/// block's width happens to be on this ffmpeg build. Unlike encoders'
/// media-type flag, an entry's muxing flag ('E') isn't always the first
/// character of that field -- demux-only entries can share this listing
/// with a blank 'E' slot -- so this checks the whole field for 'E' rather
/// than just its first byte.
pub(crate) fn parse_muxers(text: &str) -> Vec<String> {
    let mut started = false;
    let mut muxers = Vec::new();
    for line in text.lines() {
        if !started {
            if is_listing_divider(line) {
                started = true;
            }
            continue;
        }
        let mut fields = line.split_whitespace();
        let Some(flags) = fields.next() else { continue };
        if !flags.contains('E') {
            continue;
        }
        if let Some(name) = fields.next() {
            muxers.push(name.to_string());
        }
    }
    muxers
}

/// Whether the configured ffmpeg (see `binary`) has the libzvbi-based
/// teletext decoder built in -- most distro ffmpeg packages don't (it's an
/// optional `--enable-libzvbi` build flag), and without it the decoder's
/// own private options (`txt_format`/`txt_page`/`txt_duration`) aren't
/// even recognized as valid flags at all, let alone usable, so pickers use
/// this to decide whether to offer them as curated input extra-args in the
/// first place. `false` if the query itself fails for any reason, same
/// fail-safe default as `list_encoders`/`list_muxers`.
pub fn has_teletext_decoder() -> bool {
    let Ok(output) = Command::new(binary("ffmpeg")).args(["-hide_banner", "-decoders"]).output() else {
        return false;
    };
    decoders_include_teletext(&String::from_utf8_lossy(&output.stdout))
}

pub(crate) fn decoders_include_teletext(decoders_output: &str) -> bool {
    decoders_output.contains("libzvbi_teletextdec")
}

/// Launch ffplay on a rendered preview file, in its own window. Runs
/// detached: unlike `run_args`, nothing here waits for or streams output
/// from the player process, since it's the user who decides when they're
/// done looking and closes the window themselves.
pub fn play(path: &str) -> Result<()> {
    Command::new(binary("ffplay"))
        .args(["-hide_banner", "-autoexit", "-window_title", "tff preview", path])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn ffplay (is it installed and on PATH?)")?;
    Ok(())
}

/// Whether a graphical display is available for `ffplay` to open a window
/// on -- checked via the same env vars X11 and Wayland clients both rely
/// on. `false` in a bare SSH session with no X forwarding, which is when
/// `play_in_terminal` (mpv's terminal video output) is used instead.
pub fn has_display() -> bool {
    has_display_from(std::env::var_os("DISPLAY").is_some(), std::env::var_os("WAYLAND_DISPLAY").is_some())
}

/// A display counts as available if either X11's or Wayland's env var is
/// set. Split out from `has_display` as a pure function of both booleans so
/// it can be tested deterministically, independent of whatever env vars
/// the machine running the test actually has set.
pub(crate) fn has_display_from(has_display_var: bool, has_wayland_display_var: bool) -> bool {
    has_display_var || has_wayland_display_var
}

/// Play a rendered preview file directly in the terminal via `mpv`'s
/// truecolor terminal video output (`--vo=tct`, Unicode half-block
/// characters + ANSI truecolor -- no X11/Wayland involved) -- the fallback
/// for when `play` has no display to open an ffplay window on. Unlike
/// `play`, this blocks until playback finishes: mpv draws straight to this
/// process's own stdout, so there's no separate window to leave running
/// detached, and the caller is responsible for having already yielded the
/// TUI's terminal (raw mode / alternate screen) before calling this and
/// restoring it again afterward. No explicit "exit when done" flag needed,
/// unlike `play`'s `-autoexit`/tplay's `--auto-exit`: exiting once playback
/// finishes is mpv's own default CLI behavior.
pub fn play_in_terminal(path: &str) -> Result<()> {
    let status = Command::new("mpv")
        .args(["--vo=tct", path])
        .status()
        .context("failed to run mpv (is it installed and on PATH?)")?;
    if !status.success() {
        bail!("mpv exited with an error");
    }
    Ok(())
}

/// Starts an mpv instance in its own window, playing `path` from
/// `start_secs`, with its IPC server bound to `socket_path` -- lets
/// `App::start_scrub` control it (seek, frame-step, query position) while
/// the user watches the actual video, not a terminal approximation of it.
/// Unlike `play`/`play_in_terminal`, left running detached and driven
/// entirely through the socket from then on.
#[cfg(unix)]
pub fn spawn_scrub_mpv(path: &str, socket_path: &str, start_secs: f64) -> Result<std::process::Child> {
    let _ = std::fs::remove_file(socket_path); // stale socket from a crashed session, if any
    Command::new("mpv")
        .args([
            format!("--input-ipc-server={socket_path}"),
            "--title=tff scrub".to_string(),
            format!("--start={start_secs}"),
            path.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn mpv (is it installed and on PATH?)")
}

/// mpv's IPC server binds a Unix domain socket -- there's no equivalent
/// this reaches for on Windows (mpv uses a named pipe there instead, a
/// different API), so scrubbing just isn't available there yet.
#[cfg(not(unix))]
pub fn spawn_scrub_mpv(_path: &str, _socket_path: &str, _start_secs: f64) -> Result<std::process::Child> {
    bail!("scrubbing isn't supported on this platform yet (mpv's IPC socket is Unix-only)")
}

/// Best-effort: hands keyboard focus back to whatever window currently has
/// it (this terminal, if called right before `spawn_scrub_mpv`) a moment
/// later, since a freshly opened mpv window otherwise grabs focus itself --
/// window managers focus newly mapped windows by default -- which would
/// leave every `Mode::Scrub` keypress going to mpv's own bindings instead
/// of tff's. Needs `xdotool` and an X11 `DISPLAY` (there's no standard
/// equivalent way to reassign focus from an arbitrary client on Wayland);
/// silently does nothing if either is missing, so the caller should still
/// tell the user to click back into the terminal themselves as a fallback
/// (see `App::start_scrub`'s log message). Runs the actual reactivation on
/// its own thread after a short delay, so tff's own event loop isn't
/// blocked waiting for mpv's window to finish appearing.
#[cfg(unix)]
pub fn try_refocus_terminal() {
    if std::env::var_os("DISPLAY").is_none() {
        return;
    }
    let Ok(output) = Command::new("xdotool").arg("getactivewindow").output() else {
        return; // xdotool not installed -- nothing this can do
    };
    if !output.status.success() {
        return;
    }
    let window_id = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if window_id.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(400));
        let _ = Command::new("xdotool").args(["windowactivate", &window_id]).status();
    });
}

#[cfg(not(unix))]
pub fn try_refocus_terminal() {}

/// One line of an mpv IPC reply: either an unsolicited event notification
/// (`event` set, e.g. `{"event":"pause"}`, fired by the user interacting
/// with mpv's own window between our commands) or a genuine reply to
/// something we sent (`data` set to whatever that command returns, `null`
/// for a fire-and-forget one like `seek`). See `mpv_command`.
#[cfg(unix)]
#[derive(Deserialize)]
struct MpvReply {
    #[serde(default)]
    event: Option<String>,
    #[serde(default)]
    data: Option<serde_json::Value>,
}

/// Sends one command to the mpv instance listening on `socket_path` (see
/// `spawn_scrub_mpv`) and returns whatever it replies with. Opens a fresh
/// connection per call rather than keeping one alive across the scrub
/// session -- simpler lifetime (nothing to reconnect if mpv restarts) at
/// the cost of a reconnect per command, which is fine for keys pressed one
/// at a time. Retries the connect briefly since the socket file may not
/// exist yet for a moment right after `spawn_scrub_mpv` returns.
#[cfg(unix)]
pub fn mpv_command(socket_path: &str, command: &[serde_json::Value]) -> Result<serde_json::Value> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    let mut stream = None;
    for _ in 0..20 {
        match UnixStream::connect(socket_path) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
    let mut stream =
        stream.context("couldn't reach mpv's IPC socket -- is the scrub session still running?")?;
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    writeln!(stream, "{}", serde_json::json!({ "command": command }))?;
    stream.flush()?;

    for line in BufReader::new(stream).lines() {
        let line = line.context("lost connection to mpv while waiting for a reply")?;
        if line.trim().is_empty() {
            continue;
        }
        let reply: MpvReply = serde_json::from_str(&line).context("couldn't parse mpv's reply")?;
        if reply.event.is_some() {
            continue; // not our command's reply -- keep reading
        }
        return Ok(reply.data.unwrap_or(serde_json::Value::Null));
    }
    bail!("mpv closed the connection without replying")
}

#[cfg(not(unix))]
pub fn mpv_command(_socket_path: &str, _command: &[serde_json::Value]) -> Result<serde_json::Value> {
    bail!("scrubbing isn't supported on this platform yet (mpv's IPC socket is Unix-only)")
}

pub fn mpv_get_time_pos(socket_path: &str) -> Result<f64> {
    mpv_command(socket_path, &[serde_json::json!("get_property"), serde_json::json!("time-pos")])?
        .as_f64()
        .context("mpv didn't report a numeric time-pos (is anything loaded?)")
}

pub fn mpv_seek_absolute(socket_path: &str, secs: f64) -> Result<()> {
    mpv_command(socket_path, &[serde_json::json!("seek"), serde_json::json!(secs), serde_json::json!("absolute")])
        .map(|_| ())
}

pub fn mpv_seek_relative(socket_path: &str, delta_secs: f64) -> Result<()> {
    mpv_command(socket_path, &[serde_json::json!("seek"), serde_json::json!(delta_secs), serde_json::json!("relative")])
        .map(|_| ())
}

pub fn mpv_frame_step(socket_path: &str, forward: bool) -> Result<()> {
    let cmd = if forward { "frame-step" } else { "frame-back-step" };
    mpv_command(socket_path, &[serde_json::json!(cmd)]).map(|_| ())
}

pub fn mpv_toggle_pause(socket_path: &str) -> Result<()> {
    mpv_command(socket_path, &[serde_json::json!("cycle"), serde_json::json!("pause")]).map(|_| ())
}

/// Spawn ffmpeg with the given arguments, streaming stdout+stderr lines
/// through `tx` as they arrive, and a final "__DONE__<code>" sentinel line.
/// Intended to run on its own thread; owns everything it needs ('static).
pub fn run_args(args: Vec<String>, tx: Sender<String>) {
    if let Err(e) = run_args_inner(&args, &tx) {
        let _ = tx.send(format!("error: {e:#}"));
        let _ = tx.send("__DONE__-1".to_string());
    }
}

fn run_args_inner(args: &[String], tx: &Sender<String>) -> Result<()> {
    let mut child = Command::new(binary("ffmpeg"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn ffmpeg (is it installed and on PATH?)")?;

    // ffmpeg writes its progress/log to stderr; stdout is normally empty for
    // `-c copy` muxing, but drain both so neither pipe ever blocks.
    let stderr = child.stderr.take().expect("piped stderr");
    let stdout = child.stdout.take().expect("piped stdout");

    let tx_err = tx.clone();
    let err_thread = std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx_err.send(line).is_err() {
                break;
            }
        }
    });
    let tx_out = tx.clone();
    let out_thread = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx_out.send(line).is_err() {
                break;
            }
        }
    });

    let status = child.wait().context("failed to wait on ffmpeg")?;
    let _ = err_thread.join();
    let _ = out_thread.join();
    let _ = tx.send(format!("__DONE__{}", status.code().unwrap_or(-1)));

    Ok(())
}

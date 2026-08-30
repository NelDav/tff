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

/// Whether the `mpv` binary can actually be run -- checked once per
/// `play_preview`/`App::start_scrub` call to decide whether mpv (the
/// preferred player: proper playback controls for preview, full IPC
/// remote control for scrub) is available at all, falling back to ffplay
/// otherwise. Actually spawns it (briefly, `--version` exits immediately)
/// rather than just checking `PATH` some other way, since that's the same
/// thing that would fail later if this said yes but was wrong.
pub fn mpv_is_installed() -> bool {
    Command::new("mpv")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Launch mpv on a rendered preview file, in its own window -- the
/// preferred player when one's installed (see `mpv_is_installed`); `play`
/// (ffplay) is the fallback otherwise. Runs detached, same as `play`:
/// nothing here waits for or streams output from the player process, since
/// it's the user who decides when they're done looking and closes the
/// window themselves. No explicit "exit when done" flag needed, unlike
/// `play`'s `-autoexit`: mpv's own default (`--keep-open=no`) already
/// closes the window once playback reaches the end of the file.
pub fn play_mpv(path: &str) -> Result<()> {
    Command::new("mpv")
        .args(["--title=tff preview", path])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn mpv (is it installed and on PATH?)")?;
    Ok(())
}

/// Launch ffplay on a rendered preview file, in its own window -- the
/// fallback when mpv isn't installed (see `play_mpv`/`mpv_is_installed`).
/// Runs detached: unlike `run_args`, nothing here waits for or streams
/// output from the player process, since it's the user who decides when
/// they're done looking and closes the window themselves.
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

/// Starts an mpv instance playing `path` from `start_secs`, with its IPC
/// server bound to `socket_path` -- lets `App::start_scrub` control it
/// (seek, frame-step, query position) while the user watches the actual
/// video. Left running detached and driven entirely through the socket
/// from then on, unlike `play`/`play_in_terminal`.
///
/// `headless` picks how it renders: `false` opens mpv's own window
/// (a display is available); `true` renders `--vo=tct` (the same
/// truecolor-block output `play_in_terminal` uses) straight into this
/// terminal instead, with `--input-terminal=no` so mpv doesn't also try to
/// read this terminal's keyboard input itself -- tff's own event loop
/// keeps that (see `main.rs`'s dedicated headless-scrub loop) and relays
/// every command over the socket instead, exactly like the windowed path,
/// so the same `scrub_*` methods serve both.
#[cfg(unix)]
pub fn spawn_scrub_mpv(path: &str, socket_path: &str, start_secs: f64, headless: bool) -> Result<std::process::Child> {
    let _ = std::fs::remove_file(socket_path); // stale socket from a crashed session, if any
    let mut args = vec![format!("--input-ipc-server={socket_path}"), format!("--start={start_secs}")];
    if headless {
        args.push("--vo=tct".to_string());
        args.push("--input-terminal=no".to_string());
    } else {
        args.push("--title=tff scrub".to_string());
    }
    args.push(path.to_string());
    Command::new("mpv")
        .args(args)
        .stdin(Stdio::null())
        .stdout(if headless { Stdio::inherit() } else { Stdio::null() })
        .stderr(Stdio::null())
        .spawn()
        .context("failed to spawn mpv (is it installed and on PATH?)")
}

/// mpv's IPC server binds a Unix domain socket -- there's no equivalent
/// this reaches for on Windows (mpv uses a named pipe there instead, a
/// different API), so scrubbing just isn't available there yet.
#[cfg(not(unix))]
pub fn spawn_scrub_mpv(
    _path: &str,
    _socket_path: &str,
    _start_secs: f64,
    _headless: bool,
) -> Result<std::process::Child> {
    bail!("scrubbing isn't supported on this platform yet (mpv's IPC socket is Unix-only)")
}

/// Starts ffplay in its own window, playing `path` from `start_secs` --
/// the fallback `App::start_scrub` uses when mpv isn't installed and a
/// display is available (there's no headless equivalent: ffplay can't
/// render at all without one, so a missing mpv with no display leaves
/// scrubbing unavailable entirely). Unlike the mpv path, there's no remote
/// control at all: ffplay has no IPC/stdin protocol of any kind, so the
/// user drives playback directly in its window using its own native keys
/// (space pause, left/right seek, `s` step one frame forward -- there's no
/// reverse single-frame step, an ffplay limitation, not tff's). Its stderr
/// is piped back (not silenced, unlike `play`) so `spawn_ffplay_position_reader`
/// can scrape the live position it prints there, which is the only channel
/// tff has into what ffplay is doing.
pub fn spawn_scrub_ffplay(path: &str, start_secs: f64) -> Result<std::process::Child> {
    Command::new(binary("ffplay"))
        .args([
            "-hide_banner".to_string(),
            "-window_title".to_string(),
            "tff scrub".to_string(),
            "-ss".to_string(),
            start_secs.to_string(),
            path.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .context("failed to spawn ffplay (is it installed and on PATH?)")
}

/// Picks the leading `<seconds>.<fraction>` off one fragment of ffplay's
/// periodic status output (e.g. `"   12.34 A-V: -0.030 fd=..."`, updated in
/// place via `\r`, not `\n` -- see `spawn_ffplay_position_reader`, which
/// splits on both before calling this). `None` for any other line ffplay
/// prints (banner/version info, the `Input #0 ...` block, etc.), which
/// don't start with a bare number.
pub(crate) fn parse_ffplay_position(fragment: &str) -> Option<f64> {
    let trimmed = fragment.trim_start();
    let end = trimmed.find(|c: char| !c.is_ascii_digit() && c != '.')?;
    if end == 0 {
        return None;
    }
    trimmed[..end].parse().ok()
}

/// Reads `stderr` on its own thread for as long as the ffplay process it
/// belongs to is alive, keeping `position` updated with the latest time
/// `parse_ffplay_position` can find -- ffplay has no way to be *asked* its
/// current position, so this passive, continuously-updated estimate (at
/// most one status interval stale) is what `App::mark_scrub_point` reads
/// instead of a live query. Exits on its own once the pipe closes (the
/// process exiting, e.g. via `ScrubSession`'s `Drop`), nothing to join.
pub fn spawn_ffplay_position_reader(
    mut stderr: std::process::ChildStderr,
    position: std::sync::Arc<std::sync::Mutex<Option<f64>>>,
) {
    use std::io::Read;
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut pending = String::new();
        while let Ok(n) = stderr.read(&mut buf) {
            if n == 0 {
                break; // EOF -- the process exited
            }
            pending.push_str(&String::from_utf8_lossy(&buf[..n]));
            while let Some(idx) = pending.find(['\r', '\n']) {
                if let Some(secs) = parse_ffplay_position(&pending[..idx])
                    && let Ok(mut guard) = position.lock()
                {
                    *guard = Some(secs);
                }
                pending.drain(..=idx);
            }
        }
    });
}

/// Best-effort: hands keyboard focus back to whatever window currently has
/// it (this terminal, if called right before a windowed `spawn_scrub_mpv`
/// or `spawn_scrub_ffplay`) a moment later, since a freshly opened player
/// window otherwise grabs focus itself -- window managers focus newly
/// mapped windows by default -- which would leave every `Mode::Scrub`
/// keypress going to the player's own bindings instead of tff's (mpv can
/// at least still be relayed commands over IPC regardless of which window
/// has focus; ffplay has no such channel, so for it this is the *only*
/// way tff's own g/i/o/Esc keys reach tff at all without a manual
/// alt-tab). Needs `xdotool` and an X11 `DISPLAY` (there's no standard
/// equivalent way to reassign focus from an arbitrary client on Wayland);
/// silently does nothing if either is missing, so the caller should still
/// tell the user to click back into the terminal themselves as a fallback
/// (see `App::start_scrub`'s log message). Runs the actual reactivation on
/// its own thread after a short delay, so tff's own event loop isn't
/// blocked waiting for the player's window to finish appearing.
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

/// A relative seek defaults to mpv's "keyframes" precision mode (fast, but
/// snaps to the nearest keyframe rather than landing exactly `delta_secs`
/// away -- verified against `man mpv`'s `seek` entry: "By default,
/// keyframes is used for relative, relative-percent, and absolute-percent
/// seeks, while exact is used for absolute seeks"). For hunting a trim
/// point, landing exactly where asked matters more than the extra
/// decoding work exact seeking costs, so this asks for `relative+exact`
/// explicitly rather than taking the imprecise default -- without it, how
/// far Shift+Left/Right actually moves depends on how far apart the
/// source's keyframes happen to be, which is exactly the "sometimes
/// doesn't seem to go back a second" inconsistency this fixes.
pub fn mpv_seek_relative(socket_path: &str, delta_secs: f64) -> Result<()> {
    mpv_command(
        socket_path,
        &[serde_json::json!("seek"), serde_json::json!(delta_secs), serde_json::json!("relative+exact")],
    )
    .map(|_| ())
}

pub fn mpv_frame_step(socket_path: &str, forward: bool) -> Result<()> {
    let cmd = if forward { "frame-step" } else { "frame-back-step" };
    mpv_command(socket_path, &[serde_json::json!(cmd)]).map(|_| ())
}

pub fn mpv_toggle_pause(socket_path: &str) -> Result<()> {
    mpv_command(socket_path, &[serde_json::json!("cycle"), serde_json::json!("pause")]).map(|_| ())
}

/// Explicitly sets mpv's pause state (as opposed to `mpv_toggle_pause`,
/// which flips whatever it currently is) -- `App::start_scrub` uses this to
/// start every mpv session paused. Verified directly against a real mpv
/// instance that this matters a great deal: with playback still running,
/// a "seek back 1s"/frame-back-step only nets whatever it manages to claw
/// back before normal forward playback erodes it again by the time the
/// next command arrives, which can leave backward navigation looking
/// almost entirely broken (repeated `-1s` seeks *creeping forward* was
/// observed in that test) even though the seek itself lands exactly right
/// every time once actually paused. Starting paused via `--pause` on the
/// command line was tried first and rejected: verified to leave the
/// player in a state where every subsequent seek silently no-ops (`time-pos`
/// never changes) -- setting the property over IPC after the process is
/// already up and running, as this does, doesn't have that problem.
pub fn mpv_set_pause(socket_path: &str, pause: bool) -> Result<()> {
    mpv_command(socket_path, &[serde_json::json!("set_property"), serde_json::json!("pause"), serde_json::json!(pause)])
        .map(|_| ())
}

/// Prepended to every spawned ffmpeg invocation, ahead of the graph's own
/// args -- not part of the "real" command (kept out of the `$ ffmpeg ...`
/// line `render.rs` logs, so a user copy-pasting that line gets the exact
/// same command tff ran, minus this purely internal aid). `-loglevel
/// level` keeps ffmpeg's default verbosity (still shows info/warning/error
/// exactly as before) but adds a `[level]` tag -- e.g. `[warning]`,
/// `[error]` -- to every line severe enough to carry one, which is the
/// only reliable way to tell a warning from an error in ffmpeg's own
/// stderr: there's no other structural marker, and matching on message
/// wording would be a losing game against ffmpeg's own message text
/// (verified live: "mmco: unref short failure" reads like a warning but
/// ffmpeg itself tags it `[error]`). See `classify_log_line`, which reads
/// this tag back out.
const LOGLEVEL_ARGS: [&str; 2] = ["-loglevel", "level"];

/// A log line's severity, as ffmpeg itself tagged it via the `[level]`
/// prefix `LOGLEVEL_ARGS` requests -- `None` for anything untagged
/// (info/verbose/debug/trace, or a line tff pushed itself, like the `$
/// ffmpeg ...` echo or `ffmpeg exited with code N`). `Fatal`/`Panic` fold
/// into `Error`: rarer, but still something a user watching the log
/// pane cares about exactly the same way.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogSeverity {
    Warning,
    Error,
}

pub fn classify_log_line(line: &str) -> Option<LogSeverity> {
    if line.contains("[warning]") {
        Some(LogSeverity::Warning)
    } else if line.contains("[error]") || line.contains("[fatal]") || line.contains("[panic]") {
        Some(LogSeverity::Error)
    } else {
        None
    }
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
        .args(LOGLEVEL_ARGS)
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

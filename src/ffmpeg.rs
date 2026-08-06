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

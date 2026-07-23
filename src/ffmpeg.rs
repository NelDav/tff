use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::graph::{StreamInfo, StreamKind};

#[derive(Deserialize)]
struct ProbeOutput {
    streams: Vec<ProbeStream>,
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

/// Run ffprobe on a file and return the streams it reports.
pub fn probe(path: &str) -> Result<Vec<StreamInfo>> {
    let output = Command::new("ffprobe")
        .args([
            "-v", "error",
            "-show_streams",
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

    if parsed.streams.is_empty() {
        bail!("ffprobe found no streams in '{path}'");
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

    Ok(streams)
}

/// Ask ffmpeg which encoders it was actually built with, so the codec picker
/// can offer the real, complete list instead of a guessed-at curated one.
/// Parses `ffmpeg -encoders` output, e.g.:
///   " V....D a64multi             Multicolor charset for Commodore 64 ..."
/// The media-type flag (V/A/S) is always the first non-whitespace character
/// of a real entry, so trimming leading whitespace before taking the fixed
/// 6-character flag block is safe here (unlike -muxers, see list_muxers).
pub fn list_encoders() -> Result<Vec<(String, StreamKind)>> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .context("failed to run ffmpeg -encoders")?;
    if !output.status.success() {
        bail!("ffmpeg -encoders exited with an error");
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut started = false;
    let mut encoders = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if !started {
            if trimmed.starts_with("---") {
                started = true;
            }
            continue;
        }
        if trimmed.len() < 8 {
            continue;
        }
        let kind = match trimmed.as_bytes()[0] {
            b'V' => StreamKind::Video,
            b'A' => StreamKind::Audio,
            b'S' => StreamKind::Subtitle,
            _ => continue,
        };
        if let Some(name) = trimmed[6..].split_whitespace().next() {
            encoders.push((name.to_string(), kind));
        }
    }
    Ok(encoders)
}

/// Ask ffmpeg which muxers (container formats) it was actually built with.
/// Parses `ffmpeg -muxers` output, e.g.:
///   "  E  3g2             3GP2 (3GPP2 file format)"
/// Unlike -encoders, the first flag column (demuxing support) is routinely
/// blank for mux-only entries, so naively trimming leading whitespace would
/// silently swallow that blank column and misalign the rest of the line.
/// The layout is fixed-width instead: 1 indent + 3 flag columns (D, E, d) +
/// 1 separator, so the muxing flag is always at byte offset 2 and the name
/// always starts at offset 5, regardless of which flags are blank.
pub fn list_muxers() -> Result<Vec<String>> {
    let output = Command::new("ffmpeg")
        .args(["-hide_banner", "-muxers"])
        .output()
        .context("failed to run ffmpeg -muxers")?;
    if !output.status.success() {
        bail!("ffmpeg -muxers exited with an error");
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let mut started = false;
    let mut muxers = Vec::new();
    for line in text.lines() {
        if !started {
            if line.trim_start().starts_with("---") {
                started = true;
            }
            continue;
        }
        let bytes = line.as_bytes();
        if bytes.len() < 6 || bytes[2] != b'E' {
            continue;
        }
        if let Some(name) = line[5..].split_whitespace().next() {
            muxers.push(name.to_string());
        }
    }
    Ok(muxers)
}

/// Launch ffplay on a rendered preview file, in its own window. Runs
/// detached: unlike `run_args`, nothing here waits for or streams output
/// from the player process, since it's the user who decides when they're
/// done looking and closes the window themselves.
pub fn play(path: &str) -> Result<()> {
    Command::new("ffplay")
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
    std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
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
    let mut child = Command::new("ffmpeg")
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

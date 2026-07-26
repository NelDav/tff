mod app_state;
mod chapters_model;
mod chapters_picker;
mod end_to_end;
mod filters_e2e;
mod graph_model;
mod node_view;
mod path_autocomplete;
mod ui_rendering;

use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::sync::mpsc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::ffmpeg;
use crate::graph::{Chapter, Codec, Endpoint, FilterName, Graph, ModifierKind, NodeId, StreamInfo, StreamKind, Target};

/// A no-modifiers key press -- what `App::text_input_handle_key` expects,
/// same as a real terminal reports for a plain keystroke. Drives the
/// simulated typing/editing throughout this file's text-input tests.
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// Any glyph `draw_wire` (see `src/ui.rs`) can render for a wire segment or
/// corner -- straight runs, a lone wire's own turn (rounded), or a real
/// junction between two different wires' cells (sharp). Row-offset
/// regression tests use this to check a wire actually attaches at a given
/// row without pinning down exactly which of these shapes it takes, since
/// that depends on the corner's direction (down-then-right vs. up-then-left,
/// etc.), not on what the test cares about.
fn is_wire_glyph(c: char) -> bool {
    "─│╭╮╰╯┌┐└┘┬┴├┤┼".contains(c)
}

fn video_stream() -> Vec<StreamInfo> {
    vec![StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None }]
}

fn video_audio_streams() -> Vec<StreamInfo> {
    vec![
        StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None },
        StreamInfo { index: 1, kind: StreamKind::Audio, codec: "aac".to_string(), lang: None },
    ]
}

fn three_streams() -> Vec<StreamInfo> {
    vec![
        StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None },
        StreamInfo { index: 1, kind: StreamKind::Audio, codec: "aac".to_string(), lang: None },
        StreamInfo { index: 2, kind: StreamKind::Subtitle, codec: "srt".to_string(), lang: None },
    ]
}

fn metadata_fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn disposition_set(flags: &[&str]) -> BTreeSet<String> {
    flags.iter().map(|f| f.to_string()).collect()
}

fn filter_fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// A `ChapterEdit` modifier's own chapter list, if `id` refers to one --
/// mirrors the private helper of the same name in `app`. Used by both the
/// bulk chapter-import cleanup tests (in `app_state`) and the chapter
/// table flow tests (in `chapters_picker`).
fn chapter_edit_chapters(graph: &Graph, id: NodeId) -> Option<&Vec<Chapter>> {
    match graph.modifier(id).map(|m| &m.kind) {
        Some(ModifierKind::ChapterEdit { chapters }) => Some(chapters),
        _ => None,
    }
}

// The following three are used by end-to-end tests spread across several
// files (chapters_model, app_state, end_to_end, filters_e2e), so they live
// here rather than in any single one of those.

fn run_ok(cmd: &mut Command) {
    let status = cmd.status().expect("failed to run ffmpeg");
    assert!(status.success(), "ffmpeg setup command failed");
}

fn run_graph_and_wait(graph: &Graph) -> Option<String> {
    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let (tx, rx) = mpsc::channel();
    ffmpeg::run_args(args, tx);
    let mut done_code = None;
    while let Ok(line) = rx.recv() {
        if let Some(code) = line.strip_prefix("__DONE__") {
            done_code = Some(code.to_string());
            break;
        }
    }
    done_code
}

/// A real video+audio file for the filter end-to-end tests below, built
/// fresh per test into its own temp dir.
fn make_test_source(dir: &std::path::Path, duration_secs: u32, width: u32, height: u32) -> std::path::PathBuf {
    let path = dir.join("source.mp4");
    run_ok(Command::new("ffmpeg").args([
        "-y", "-loglevel", "error",
        "-f", "lavfi", "-i", &format!("testsrc=duration={duration_secs}:size={width}x{height}:rate=10"),
        "-f", "lavfi", "-i", &format!("sine=frequency=440:duration={duration_secs}"),
        "-c:v", "libx264", "-c:a", "aac", "-shortest", path.to_str().unwrap(),
    ]));
    path
}

/// Sets up an isolated temp directory with known contents (two matching
/// files, one non-matching, a subdirectory, and a hidden file) so
/// path_suggestions' listing/filtering logic can be checked deterministically,
/// independent of whatever the test runner's actual cwd happens to contain.
/// Used by both the UI suggestions-popup test and the path-autocomplete
/// tests below.
fn make_suggestion_fixture() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tff-test-suggest-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("subdir")).unwrap();
    std::fs::write(dir.join("alpha.mp4"), b"").unwrap();
    std::fs::write(dir.join("alpha2.txt"), b"").unwrap();
    std::fs::write(dir.join("beta.mkv"), b"").unwrap();
    std::fs::write(dir.join(".hidden"), b"").unwrap();
    dir
}

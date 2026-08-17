use serde::{Deserialize, Serialize};

use super::NodeId;

/// One chapter marker. Ordering in a `ModifierKind::ChapterEdit` node's
/// list is append order (however the user added them), not automatically
/// sorted by time -- deliberately: `add chapter...` prefills a new
/// chapter's start from the *last* entry's end, which matches the natural
/// workflow of building a chapter list forward through the timeline
/// without needing to re-sort.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Chapter {
    pub start_secs: f64,
    pub end_secs: f64,
    pub title: String,
    /// Whether this entry was auto-imported into a `ChapterEdit` node from
    /// a connected input's own chapters, as opposed to being added
    /// directly by the user -- see `ModifierKind::ChapterEdit`'s doc
    /// comment. Meaningless outside a `ChapterEdit` node's own list: always
    /// `false` on an `InputNode`'s probed chapters, and ignored entirely
    /// by `chapters_ffmetadata` (which only reads the other three fields).
    pub imported: bool,
}

impl Chapter {
    pub fn new(start_secs: f64, end_secs: f64, title: String) -> Self {
        Chapter { start_secs, end_secs, title, imported: false }
    }

    /// Same as `new`, but flagged as auto-imported -- see the `imported`
    /// field's doc comment.
    pub fn imported(start_secs: f64, end_secs: f64, title: String) -> Self {
        Chapter { start_secs, end_secs, title, imported: true }
    }
}

/// Parses a chapter time field: either `HH:MM:SS`/`MM:SS` (colon-separated,
/// each part may have a fractional seconds component on the last one) or a
/// plain number of seconds. Returns `None` for anything that doesn't
/// unambiguously parse, rather than guessing.
pub fn parse_time(input: &str) -> Option<f64> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if s.contains(':') {
        let parts: Vec<&str> = s.split(':').collect();
        if parts.is_empty() || parts.len() > 3 || parts.iter().any(|p| p.is_empty()) {
            return None;
        }
        let mut secs = 0.0;
        for part in &parts {
            secs = secs * 60.0 + part.parse::<f64>().ok()?;
        }
        Some(secs)
    } else {
        s.parse::<f64>().ok()
    }
}

/// Renders seconds as `HH:MM:SS` (or `HH:MM:SS.mmm` if there's a
/// sub-second remainder) for display -- the inverse of `parse_time`'s
/// colon-separated form, always fully-padded so chapter rows line up.
pub fn format_time(secs: f64) -> String {
    let total_ms = (secs.max(0.0) * 1000.0).round() as i64;
    let ms = total_ms % 1000;
    let total_secs = total_ms / 1000;
    let s = total_secs % 60;
    let total_mins = total_secs / 60;
    let m = total_mins % 60;
    let h = total_mins / 60;
    if ms == 0 {
        format!("{h:02}:{m:02}:{s:02}")
    } else {
        format!("{h:02}:{m:02}:{s:02}.{ms:03}")
    }
}

/// Escapes the characters FFMETADATA treats specially in a value
/// (`=`, `;`, `#`, `\`, newline) -- verified against a real ffmpeg-exported
/// file that this is exactly the escaping it itself uses for chapter
/// titles.
fn escape_ffmetadata(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        if matches!(ch, '=' | ';' | '#' | '\\' | '\n') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Renders a chapter list as an FFMETADATA1 file's content -- ffmpeg's only
/// mechanism for setting chapters that didn't come straight from an
/// existing input file (see `Graph::resolve_chapters`'s doc comment).
/// Always writes `TIMEBASE=1/1000` (milliseconds), since this app only
/// ever holds `f64` seconds internally by the time it gets here.
pub fn chapters_ffmetadata(chapters: &[Chapter]) -> String {
    let mut out = String::from(";FFMETADATA1\n");
    for c in chapters {
        let start_ms = (c.start_secs * 1000.0).round() as i64;
        let end_ms = (c.end_secs * 1000.0).round() as i64;
        out.push_str("[CHAPTER]\n");
        out.push_str("TIMEBASE=1/1000\n");
        out.push_str(&format!("START={start_ms}\n"));
        out.push_str(&format!("END={end_ms}\n"));
        out.push_str(&format!("title={}\n", escape_ffmetadata(&c.title)));
    }
    out
}

/// Where a resolved chapter stream ultimately comes from -- see
/// `Graph::resolve_chapters`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChapterSource {
    /// Straight from a real input file's own chapters -- `-map_chapters`
    /// can point directly at this file index, no synthesized file needed.
    FromInput { input_file_index: usize },
    /// Materialized by a `ChapterEdit` modifier node -- needs a synthesized
    /// FFMETADATA temp file (see `chapters_ffmetadata`), one per distinct
    /// node even if it feeds more than one output.
    Edited { modifier_id: NodeId },
}

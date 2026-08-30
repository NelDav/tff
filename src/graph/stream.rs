use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    /// A chapter list -- not a real ffmpeg stream (no `-map file:idx` for
    /// it), but exposed as one more port on an input node so it can be
    /// wired around the graph the same way a video/audio stream is. See
    /// `InputNode::chapters` and `Graph::resolve_chapters`.
    Chapter,
    Other,
}

impl fmt::Display for StreamKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            StreamKind::Video => "v",
            StreamKind::Audio => "a",
            StreamKind::Subtitle => "s",
            StreamKind::Chapter => "c",
            StreamKind::Other => "?",
        };
        write!(f, "{s}")
    }
}

impl StreamKind {
    /// Full word, for user-facing messages where the single-letter `Display`
    /// form (used in stream labels like "v:0") would read badly.
    pub fn noun(&self) -> &'static str {
        match self {
            StreamKind::Video => "video",
            StreamKind::Audio => "audio",
            StreamKind::Subtitle => "subtitle",
            StreamKind::Chapter => "chapters",
            StreamKind::Other => "other",
        }
    }
}

/// A codec choice: either pass a stream through unchanged ("stream copy",
/// the fast/lossless default) or re-encode it with a specific ffmpeg
/// encoder name. The name is an owned `String` because the real option
/// list is discovered at runtime from `ffmpeg -encoders`
/// (see `ffmpeg::list_encoders`), not fixed at compile time.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Codec {
    Copy,
    Encode(String),
}

impl Codec {
    /// A small curated fallback list, used only if querying the local
    /// ffmpeg build for its real encoder list fails.
    pub fn curated_fallback(kind: StreamKind) -> Vec<Codec> {
        let names: &[&str] = match kind {
            StreamKind::Video => &["libx264", "libx265", "libvpx-vp9"],
            StreamKind::Audio => &["aac", "libmp3lame", "libopus", "flac"],
            StreamKind::Subtitle => &["mov_text", "srt"],
            StreamKind::Chapter | StreamKind::Other => &[],
        };
        names.iter().map(|n| Codec::Encode(n.to_string())).collect()
    }

    /// Short label for display in the UI, e.g. "libx265" -> "x265".
    pub fn label(&self) -> &str {
        match self {
            Codec::Copy => "copy",
            Codec::Encode(name) => match name.as_str() {
                "libx264" => "x264",
                "libx265" => "x265",
                "libvpx-vp9" => "vp9",
                "libmp3lame" => "mp3",
                "libopus" => "opus",
                other => other,
            },
        }
    }

    /// The actual ffmpeg encoder/codec name to pass on the command line.
    pub fn ffmpeg_name(&self) -> Option<&str> {
        match self {
            Codec::Copy => None,
            Codec::Encode(name) => Some(name),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StreamInfo {
    /// Absolute stream index within the source file, as reported by ffprobe.
    pub index: usize,
    pub kind: StreamKind,
    pub codec: String,
    pub lang: Option<String>,
    /// This stream's own duration in seconds, as reported by ffprobe --
    /// `None` if ffprobe didn't report one (some containers/codecs don't
    /// carry a per-stream duration; a chapter "stream" never has one, see
    /// `add_input`). The base case `Graph::expected_output_duration` builds
    /// every output's estimate from -- see its own doc comment for how a
    /// `Trim`/`Concat`/etc. in the chain adjusts it from there.
    ///
    /// Deliberately `#[serde(skip)]`, not saved in a project file: unlike
    /// `codec`/`lang`, which are worth preserving so a moved/missing input
    /// still shows sensible info (see `InputNode::file_missing`), a stale
    /// duration has no such use -- `Graph::from_project_file` re-probes
    /// every input fresh on load and only falls back to the saved fields
    /// when that fails, and a genuinely missing file can't be rendered
    /// (and therefore can't need a progress estimate) regardless of
    /// whether a duration was saved for it.
    #[serde(skip)]
    pub duration: Option<f64>,
}

impl StreamInfo {
    pub fn label(&self) -> String {
        // A chapter "stream" has no real per-file index or codec -- `codec`
        // just holds a plain chapter count for this case (see `add_input`).
        if self.kind == StreamKind::Chapter {
            return format!("chapters: {}", self.codec);
        }
        match &self.lang {
            Some(lang) => format!("{}:{} {} ({lang})", self.kind, self.index, self.codec),
            None => format!("{}:{} {}", self.kind, self.index, self.codec),
        }
    }
}

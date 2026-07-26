use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug)]
pub struct StreamInfo {
    /// Absolute stream index within the source file, as reported by ffprobe.
    pub index: usize,
    pub kind: StreamKind,
    pub codec: String,
    pub lang: Option<String>,
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

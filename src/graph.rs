use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub type NodeId = usize;

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

pub struct InputNode {
    pub id: NodeId,
    pub path: String,
    /// Position of this input in the `ffmpeg -i` argument list (0-based).
    pub file_index: usize,
    pub streams: Vec<StreamInfo>,
    /// Chapters found in this input, whether real embedded ones (probed
    /// from an ordinary media file's own chapter list) or the entire
    /// content of a plain FFMETADATA text file added as an input --
    /// ffprobe reports both the same way (see `ffmpeg::probe`), so this
    /// app doesn't need to tell the two apart. Non-empty exactly when
    /// `streams` has one `StreamKind::Chapter` entry (see `add_input`),
    /// which is what makes this input's chapters wireable elsewhere in the
    /// graph the same way a video/audio stream is.
    pub chapters: Vec<Chapter>,
    /// Advanced escape hatch for global *input* options not otherwise
    /// covered by the node graph (e.g. `itsoffset -> "2.5"`). Each entry is
    /// emitted as `-<key>` immediately before this input's own `-i <path>`,
    /// followed by the value as a separate token if it's non-empty -- an
    /// empty value represents a valueless switch flag (e.g. `-re`, which
    /// takes no operand at all), not "unset"; "unset" is the key being
    /// absent from the map entirely. See `input_extra_arg_keys`.
    pub extra_args: BTreeMap<String, String>,
    pub pos: (f64, f64),
    pub width: u16,
}

pub struct OutputNode {
    pub id: NodeId,
    pub path: String,
    /// Explicit muxer override (e.g. "webm", "matroska"), passed to ffmpeg
    /// as `-f <name>`. `None` means "infer from the output path's
    /// extension", ffmpeg's own default behavior.
    pub container: Option<String>,
    /// Advanced escape hatch for global *output* options not otherwise
    /// covered by the node graph (e.g. `max_interleave_delta -> "5000000"`).
    /// Same emission shape as `InputNode::extra_args` -- `-<key>` plus an
    /// optional value token -- appended after everything else this output's
    /// section builds, just before the output path. See
    /// `output_extra_arg_keys`.
    pub extra_args: BTreeMap<String, String>,
    pub pos: (f64, f64),
    pub width: u16,
}

/// One chapter marker. Ordering in a `ModifierKind::ChapterEdit` node's
/// list is append order (however the user added them), not automatically
/// sorted by time -- deliberately: `add chapter...` prefills a new
/// chapter's start from the *last* entry's end, which matches the natural
/// workflow of building a chapter list forward through the timeline
/// without needing to re-sort.
#[derive(Clone, Debug)]
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

/// Curated global *input* options offered by the extra-args picker ('e' on
/// an input node), verified via `ffmpeg -h full`: a shift for sync issues
/// (`itsoffset`), looping (`stream_loop`), and reading at native rate
/// (`re`, useful for simulating a live source). The `bool` marks a
/// valueless switch flag (no ffmpeg operand) that's toggled on pick rather
/// than prompting for a value, same idea as `disposition_flags`' checkboxes.
/// A "custom key..." escape hatch in the picker covers anything else.
pub fn input_extra_arg_keys() -> &'static [(&'static str, bool)] {
    &[("itsoffset", false), ("stream_loop", false), ("re", true)]
}

/// Curated global *output* options, same shape as `input_extra_arg_keys`.
/// `max_interleave_delta` is the option that prompted this feature;
/// `movflags`/`avoid_negative_ts`/`fflags` are common muxing-correctness
/// knobs; `shortest` (stop encoding once the shortest mapped stream ends)
/// is the curated valueless switch.
pub fn output_extra_arg_keys() -> &'static [(&'static str, bool)] {
    &[
        ("max_interleave_delta", false),
        ("movflags", false),
        ("avoid_negative_ts", false),
        ("fflags", false),
        ("shortest", true),
    ]
}

/// The stream-metadata keys offered by the metadata picker for a given
/// stream kind, passed to ffmpeg as `-metadata:s:<i> key=value`.
///
/// Kept deliberately small and kind-independent: `language`, `title`, and
/// `handler_name` were verified (round-tripped through real ffmpeg/ffprobe
/// runs against both MKV and MP4) to reliably survive as stream tags for
/// every stream kind. A commonly-cited "video-specific" key, `rotate`, was
/// tested the same way and turned out unreliable -- it showed up as a
/// plain tag on MKV but vanished entirely on MP4 once combined with
/// re-encoding, with no side_data trace either. Rather than present a
/// feature that silently doesn't work on a common container, it's left out
/// of the curated list; the picker's "custom key..." option still lets
/// anyone set it (or anything else ffmpeg accepts) explicitly.
pub fn metadata_keys_for(_kind: StreamKind) -> &'static [&'static str] {
    &["language", "title", "handler_name"]
}

/// The disposition flags offered by the picker. `ffmpeg -dispositions`
/// reports 19 in total, most of which are niche container-level bookkeeping
/// (`attached_pic`, `still_image`, `multilayer`, `timed_thumbnails`, ...).
/// This is the practically useful subset for marking a track's role: which
/// one plays by default, which subtitle track is "forced", and the common
/// accessibility/dub-language markers.
pub fn disposition_flags() -> &'static [&'static str] {
    &["default", "forced", "hearing_impaired", "visual_impaired", "original", "dub", "comment", "lyrics", "karaoke"]
}

/// A filter-based effect: unlike Convert/Metadata/Disposition (which only
/// ever emit `-c`/`-metadata`/`-disposition` stream-specifier flags), these
/// need a real `-filter_complex` graph, built in `build_output_section`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterName {
    /// Delay a track by N seconds to fix an audio/video sync offset.
    Shift,
    Volume,
    Scale,
    Crop,
    Fade,
    /// 90-degree-increment rotation, done in the pixels via `transpose`
    /// rather than a container-level `rotate` tag -- see `expression`'s doc
    /// comment for why the tag route was abandoned.
    Rotate,
}

impl FilterName {
    pub fn label(&self) -> &'static str {
        match self {
            FilterName::Shift => "shift",
            FilterName::Volume => "volume",
            FilterName::Scale => "scale",
            FilterName::Crop => "crop",
            FilterName::Fade => "fade",
            FilterName::Rotate => "rotate",
        }
    }

    /// Which stream kinds this filter is meaningful for -- gates the field
    /// picker the same way Convert's codec choices are gated by the
    /// connected stream's kind, since e.g. scaling an audio-only stream
    /// isn't a real ffmpeg operation.
    pub fn applies_to(&self, kind: StreamKind) -> bool {
        match self {
            FilterName::Shift | FilterName::Fade => matches!(kind, StreamKind::Video | StreamKind::Audio),
            FilterName::Volume => matches!(kind, StreamKind::Audio),
            FilterName::Scale | FilterName::Crop | FilterName::Rotate => matches!(kind, StreamKind::Video),
        }
    }

    /// The fields the picker offers for this filter, edited the same way as
    /// Metadata's fields (pick a field, type a value) but without a
    /// "custom key..." escape hatch -- unlike metadata tags, a filter's
    /// parameter set is fixed, not open-ended.
    pub fn fields(&self) -> &'static [&'static str] {
        match self {
            FilterName::Shift => &["seconds"],
            FilterName::Volume => &["factor"],
            FilterName::Scale => &["width", "height"],
            FilterName::Crop => &["width", "height", "x", "y"],
            FilterName::Fade => &["type", "start", "duration"],
            FilterName::Rotate => &["direction"],
        }
    }

    /// The fixed set of values a field accepts, if it's one of the
    /// enum-like ones (as opposed to a number/expression ffmpeg parses
    /// itself, like `width` or `seconds`) -- lets the picker offer a
    /// selection instead of free text for a field where anything else
    /// typed is simply wrong, not just unusual.
    pub fn value_options(&self, key: &str) -> Option<&'static [&'static str]> {
        match (self, key) {
            (FilterName::Rotate, "direction") => Some(&["90cw", "90ccw", "180"]),
            (FilterName::Fade, "type") => Some(&["in", "out"]),
            _ => None,
        }
    }

    /// Builds this filter's ffmpeg filtergraph expression (e.g.
    /// `"scale=w=1280:h=720"`) for a stream of the given kind, or `None` if
    /// nothing usable is set (an unconfigured filter node is a no-op rather
    /// than an error, same as an empty Metadata/Disposition node).
    ///
    /// Uses named parameters throughout (`w=`/`h=`, not positional) so an
    /// unset field can just be omitted and fall back to the filter's own
    /// ffmpeg-native default -- verified via `ffmpeg -h filter=<name>` for
    /// each one: `scale`'s missing dimension defaults to `-1` (preserve
    /// aspect) here rather than "iw"/"ih", since -1 is more likely to be
    /// what's wanted when only one side is set; `crop`'s x/y already default
    /// to centered when omitted.
    ///
    /// `Shift` and `Fade` need a different real filter for video vs audio
    /// (`setpts`/`fade` vs `adelay`/`afade`), picked from `kind`. This is
    /// also where a real, verified asymmetry lives: `setpts` tolerates a
    /// negative shift (frames that would land before t=0 are simply
    /// dropped), but `adelay` rejects a negative delay outright with "Delay
    /// must be non negative number" -- so audio can only be shifted later,
    /// never earlier, without trimming. Not worked around here; ffmpeg's own
    /// error surfaces in the render log same as any other bad argument.
    ///
    /// `Rotate` deliberately re-encodes pixels via `transpose` instead of
    /// setting a container `rotate` tag: this codebase already found (see
    /// `metadata_keys_for`'s doc comment) that the tag is unreliable --
    /// preserved on MKV but silently dropped on MP4 once combined with
    /// re-encoding. A pixel-level transpose has no such container
    /// dependence. 180 degrees is two 90-degree transposes chained (there's
    /// no single-step "flip" direction), verified to compose correctly.
    pub fn expression(&self, kind: StreamKind, fields: &BTreeMap<String, String>) -> Option<String> {
        let get = |k: &str| fields.get(k).map(String::as_str);
        match self {
            FilterName::Shift => {
                let seconds = get("seconds")?;
                match kind {
                    StreamKind::Audio => {
                        let ms = (seconds.trim().parse::<f64>().ok()? * 1000.0).round() as i64;
                        Some(format!("adelay=delays={ms}:all=1"))
                    }
                    _ => Some(format!("setpts=PTS+({seconds})/TB")),
                }
            }
            FilterName::Volume => Some(format!("volume=volume={}", get("factor")?)),
            FilterName::Scale => {
                if get("width").is_none() && get("height").is_none() {
                    return None;
                }
                let w = get("width").unwrap_or("-1");
                let h = get("height").unwrap_or("-1");
                Some(format!("scale=w={w}:h={h}"))
            }
            FilterName::Crop => {
                if get("width").is_none() && get("height").is_none() {
                    return None;
                }
                let mut parts = Vec::new();
                for (key, param) in [("width", "w"), ("height", "h"), ("x", "x"), ("y", "y")] {
                    if let Some(v) = get(key) {
                        parts.push(format!("{param}={v}"));
                    }
                }
                Some(format!("crop={}", parts.join(":")))
            }
            FilterName::Fade => {
                let fade_type = get("type")?;
                if fade_type != "in" && fade_type != "out" {
                    return None;
                }
                let start = get("start").unwrap_or("0");
                let duration = get("duration").unwrap_or("1");
                let filter = if kind == StreamKind::Audio { "afade" } else { "fade" };
                Some(format!("{filter}=t={fade_type}:st={start}:d={duration}"))
            }
            FilterName::Rotate => match get("direction")? {
                "90cw" => Some("transpose=dir=1".to_string()),
                "90ccw" => Some("transpose=dir=2".to_string()),
                "180" => Some("transpose=dir=1,transpose=dir=1".to_string()),
                _ => None,
            },
        }
    }
}

/// What a modifier node does to whatever stream flows through it.
#[derive(Clone, Debug)]
pub enum ModifierKind {
    Convert(Codec),
    /// Arbitrary `-metadata:s:<i> key=value` pairs, keyed by field name.
    Metadata { fields: BTreeMap<String, String> },
    /// Which of `disposition_flags()` are set on this stream, passed to
    /// ffmpeg as a single `+`-joined `-disposition:<i>` value (`0` to
    /// explicitly clear all of them).
    Disposition { flags: BTreeSet<String> },
    /// A `-filter_complex` effect; `fields` holds whatever `name.fields()`
    /// asks for, same shape/editing flow as Metadata's fields.
    Filter { name: FilterName, fields: BTreeMap<String, String> },
    /// Add/edit/remove chapters. Unlike every other kind, this doesn't
    /// decorate whatever flows through it -- its `chapters` list is fully
    /// authoritative on its own (buildable from scratch with no upstream
    /// connection at all), and if something *is* wired into its input, the
    /// only effect is offering an explicit "import from connected input"
    /// convenience action in the picker (see `Graph::resolve_chapters`,
    /// which deliberately doesn't look upstream past a node of this kind).
    ChapterEdit { chapters: Vec<Chapter> },
}

impl ModifierKind {
    pub fn short_label(&self) -> String {
        match self {
            ModifierKind::Convert(codec) => format!("convert: {}", codec.label()),
            // The fields/flags themselves are listed in the node's body
            // now, so the title just needs to name the kind.
            ModifierKind::Metadata { .. } => "metadata".to_string(),
            ModifierKind::Disposition { .. } => "disposition".to_string(),
            ModifierKind::Filter { name, .. } => name.label().to_string(),
            ModifierKind::ChapterEdit { .. } => "chapters".to_string(),
        }
    }
}

/// A node that transforms one stream in transit from an input to an
/// output: either re-encoding it (`Convert`) or attaching stream metadata
/// like language/title (`Metadata`). Sits in a chain between an input
/// stream and an output, with exactly one incoming connection (it only
/// ever transforms a single stream at a time) but any number of outgoing
/// ones, so the same converted/tagged result can feed several outputs.
pub struct ModifierNode {
    pub id: NodeId,
    pub kind: ModifierKind,
    pub pos: (f64, f64),
    pub width: u16,
}

/// The source side of a connection: either a specific stream on an input
/// file, or the (single, always-transformed) output of a modifier node.
/// Ordered so a set of these (see `App::armed`/`App::selected`) has a
/// stable iteration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Endpoint {
    Stream { node: NodeId, stream_idx: usize },
    ModifierOut(NodeId),
}

/// The destination side of a connection: a modifier's single input slot,
/// an output file's mapped-stream list (any number of incoming wires), or
/// an output's chapters slot (like `ModifierIn`, only one wire at a time --
/// see `Graph::connect`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    ModifierIn(NodeId),
    Output(NodeId),
    OutputChapters(NodeId),
}

/// A connection in the graph. Deliberately carries no settings of its own
/// -- all transformation happens in the modifier nodes along the chain a
/// wire is part of, resolved by walking backward from an output (see
/// `Graph::resolve`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wire {
    pub from: Endpoint,
    pub to: Target,
}

/// The result of walking a chain of wires/modifiers back to its ultimate
/// source stream: which stream it started from, and the effective codec,
/// metadata, disposition, and filter chain accumulated from every modifier
/// along the way. Metadata fields merge across the whole chain (they're
/// independent named slots), and filters accumulate as an ordered list
/// (each is a distinct pipeline stage, so e.g. two Scale nodes both apply,
/// in chain order) -- but codec and disposition are each an all-or-nothing
/// setting for the stream, so whichever modifier sets one first walking
/// backward -- i.e. closest to the output -- wins outright, matching how a
/// real pipeline's last stage wins.
pub struct Resolved {
    pub from_node: NodeId,
    pub from_stream_idx: usize,
    pub codec: Codec,
    pub metadata: BTreeMap<String, String>,
    pub disposition: Option<BTreeSet<String>>,
    /// In source-to-output order (the order the filters should actually be
    /// applied), even though this is built up walking backward from the
    /// output.
    pub filters: Vec<(FilterName, BTreeMap<String, String>)>,
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

/// Flattens an extra_args map into ffmpeg CLI tokens: `-<key>`, plus the
/// value as its own token when non-empty (see `InputNode::extra_args`'s doc
/// comment for why an empty value isn't just skipped).
fn extra_arg_tokens(args: &BTreeMap<String, String>) -> impl Iterator<Item = String> + '_ {
    args.iter().flat_map(|(key, value)| {
        let mut tokens = vec![format!("-{key}")];
        if !value.is_empty() {
            tokens.push(value.clone());
        }
        tokens
    })
}

pub struct Graph {
    pub inputs: Vec<InputNode>,
    pub modifiers: Vec<ModifierNode>,
    pub outputs: Vec<OutputNode>,
    pub wires: Vec<Wire>,
    next_id: NodeId,
}

impl Graph {
    pub fn new() -> Self {
        let mut graph = Graph {
            inputs: Vec::new(),
            modifiers: Vec::new(),
            outputs: Vec::new(),
            wires: Vec::new(),
            next_id: 1,
        };
        graph.add_output(); // start with one, like a typical single-file mux
        graph
    }

    /// `chapters` is whatever ffprobe reported for this file (see
    /// `ffmpeg::probe`) -- empty for a file with no chapters, or the whole
    /// content of a plain FFMETADATA text file added as an input, since
    /// ffprobe reports both the same way. When non-empty, a synthetic
    /// `StreamKind::Chapter` entry is appended to `streams` so the chapter
    /// list becomes one more wireable port on this node, exactly like a
    /// real video/audio stream.
    pub fn add_input(&mut self, path: String, mut streams: Vec<StreamInfo>, chapters: Vec<Chapter>) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        let file_index = self.inputs.len();
        if !chapters.is_empty() {
            streams.push(StreamInfo {
                index: 0, // unused for chapters -- there's no per-file stream index to map by
                kind: StreamKind::Chapter,
                codec: chapters.len().to_string(),
                lang: None,
            });
        }
        let y = 2.0 + (self.inputs.len() as f64) * 12.0;
        self.inputs.push(InputNode {
            id,
            path,
            file_index,
            streams,
            chapters,
            extra_args: BTreeMap::new(),
            pos: (2.0, y),
            width: 34,
        });
        id
    }

    pub fn add_output(&mut self) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        let idx = self.outputs.len();
        let path = if idx == 0 { "output.mkv".to_string() } else { format!("output{}.mkv", idx + 1) };
        let y = 2.0 + (idx as f64) * 12.0;
        self.outputs.push(OutputNode {
            id,
            path,
            container: None,
            extra_args: BTreeMap::new(),
            pos: (74.0, y),
            width: 30,
        });
        id
    }

    pub fn add_modifier(&mut self, kind: ModifierKind) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        let idx = self.modifiers.len();
        let y = 2.0 + (idx as f64) * 8.0;
        self.modifiers.push(ModifierNode {
            id,
            kind,
            pos: (40.0, y),
            width: 30,
        });
        id
    }

    pub fn remove_input(&mut self, id: NodeId) {
        self.inputs.retain(|n| n.id != id);
        self.wires.retain(|w| !matches!(w.from, Endpoint::Stream { node, .. } if node == id));
        // Re-derive file_index so ffmpeg -i ordering stays contiguous.
        for (idx, node) in self.inputs.iter_mut().enumerate() {
            node.file_index = idx;
        }
    }

    pub fn remove_output(&mut self, id: NodeId) {
        self.outputs.retain(|n| n.id != id);
        self.wires.retain(|w| w.to != Target::Output(id) && w.to != Target::OutputChapters(id));
    }

    pub fn remove_modifier(&mut self, id: NodeId) {
        self.modifiers.retain(|n| n.id != id);
        self.wires
            .retain(|w| w.from != Endpoint::ModifierOut(id) && w.to != Target::ModifierIn(id));
    }

    pub fn input(&self, id: NodeId) -> Option<&InputNode> {
        self.inputs.iter().find(|n| n.id == id)
    }

    pub fn input_mut(&mut self, id: NodeId) -> Option<&mut InputNode> {
        self.inputs.iter_mut().find(|n| n.id == id)
    }

    pub fn output(&self, id: NodeId) -> Option<&OutputNode> {
        self.outputs.iter().find(|n| n.id == id)
    }

    pub fn output_mut(&mut self, id: NodeId) -> Option<&mut OutputNode> {
        self.outputs.iter_mut().find(|n| n.id == id)
    }

    pub fn modifier(&self, id: NodeId) -> Option<&ModifierNode> {
        self.modifiers.iter().find(|n| n.id == id)
    }

    pub fn modifier_mut(&mut self, id: NodeId) -> Option<&mut ModifierNode> {
        self.modifiers.iter_mut().find(|n| n.id == id)
    }

    /// Wires leaving a given source, in stable order -- what a modifier's
    /// (or, conceptually, a stream port's) outgoing row list indexes into.
    pub fn outgoing(&self, from: Endpoint) -> Vec<usize> {
        self.wires.iter().enumerate().filter(|(_, w)| w.from == from).map(|(i, _)| i).collect()
    }

    /// Wires arriving at a given target, in stable order -- what an
    /// output's (or a modifier's, though that's always at most one)
    /// incoming row list indexes into.
    pub fn incoming(&self, to: Target) -> Vec<usize> {
        self.wires.iter().enumerate().filter(|(_, w)| w.to == to).map(|(i, _)| i).collect()
    }

    /// Connect (or, if it already exists, disconnect) `from` -> `to`. A
    /// modifier's input and an output's chapters slot each accept only one
    /// connection at a time, so wiring a new source into an already-fed one
    /// replaces the old one; an output's regular mapped-stream slot accepts
    /// any number, matching multi-output fan-in.
    pub fn connect(&mut self, from: Endpoint, to: Target) {
        if let Some(i) = self.wires.iter().position(|w| w.from == from && w.to == to) {
            self.wires.remove(i);
            return;
        }
        if let Target::ModifierIn(_) | Target::OutputChapters(_) = to {
            self.wires.retain(|w| w.to != to);
        }
        self.wires.push(Wire { from, to });
    }

    pub fn remove_wire_at(&mut self, wire_idx: usize) {
        if wire_idx < self.wires.len() {
            self.wires.remove(wire_idx);
        }
    }

    /// Walk backward from `from` to its ultimate source stream, threading
    /// through however many modifiers sit in between and accumulating the
    /// codec/metadata each one sets (first one encountered walking
    /// backward -- i.e. closest to the output -- wins per field). Returns
    /// `None` if the chain is broken (a modifier with nothing feeding it),
    /// forms a cycle (guarded by a bounded number of hops), or passes
    /// through a `ChapterEdit` node -- that kind never produces a
    /// resolvable media stream; see `resolve_chapters` for its own path.
    pub fn resolve(&self, from: Endpoint) -> Option<Resolved> {
        let mut codec = Codec::Copy;
        let mut metadata = BTreeMap::new();
        let mut disposition: Option<BTreeSet<String>> = None;
        let mut filters: Vec<(FilterName, BTreeMap<String, String>)> = Vec::new();
        let mut current = from;
        let mut hops = 0usize;
        loop {
            hops += 1;
            if hops > self.modifiers.len() + 1 {
                return None; // cycle guard
            }
            match current {
                Endpoint::Stream { node, stream_idx } => {
                    return Some(Resolved {
                        from_node: node,
                        from_stream_idx: stream_idx,
                        codec,
                        metadata,
                        disposition,
                        filters,
                    });
                }
                Endpoint::ModifierOut(mid) => {
                    let m = self.modifier(mid)?;
                    match &m.kind {
                        ModifierKind::Convert(c) => {
                            if matches!(codec, Codec::Copy) {
                                codec = c.clone();
                            }
                        }
                        ModifierKind::Metadata { fields } => {
                            for (k, v) in fields {
                                metadata.entry(k.clone()).or_insert_with(|| v.clone());
                            }
                        }
                        ModifierKind::Disposition { flags } => {
                            if disposition.is_none() {
                                disposition = Some(flags.clone());
                            }
                        }
                        ModifierKind::Filter { name, fields } => {
                            filters.insert(0, (*name, fields.clone()));
                        }
                        ModifierKind::ChapterEdit { .. } => return None,
                    }
                    let incoming = self.wires.iter().find(|w| w.to == Target::ModifierIn(mid))?;
                    current = incoming.from;
                }
            }
        }
    }

    /// Where an output's (or, mid-chain, a `ChapterEdit` node's) chapters
    /// ultimately come from, per `resolve_chapters`.
    pub fn output_chapters(&self, output_id: NodeId) -> Option<ChapterSource> {
        let wi = self.incoming(Target::OutputChapters(output_id)).into_iter().next()?;
        self.resolve_chapters(self.wires[wi].from)
    }

    /// Chapters, unlike video/audio, don't have an underlying stream that
    /// gets decorated -- the chapter *list itself* is the payload, so this
    /// doesn't walk a chain accumulating settings the way `resolve` does.
    /// Instead it looks at exactly one hop from `from`:
    /// - a real input's own chapter port resolves to a direct reference to
    ///   that input file (`FromInput`) -- ffmpeg can point `-map_chapters`
    ///   straight at it, no extra file needed (verified against a real
    ///   ffmpeg run: `-map_chapters` accepts any input index, not just an
    ///   FFMETADATA one);
    /// - a `ChapterEdit` modifier's output resolves to a reference to that
    ///   node (`Edited`) -- its own list is authoritative regardless of
    ///   whatever (if anything) feeds its input, so nothing further
    ///   upstream is examined;
    /// - anything else (an unconnected port, or a non-`ChapterEdit`
    ///   modifier sitting where a chapter source was expected) is `None`.
    pub fn resolve_chapters(&self, from: Endpoint) -> Option<ChapterSource> {
        match from {
            Endpoint::Stream { node, stream_idx } => {
                let input = self.input(node)?;
                let stream = input.streams.get(stream_idx)?;
                (stream.kind == StreamKind::Chapter)
                    .then_some(ChapterSource::FromInput { input_file_index: input.file_index })
            }
            Endpoint::ModifierOut(mid) => match &self.modifier(mid)?.kind {
                ModifierKind::ChapterEdit { .. } => Some(ChapterSource::Edited { modifier_id: mid }),
                _ => None,
            },
        }
    }

    /// Builds the `-map`/`-c`/`-metadata`/`-f`/path argument block for a
    /// single output node, or `None` if it has nothing resolvable to map
    /// (see `build_ffmpeg_args`). `path_override` writes somewhere other
    /// than the node's own configured path, and `extra_args` are spliced in
    /// just before the path -- both exist so `build_preview_args` can reuse
    /// this for a short, temp-file rendition of one output instead of
    /// duplicating the section-building logic.
    ///
    /// A resolved stream with a non-empty filter chain gets its own
    /// `[in]expr,expr[label]` entry pushed onto `filter_complex` (labels are
    /// unique per call, keyed off `filter_complex`'s running length -- never
    /// reused, since a real ffmpeg run rejects `-map`ping the same
    /// filtergraph output label twice, "already used elsewhere") and is
    /// `-map`ped by that label instead of by `file:stream`; a stream with no
    /// filters (or whose filters are all unconfigured no-ops) is mapped
    /// directly, exactly as before this existed.
    fn build_output_section(
        &self,
        output: &OutputNode,
        path_override: Option<&str>,
        extra_args: &[String],
        filter_complex: &mut Vec<String>,
        chapter_input: Option<usize>,
    ) -> Option<Vec<String>> {
        let resolved: Vec<Resolved> = self
            .incoming(Target::Output(output.id))
            .into_iter()
            .filter_map(|wi| self.resolve(self.wires[wi].from))
            .collect();
        if resolved.is_empty() {
            return None;
        }

        let mut args = Vec::new();
        // Per resolved stream, whether it ended up routed through the
        // filtergraph -- affects the default codec below, since a filtered
        // stream can't use stream copy (verified against real ffmpeg:
        // "Filtering and streamcopy cannot be used together", a hard error,
        // not a warning).
        let mut was_filtered = Vec::with_capacity(resolved.len());
        for r in &resolved {
            let Some(input) = self.input(r.from_node) else {
                was_filtered.push(false);
                continue;
            };
            let Some(stream) = input.streams.get(r.from_stream_idx) else {
                was_filtered.push(false);
                continue;
            };
            let source = format!("{}:{}", input.file_index, stream.index);

            let expr_parts: Vec<String> =
                r.filters.iter().filter_map(|(name, fields)| name.expression(stream.kind, fields)).collect();

            args.push("-map".to_string());
            if expr_parts.is_empty() {
                args.push(source);
                was_filtered.push(false);
            } else {
                let label = format!("f{}", filter_complex.len());
                filter_complex.push(format!("[{source}]{}[{label}]", expr_parts.join(",")));
                args.push(format!("[{label}]"));
                was_filtered.push(true);
            }
        }
        // Stream specifiers like -c:0/-metadata:s:0 are scoped to the
        // *current* output section, so the index here is local to this
        // output's own resolved list, not a position in self.wires.
        //
        // -disposition deliberately uses the bare bare `:0` form (no `s:`
        // prefix) like -c does, *not* -metadata's `:s:0` form: verified
        // against a real ffmpeg build that `-disposition:s:N` silently
        // no-ops (there, "s" is the stream-*type* letter for subtitle, so
        // "s:0" means "the first subtitle stream" -- for -metadata, by
        // contrast, "s:" is metadata's own fixed stream-vs-chapter-vs-
        // program marker, not a type selector, so absolute indices work
        // fine there). The bare numeric form is the one that means
        // "absolute output stream index" for both -c and -disposition.
        for (local_i, (r, &filtered)) in resolved.iter().zip(&was_filtered).enumerate() {
            match r.codec.ffmpeg_name() {
                Some(name) => {
                    args.push(format!("-c:{local_i}"));
                    args.push(name.to_string());
                }
                // No explicit codec chosen: a plain stream defaults to
                // copy same as always, but a filtered one can't be copied
                // (its bytes no longer match the source), so it's left
                // unset instead -- ffmpeg then picks its own default
                // encoder for the target container, exactly as if this
                // stream had no -c:i at all.
                None if !filtered => {
                    args.push(format!("-c:{local_i}"));
                    args.push("copy".to_string());
                }
                None => {}
            }
            for (key, value) in &r.metadata {
                args.push(format!("-metadata:s:{local_i}"));
                args.push(format!("{key}={value}"));
            }
            if let Some(flags) = &r.disposition {
                args.push(format!("-disposition:{local_i}"));
                args.push(if flags.is_empty() {
                    "0".to_string()
                } else {
                    flags.iter().cloned().collect::<Vec<_>>().join("+")
                });
            }
        }
        if let Some(container) = &output.container {
            args.push("-f".to_string());
            args.push(container.clone());
        }
        if let Some(idx) = chapter_input {
            args.push("-map_chapters".to_string());
            args.push(idx.to_string());
        }
        args.extend(extra_arg_tokens(&output.extra_args));
        args.extend_from_slice(extra_args);
        args.push(path_override.unwrap_or(&output.path).to_string());
        Some(args)
    }

    /// Build the `ffmpeg` argument list for the current graph: all inputs
    /// up front, then one output "section" per output node that has at
    /// least one resolvable connection -- mirroring ffmpeg's own
    /// multi-output command syntax. Outputs with nothing resolvable are
    /// skipped entirely: handing ffmpeg an output path with no `-map` would
    /// trigger its default stream auto-selection, which isn't what an empty
    /// output node means here.
    ///
    /// `chapter_files` maps a `ChapterEdit` modifier's `NodeId` to the path
    /// of an already-written FFMETADATA file holding its chapters (see
    /// `chapters_ffmetadata`) -- `Graph` stays free of file I/O itself, so
    /// the caller (`App`) writes these first and passes the paths in.
    /// Resolved via `output_chapters`: an output whose chapters trace
    /// straight back to a real input file needs no entry here at all
    /// (`-map_chapters` just points at that input directly); one whose
    /// chapters were materialized by a `ChapterEdit` node gets that node's
    /// temp file appended as an extra `-i` after the real inputs (never
    /// disturbing their own `file_index`-based `-map` references) -- once
    /// per distinct node, even if it feeds more than one output. An output
    /// needing a `ChapterEdit` node's file with no entry here (e.g. the
    /// write failed) simply renders without chapters rather than erroring.
    pub fn build_ffmpeg_args(&self, chapter_files: &BTreeMap<NodeId, String>) -> Vec<String> {
        let mut args = vec!["-y".to_string()];
        for input in &self.inputs {
            args.extend(extra_arg_tokens(&input.extra_args));
            args.push("-i".to_string());
            args.push(input.path.clone());
        }
        let chapter_sources: BTreeMap<NodeId, ChapterSource> =
            self.outputs.iter().filter_map(|o| self.output_chapters(o.id).map(|src| (o.id, src))).collect();
        let mut next_input_index = self.inputs.len();
        let mut chapter_modifier_index: BTreeMap<NodeId, usize> = BTreeMap::new();
        for source in chapter_sources.values() {
            if let ChapterSource::Edited { modifier_id } = source
                && !chapter_modifier_index.contains_key(modifier_id)
                && let Some(path) = chapter_files.get(modifier_id)
            {
                args.push("-i".to_string());
                args.push(path.clone());
                chapter_modifier_index.insert(*modifier_id, next_input_index);
                next_input_index += 1;
            }
        }
        let mut filter_complex = Vec::new();
        let mut sections = Vec::new();
        for output in &self.outputs {
            let chapter_input = match chapter_sources.get(&output.id) {
                Some(ChapterSource::FromInput { input_file_index }) => Some(*input_file_index),
                Some(ChapterSource::Edited { modifier_id }) => chapter_modifier_index.get(modifier_id).copied(),
                None => None,
            };
            if let Some(section) = self.build_output_section(output, None, &[], &mut filter_complex, chapter_input) {
                sections.push(section);
            }
        }
        // -filter_complex is a single global graph (not one per output), so
        // it has to land once, before any -map references one of its
        // labels -- everything upstream of this point is still valid
        // regardless of which/how many outputs actually use a filter.
        if !filter_complex.is_empty() {
            args.push("-filter_complex".to_string());
            args.push(filter_complex.join(";"));
        }
        for section in sections {
            args.extend(section);
        }
        args
    }

    /// Args for rendering just one output to `preview_path`, capped to its
    /// first `duration_secs` seconds (as an output-scoped `-t`, so it stops
    /// that output early without truncating what's read from the inputs) --
    /// `None` if that output has nothing resolvable, same as a real render.
    /// `chapter_files` is the same map `build_ffmpeg_args` takes, keyed the
    /// same way even though only `output_id`'s entry (if any) is relevant.
    pub fn build_preview_args(
        &self,
        output_id: NodeId,
        preview_path: &str,
        duration_secs: u32,
        chapter_files: &BTreeMap<NodeId, String>,
    ) -> Option<Vec<String>> {
        let output = self.outputs.iter().find(|o| o.id == output_id)?;
        let mut args = vec!["-y".to_string()];
        for input in &self.inputs {
            args.extend(extra_arg_tokens(&input.extra_args));
            args.push("-i".to_string());
            args.push(input.path.clone());
        }
        let chapter_input = match self.output_chapters(output.id) {
            Some(ChapterSource::FromInput { input_file_index }) => Some(input_file_index),
            Some(ChapterSource::Edited { modifier_id }) => chapter_files.get(&modifier_id).map(|path| {
                let idx = self.inputs.len();
                args.push("-i".to_string());
                args.push(path.clone());
                idx
            }),
            None => None,
        };
        let mut filter_complex = Vec::new();
        let extra = vec!["-t".to_string(), duration_secs.to_string()];
        let section = self.build_output_section(output, Some(preview_path), &extra, &mut filter_complex, chapter_input)?;
        if !filter_complex.is_empty() {
            args.push("-filter_complex".to_string());
            args.push(filter_complex.join(";"));
        }
        args.extend(section);
        Some(args)
    }
}

use std::collections::{BTreeMap, BTreeSet};

use super::chapter::Chapter;
use super::stream::{Codec, StreamInfo, StreamKind};
use super::NodeId;

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
    /// Cut the stream down to `[start, end]` (either end optional --
    /// omitted `start` keeps the beginning, omitted `end` keeps the rest),
    /// with timestamps reset back to zero afterward. See `expression`'s
    /// doc comment for why that reset is required.
    Trim,
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
            FilterName::Trim => "trim",
        }
    }

    /// Which stream kinds this filter is meaningful for -- gates the field
    /// picker the same way Convert's codec choices are gated by the
    /// connected stream's kind, since e.g. scaling an audio-only stream
    /// isn't a real ffmpeg operation.
    pub fn applies_to(&self, kind: StreamKind) -> bool {
        match self {
            FilterName::Shift | FilterName::Fade | FilterName::Trim => {
                matches!(kind, StreamKind::Video | StreamKind::Audio)
            }
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
            FilterName::Trim => &["start", "end"],
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
    ///
    /// `Trim` chains a `setpts`/`asetpts` reset after `trim`/`atrim`:
    /// `trim` keeps the kept segment's *original* timestamps (so a clip
    /// starting at `start=10` still carries PTS values starting around 10s
    /// rather than 0), which downstream muxing/sync treats as a 10-second
    /// gap at the front of the output instead of an actual cut -- verified
    /// against a real ffmpeg run. Resetting PTS to start at zero is the
    /// documented fix (ffmpeg's own `trim` filter docs call this out
    /// explicitly).
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
            FilterName::Trim => {
                if get("start").is_none() && get("end").is_none() {
                    return None;
                }
                let mut parts = Vec::new();
                if let Some(start) = get("start") {
                    parts.push(format!("start={start}"));
                }
                if let Some(end) = get("end") {
                    parts.push(format!("end={end}"));
                }
                if kind == StreamKind::Audio {
                    Some(format!("atrim={},asetpts=PTS-STARTPTS", parts.join(":")))
                } else {
                    Some(format!("trim={},setpts=PTS-STARTPTS", parts.join(":")))
                }
            }
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

    /// Whether a stream of `kind` is something this modifier can
    /// meaningfully sit downstream of, checked before wiring one in so a
    /// mismatched connection is rejected up front rather than accepted
    /// silently and only complained about later, when the node is edited
    /// (`Filter`) or simply never doing anything useful with it
    /// (`ChapterEdit`, whose import logic just ignores a non-chapter
    /// source). `Convert`/`Metadata`/`Disposition` apply to any stream
    /// kind, so they never reject a connection here.
    pub fn accepts_stream_kind(&self, kind: StreamKind) -> bool {
        match self {
            ModifierKind::Convert(_) | ModifierKind::Metadata { .. } | ModifierKind::Disposition { .. } => true,
            ModifierKind::Filter { name, .. } => name.applies_to(kind),
            ModifierKind::ChapterEdit { .. } => kind == StreamKind::Chapter,
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

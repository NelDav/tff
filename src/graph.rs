use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub type NodeId = usize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamKind {
    Video,
    Audio,
    Subtitle,
    Other,
}

impl fmt::Display for StreamKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            StreamKind::Video => "v",
            StreamKind::Audio => "a",
            StreamKind::Subtitle => "s",
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
            StreamKind::Other => &[],
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
    pub pos: (f64, f64),
    pub width: u16,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    Stream { node: NodeId, stream_idx: usize },
    ModifierOut(NodeId),
}

/// The destination side of a connection: a modifier's single input slot,
/// or an output file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    ModifierIn(NodeId),
    Output(NodeId),
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

    pub fn add_input(&mut self, path: String, streams: Vec<StreamInfo>) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        let file_index = self.inputs.len();
        let y = 2.0 + (self.inputs.len() as f64) * 12.0;
        self.inputs.push(InputNode {
            id,
            path,
            file_index,
            streams,
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
        self.wires.retain(|w| w.to != Target::Output(id));
    }

    pub fn remove_modifier(&mut self, id: NodeId) {
        self.modifiers.retain(|n| n.id != id);
        self.wires
            .retain(|w| w.from != Endpoint::ModifierOut(id) && w.to != Target::ModifierIn(id));
    }

    pub fn input(&self, id: NodeId) -> Option<&InputNode> {
        self.inputs.iter().find(|n| n.id == id)
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
    /// modifier's input accepts only one connection at a time, so wiring a
    /// new source into an already-fed `ModifierIn` replaces the old one; an
    /// output accepts any number, matching multi-output fan-in.
    pub fn connect(&mut self, from: Endpoint, to: Target) {
        if let Some(i) = self.wires.iter().position(|w| w.from == from && w.to == to) {
            self.wires.remove(i);
            return;
        }
        if let Target::ModifierIn(_) = to {
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
    /// `None` if the chain is broken (a modifier with nothing feeding it)
    /// or forms a cycle (guarded by a bounded number of hops).
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
                    }
                    let incoming = self.wires.iter().find(|w| w.to == Target::ModifierIn(mid))?;
                    current = incoming.from;
                }
            }
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
    pub fn build_ffmpeg_args(&self) -> Vec<String> {
        let mut args = vec!["-y".to_string()];
        for input in &self.inputs {
            args.push("-i".to_string());
            args.push(input.path.clone());
        }
        let mut filter_complex = Vec::new();
        let mut sections = Vec::new();
        for output in &self.outputs {
            if let Some(section) = self.build_output_section(output, None, &[], &mut filter_complex) {
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
    pub fn build_preview_args(&self, output_id: NodeId, preview_path: &str, duration_secs: u32) -> Option<Vec<String>> {
        let output = self.outputs.iter().find(|o| o.id == output_id)?;
        let mut args = vec!["-y".to_string()];
        for input in &self.inputs {
            args.push("-i".to_string());
            args.push(input.path.clone());
        }
        let mut filter_complex = Vec::new();
        let extra = vec!["-t".to_string(), duration_secs.to_string()];
        let section = self.build_output_section(output, Some(preview_path), &extra, &mut filter_complex)?;
        if !filter_complex.is_empty() {
            args.push("-filter_complex".to_string());
            args.push(filter_complex.join(";"));
        }
        args.extend(section);
        Some(args)
    }
}

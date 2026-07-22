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
}

impl ModifierKind {
    pub fn short_label(&self) -> String {
        match self {
            ModifierKind::Convert(codec) => format!("convert: {}", codec.label()),
            // The fields/flags themselves are listed in the node's body
            // now, so the title just needs to name the kind.
            ModifierKind::Metadata { .. } => "metadata".to_string(),
            ModifierKind::Disposition { .. } => "disposition".to_string(),
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
/// metadata, and disposition accumulated from every modifier along the way.
/// Metadata fields merge across the whole chain (they're independent named
/// slots), but codec and disposition are each an all-or-nothing setting for
/// the stream, so whichever modifier sets one first walking backward --
/// i.e. closest to the output -- wins outright, matching how a real
/// pipeline's last stage wins.
pub struct Resolved {
    pub from_node: NodeId,
    pub from_stream_idx: usize,
    pub codec: Codec,
    pub metadata: BTreeMap<String, String>,
    pub disposition: Option<BTreeSet<String>>,
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
    fn build_output_section(
        &self,
        output: &OutputNode,
        path_override: Option<&str>,
        extra_args: &[String],
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
        for r in &resolved {
            if let Some(input) = self.input(r.from_node)
                && let Some(stream) = input.streams.get(r.from_stream_idx) {
                    args.push("-map".to_string());
                    args.push(format!("{}:{}", input.file_index, stream.index));
                }
        }
        args.push("-c".to_string());
        args.push("copy".to_string());
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
        for (local_i, r) in resolved.iter().enumerate() {
            if let Some(name) = r.codec.ffmpeg_name() {
                args.push(format!("-c:{local_i}"));
                args.push(name.to_string());
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
        for output in &self.outputs {
            if let Some(section) = self.build_output_section(output, None, &[]) {
                args.extend(section);
            }
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
        let extra = vec!["-t".to_string(), duration_secs.to_string()];
        args.extend(self.build_output_section(output, Some(preview_path), &extra)?);
        Some(args)
    }
}

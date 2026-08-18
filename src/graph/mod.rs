mod chapter;
mod node;
mod project;
mod stream;
mod wire;

pub use chapter::{chapters_ffmetadata, format_time, parse_time, Chapter, ChapterSource};
pub use node::{
    disposition_flags, input_extra_arg_keys, metadata_keys_for, output_extra_arg_keys, FilterName, InputNode,
    ModifierKind, ModifierNode, OutputNode,
};
pub use project::{ProjectFile, SavedInput, PROJECT_FORMAT_VERSION};
pub use stream::{Codec, StreamInfo, StreamKind};
pub use wire::{Endpoint, Resolved, Target, Wire};

use std::collections::{BTreeMap, BTreeSet};

pub type NodeId = usize;

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

/// A defensive cushion added past a Trim segment's own `end` when bounding
/// its dedicated `-i` with `-to` (see `SeekInputs`) -- covers any rounding
/// slack in ffmpeg's own accurate-seek/stop handling so the `trim`/`atrim`
/// filter downstream, which still does the real frame-exact cut, never
/// risks being hand a source that's one frame short of what it asked for.
const SEEK_INPUT_END_MARGIN_SECS: f64 = 2.0;

/// `end` plus `SEEK_INPUT_END_MARGIN_SECS`, or `end` unchanged if it isn't
/// plain decimal seconds (e.g. `HH:MM:SS`, which the Trim filter's fields
/// also accept, being ffmpeg's own duration syntax) -- `-to` takes either
/// form, but only the numeric one can have a margin added here.
fn padded_seek_end(end: &str) -> String {
    match end.trim().parse::<f64>() {
        Ok(secs) => (secs + SEEK_INPUT_END_MARGIN_SECS).to_string(),
        Err(_) => end.to_string(),
    }
}

/// Accumulates extra `-i` occurrences opened only for a single Trim
/// segment's own `[start, end]` window, instead of every trimmed stream
/// sharing one input's single whole-file `-i` (see `resolve_source`'s use
/// of this). That sharing is what a real render was found to OOM on: with
/// several widely-spaced Trim segments of one long file all feeding a
/// `Concat`, ffmpeg has to decode the *entire* file in one linear pass
/// (there's only one `-i` to read from) and buffer every not-yet-consumed
/// later segment's raw frames in memory until `concat` finishes draining
/// the earlier ones in order -- for a multi-hour file that's far more RAM
/// than a typical machine has. Giving each segment its own `-i`, seeked
/// (`-ss`) and bounded (`-to`) to just its own window, means ffmpeg only
/// ever has to hold that one segment's frames at a time.
///
/// `-copyts` on each of these keeps the original absolute timestamps
/// intact through the seek (verified against `man ffmpeg`: every option
/// used here is per-file, resetting between `-i`s, so this can't leak into
/// any other input) -- the `trim`/`atrim` filter's `start=`/`end=` values
/// are absolute source timestamps, and still does the real, frame-exact
/// cut exactly as it did with a shared, unseeked input; this is purely an
/// optimization for how much dead time ffmpeg has to decode-and-discard
/// (and, critically, buffer) to get there.
///
/// Deduped by `(input node id, start, end)` so two streams trimmed to the
/// identical window (e.g. a video and its audio track cut to the same
/// real-time range, the common case) share one dedicated input rather than
/// each opening their own.
struct SeekInputs {
    next_index: usize,
    by_key: BTreeMap<(NodeId, String, String), usize>,
    args: Vec<String>,
}

impl SeekInputs {
    fn new(next_index: usize) -> Self {
        SeekInputs { next_index, by_key: BTreeMap::new(), args: Vec::new() }
    }

    /// Returns the file index of a dedicated `-i` seeked/bounded to
    /// `[start, end]` of `input`'s file, allocating a new one on first use
    /// for this exact key.
    fn get_or_insert(&mut self, input: &InputNode, start: Option<&str>, end: Option<&str>) -> usize {
        let key = (input.id, start.unwrap_or("").to_string(), end.unwrap_or("").to_string());
        if let Some(&idx) = self.by_key.get(&key) {
            return idx;
        }
        let idx = self.next_index;
        self.next_index += 1;
        self.args.extend(extra_arg_tokens(&input.extra_args));
        if let Some(s) = start {
            self.args.push("-ss".to_string());
            self.args.push(s.to_string());
        }
        if let Some(e) = end {
            self.args.push("-to".to_string());
            self.args.push(padded_seek_end(e));
        }
        self.args.push("-copyts".to_string());
        self.args.push("-i".to_string());
        self.args.push(input.path.clone());
        self.by_key.insert(key, idx);
        idx
    }
}

/// Appends the synthetic `StreamKind::Chapter` entry `add_input`'s own doc
/// comment describes (when `chapters` is non-empty) to a *raw* probed/saved
/// stream list -- shared by `add_input` and `Graph::from_project_file` so a
/// project file only ever needs to carry the raw list (see
/// `project::SavedInput`), not a second, easy-to-desync copy of this
/// derivation.
fn streams_with_chapter_marker(mut streams: Vec<StreamInfo>, chapters: &[Chapter]) -> Vec<StreamInfo> {
    if !chapters.is_empty() {
        streams.push(StreamInfo {
            index: 0, // unused for chapters -- there's no per-file stream index to map by
            kind: StreamKind::Chapter,
            codec: chapters.len().to_string(),
            lang: None,
        });
    }
    streams
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
    pub fn add_input(&mut self, path: String, streams: Vec<StreamInfo>, chapters: Vec<Chapter>) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        let file_index = self.inputs.len();
        let streams = streams_with_chapter_marker(streams, &chapters);
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
            file_missing: false,
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
    /// replaces the old one -- except a `ModifierKind::Concat` node's input,
    /// which (like an output's regular mapped-stream slot) accepts any
    /// number, appended in the order they're wired.
    pub fn connect(&mut self, from: Endpoint, to: Target) {
        if let Some(i) = self.wires.iter().position(|w| w.from == from && w.to == to) {
            self.wires.remove(i);
            return;
        }
        let replaces_existing = match to {
            Target::ModifierIn(mid) => !matches!(self.modifier(mid).map(|m| &m.kind), Some(ModifierKind::Concat)),
            Target::OutputChapters(_) => true,
            Target::Output(_) => false,
        };
        if replaces_existing {
            self.wires.retain(|w| w.to != to);
        }
        self.wires.push(Wire { from, to });
    }

    pub fn remove_wire_at(&mut self, wire_idx: usize) {
        if wire_idx < self.wires.len() {
            self.wires.remove(wire_idx);
        }
    }

    /// Swaps two wires' positions in `self.wires`. `incoming`/`outgoing`
    /// report wires in `self.wires` order, and for an output's
    /// mapped-stream slot that order is what ffmpeg's `-map` sequence (and
    /// so the stream order in the muxed container) follows -- so this is
    /// how a caller reorders an output's streams without touching any
    /// wire's own `from`/`to`.
    pub fn swap_wires(&mut self, a: usize, b: usize) {
        self.wires.swap(a, b);
    }

    /// Walk backward from `from` to its ultimate source(s), threading
    /// through however many modifiers sit in between and accumulating the
    /// codec/metadata each one sets (first one encountered walking
    /// backward -- i.e. closest to the output -- wins per field). Returns
    /// `None` if the chain is broken (a modifier with nothing feeding it),
    /// forms a cycle (guarded by `resolve_hopped`'s shared hop budget), or
    /// passes through a `ChapterEdit` node -- that kind never produces a
    /// resolvable media stream; see `resolve_chapters` for its own path.
    ///
    /// Hitting a `ModifierKind::Concat` node ends the linear walk: each of
    /// its incoming wires is resolved independently as its own segment
    /// (recursing back into this same walk), and all of them must resolve
    /// to the same `StreamKind` -- a mix, or no segments at all, is a
    /// broken chain like any other.
    pub fn resolve(&self, from: Endpoint) -> Option<Resolved> {
        let mut hops = 0usize;
        self.resolve_hopped(from, &mut hops)
    }

    /// `hops` is one counter shared across every branch of every `Concat`
    /// node's segments (not reset per branch/recursive call), so a cycle
    /// threaded through more than one segment still can't recurse forever.
    /// The cap is far above anything a real graph would ever need to walk
    /// -- it exists purely to guarantee termination on a cyclic one.
    fn resolve_hopped(&self, from: Endpoint, hops: &mut usize) -> Option<Resolved> {
        const MAX_HOPS: usize = 10_000;
        let mut codec = Codec::Copy;
        let mut metadata = BTreeMap::new();
        let mut disposition: Option<BTreeSet<String>> = None;
        let mut filters: Vec<(FilterName, BTreeMap<String, String>)> = Vec::new();
        let mut current = from;
        loop {
            *hops += 1;
            if *hops > MAX_HOPS {
                return None; // cycle guard
            }
            match current {
                Endpoint::Stream { node, stream_idx } => {
                    return Some(Resolved::Stream {
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
                        ModifierKind::Concat => {
                            let segment_wires = self.incoming(Target::ModifierIn(mid));
                            if segment_wires.is_empty() {
                                return None;
                            }
                            let segments: Vec<Resolved> = segment_wires
                                .into_iter()
                                .map(|wi| self.resolve_hopped(self.wires[wi].from, hops))
                                .collect::<Option<_>>()?;
                            let mut kinds = segments.iter().map(|s| self.resolved_stream_kind(s));
                            let kind = kinds.next().flatten()?;
                            if !kinds.all(|k| k == Some(kind)) {
                                return None;
                            }
                            return Some(Resolved::Concat { kind, segments, codec, metadata, disposition, filters });
                        }
                    }
                    let incoming = self.wires.iter().find(|w| w.to == Target::ModifierIn(mid))?;
                    current = incoming.from;
                }
            }
        }
    }

    /// The stream kind a resolved chain ultimately carries -- looked up
    /// from the real input stream for `Resolved::Stream`, or the shared
    /// kind recorded when a `Concat` node's segments were validated to
    /// match (see `resolve`) for `Resolved::Concat`.
    pub fn resolved_stream_kind(&self, r: &Resolved) -> Option<StreamKind> {
        match r {
            Resolved::Stream { from_node, from_stream_idx, .. } => {
                self.input(*from_node)?.streams.get(*from_stream_idx).map(|s| s.kind)
            }
            Resolved::Concat { kind, .. } => Some(*kind),
        }
    }

    /// A resolved chain's own descriptive label, and -- for a real stream
    /// -- the "[file_index] path" it came from (`None` for a `Concat`,
    /// which has no single source file). Split apart rather than returned
    /// as one combined string so a caller can splice a codec/metadata tag
    /// in between the two, the way the output-node display does.
    pub fn resolved_label_and_source(&self, r: &Resolved) -> (String, Option<String>) {
        match r {
            Resolved::Stream { from_node, from_stream_idx, .. } => {
                match self.input(*from_node).and_then(|n| n.streams.get(*from_stream_idx).map(|s| (n, s))) {
                    Some((n, s)) => (
                        s.label(),
                        Some(format!("[{}] {}", n.file_index, n.path.rsplit('/').next().unwrap_or(&n.path))),
                    ),
                    None => ("(dangling)".to_string(), None),
                }
            }
            Resolved::Concat { kind, segments, .. } => {
                (format!("concat[{}] of {} segments", kind.noun(), segments.len()), None)
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

    /// Resolves `r` down to a single filtergraph-referenceable label --
    /// `[file:stream]` for a plain demuxed stream (valid filter_complex
    /// input syntax with no entry of its own needed), `[fN]` for one that
    /// needed its own filter_complex entry (a filtered stream, or any
    /// `Concat`, which always routes through the filtergraph) -- the stream
    /// kind it carries, and whether it actually needed a filter_complex
    /// entry at all (a plain demuxed stream whose filters are all
    /// unconfigured no-ops doesn't; a caller building a top-level `-map`
    /// needs to know this, since `-map [file:stream]` -- unlike
    /// `-map file:stream` -- is invalid: `[...]` only means anything to
    /// `-map` when it's a label some filter_complex entry actually
    /// declared). Pushes whatever filter_complex entries are needed along
    /// the way. A `Concat`'s segments are each resolved the same way first,
    /// then joined by one more `concat=n=N:v=X:a=Y` entry (`v`/`a` always
    /// `1`/`0` or `0`/`1` for now -- v1 only joins segments that already
    /// share a single kind, see `resolve`), after which any of the
    /// `Concat` node's *own* filters (applied by a modifier downstream of
    /// it) are chained on exactly like a plain stream's -- a `Concat` is
    /// always reported as needing a filter_complex entry, since ffmpeg's
    /// `concat` filter has no other way to run. Labels are always unique
    /// (`build_output_section`'s doc comment explains why).
    ///
    /// When a `Resolved::Stream`'s *entire* filter chain is a single Trim
    /// with a configured `start`/`end` -- the common case, and the one a
    /// `Concat` node's segments always are -- the source it's read from is
    /// a dedicated, seeked `-i` (see `SeekInputs`) instead of the input
    /// node's own shared whole-file one, so ffmpeg doesn't have to decode
    /// and buffer the entire file to get there. Anything more complex (a
    /// Trim stacked with another filter, or a filter chain that isn't Trim
    /// at all) keeps using the shared `-i` unchanged -- always correct,
    /// just without this particular optimization.
    fn resolve_source(
        &self,
        r: &Resolved,
        filter_complex: &mut Vec<String>,
        seek_inputs: &mut SeekInputs,
    ) -> Option<(String, StreamKind, bool)> {
        match r {
            Resolved::Stream { from_node, from_stream_idx, filters, .. } => {
                let input = self.input(*from_node)?;
                let stream = input.streams.get(*from_stream_idx)?;
                let expr_parts: Vec<String> =
                    filters.iter().filter_map(|(name, fields)| name.expression(stream.kind, fields)).collect();
                let file_index = match filters.as_slice() {
                    [(FilterName::Trim, fields)] if !expr_parts.is_empty() => {
                        let start = fields.get("start").map(String::as_str);
                        let end = fields.get("end").map(String::as_str);
                        seek_inputs.get_or_insert(input, start, end)
                    }
                    _ => input.file_index,
                };
                let source = format!("{file_index}:{}", stream.index);
                if expr_parts.is_empty() {
                    Some((format!("[{source}]"), stream.kind, false))
                } else {
                    let label = format!("f{}", filter_complex.len());
                    filter_complex.push(format!("[{source}]{}[{label}]", expr_parts.join(",")));
                    Some((format!("[{label}]"), stream.kind, true))
                }
            }
            Resolved::Concat { kind, segments, filters, .. } => {
                let (v, a) = match kind {
                    StreamKind::Video => (1, 0),
                    StreamKind::Audio => (0, 1),
                    StreamKind::Subtitle | StreamKind::Chapter | StreamKind::Other => return None,
                };
                let mut refs = String::new();
                for seg in segments {
                    let (label, ..) = self.resolve_source(seg, filter_complex, seek_inputs)?;
                    refs.push_str(&label);
                }
                let concat_label = format!("f{}", filter_complex.len());
                filter_complex.push(format!("{refs}concat=n={}:v={v}:a={a}[{concat_label}]", segments.len()));
                let expr_parts: Vec<String> =
                    filters.iter().filter_map(|(name, fields)| name.expression(*kind, fields)).collect();
                if expr_parts.is_empty() {
                    Some((format!("[{concat_label}]"), *kind, true))
                } else {
                    let label = format!("f{}", filter_complex.len());
                    filter_complex.push(format!("[{concat_label}]{}[{label}]", expr_parts.join(",")));
                    Some((format!("[{label}]"), *kind, true))
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
        chapter_input: Option<usize>,
        seek_inputs: &mut SeekInputs,
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
        // not a warning). A `Resolved::Concat` is always routed through the
        // filtergraph (ffmpeg's `concat` filter has no stream-copy mode),
        // so it always takes the `resolve_source` path below, same as a
        // `Resolved::Stream` with a non-empty filter chain.
        let mut was_filtered = Vec::with_capacity(resolved.len());
        for r in &resolved {
            let Some((label, _kind, filtered)) = self.resolve_source(r, filter_complex, seek_inputs) else {
                was_filtered.push(false);
                continue;
            };
            args.push("-map".to_string());
            args.push(if filtered {
                label
            } else {
                // A trivial passthrough's label is always `[file:stream]`
                // (see `resolve_source`'s doc comment) -- strip the
                // brackets back off, since `-map` only accepts that syntax
                // for a label some filter_complex entry actually declared,
                // and this one has none.
                label[1..label.len() - 1].to_string()
            });
            was_filtered.push(filtered);
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
            match r.codec().ffmpeg_name() {
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
            for (key, value) in r.metadata() {
                args.push(format!("-metadata:s:{local_i}"));
                args.push(format!("{key}={value}"));
            }
            if let Some(flags) = r.disposition() {
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
        let mut seek_inputs = SeekInputs::new(next_input_index);
        let mut sections = Vec::new();
        for output in &self.outputs {
            let chapter_input = match chapter_sources.get(&output.id) {
                Some(ChapterSource::FromInput { input_file_index }) => Some(*input_file_index),
                Some(ChapterSource::Edited { modifier_id }) => chapter_modifier_index.get(modifier_id).copied(),
                None => None,
            };
            if let Some(section) =
                self.build_output_section(output, None, &[], &mut filter_complex, chapter_input, &mut seek_inputs)
            {
                sections.push(section);
            }
        }
        // Dedicated Trim-segment seek inputs (see `SeekInputs`) are more
        // `-i`s, so -- like the chapter ones above -- they have to land
        // before -filter_complex references any of their file indices.
        args.extend(seek_inputs.args);
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
        let mut next_input_index = self.inputs.len();
        let chapter_input = match self.output_chapters(output.id) {
            Some(ChapterSource::FromInput { input_file_index }) => Some(input_file_index),
            Some(ChapterSource::Edited { modifier_id }) => chapter_files.get(&modifier_id).map(|path| {
                let idx = next_input_index;
                args.push("-i".to_string());
                args.push(path.clone());
                next_input_index += 1;
                idx
            }),
            None => None,
        };
        let mut filter_complex = Vec::new();
        let mut seek_inputs = SeekInputs::new(next_input_index);
        let extra = vec!["-t".to_string(), duration_secs.to_string()];
        let section = self.build_output_section(
            output,
            Some(preview_path),
            &extra,
            &mut filter_complex,
            chapter_input,
            &mut seek_inputs,
        )?;
        args.extend(seek_inputs.args);
        if !filter_complex.is_empty() {
            args.push("-filter_complex".to_string());
            args.push(filter_complex.join(";"));
        }
        args.extend(section);
        Some(args)
    }

    /// Snapshots this graph into the serializable shape `crate::project`
    /// writes to disk -- see `ProjectFile`/`SavedInput` for what's kept and
    /// why (notably: an input's *raw* stream list, not the one with the
    /// synthetic chapter-stream entry already applied).
    pub fn to_project_file(&self) -> ProjectFile {
        ProjectFile {
            version: PROJECT_FORMAT_VERSION,
            inputs: self.inputs.iter().map(SavedInput::from_node).collect(),
            modifiers: self
                .modifiers
                .iter()
                .map(|m| ModifierNode { id: m.id, kind: m.kind.clone(), pos: m.pos, width: m.width })
                .collect(),
            outputs: self
                .outputs
                .iter()
                .map(|o| OutputNode {
                    id: o.id,
                    path: o.path.clone(),
                    container: o.container.clone(),
                    extra_args: o.extra_args.clone(),
                    pos: o.pos,
                    width: o.width,
                })
                .collect(),
            wires: self.wires.clone(),
            next_id: self.next_id,
        }
    }

    /// Rebuilds a `Graph` from a loaded `ProjectFile`. `reprobe` is called
    /// once per saved input with its path -- `Some((streams, chapters))` on
    /// a successful fresh probe (used in place of whatever was saved, so a
    /// file re-encoded since the last save is picked up correctly), `None`
    /// if it couldn't be probed at all (moved, deleted, ...), in which case
    /// the *saved* streams/chapters are used instead and the node is
    /// flagged `file_missing` so `ui` can render it accordingly -- the
    /// graph's structure and wires stay fully intact either way, since
    /// wires only reference node ids, never stream data directly. `Graph`
    /// stays free of file I/O itself, same convention `build_ffmpeg_args`'s
    /// `chapter_files` parameter already follows -- `crate::project::load`
    /// is what actually calls `ffmpeg::probe` and passes the result here.
    pub fn from_project_file(file: ProjectFile, mut reprobe: impl FnMut(&str) -> Option<(Vec<StreamInfo>, Vec<Chapter>)>) -> Graph {
        let mut inputs = Vec::with_capacity(file.inputs.len());
        for (file_index, saved) in file.inputs.into_iter().enumerate() {
            let (raw_streams, chapters, file_missing) = match reprobe(&saved.path) {
                Some((streams, chapters)) => (streams, chapters, false),
                None => (saved.streams, saved.chapters, true),
            };
            let streams = streams_with_chapter_marker(raw_streams, &chapters);
            inputs.push(InputNode {
                id: saved.id,
                path: saved.path,
                file_index,
                streams,
                chapters,
                extra_args: saved.extra_args,
                pos: saved.pos,
                width: saved.width,
                file_missing,
            });
        }
        Graph {
            inputs,
            modifiers: file.modifiers,
            outputs: file.outputs,
            wires: file.wires,
            next_id: file.next_id,
        }
    }
}

mod chapter;
mod node;
mod stream;
mod wire;

pub use chapter::{chapters_ffmetadata, format_time, parse_time, Chapter, ChapterSource};
pub use node::{
    disposition_flags, input_extra_arg_keys, metadata_keys_for, output_extra_arg_keys, FilterName, InputNode,
    ModifierKind, ModifierNode, OutputNode,
};
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

    /// Swaps two wires' positions in `self.wires`. `incoming`/`outgoing`
    /// report wires in `self.wires` order, and for an output's
    /// mapped-stream slot that order is what ffmpeg's `-map` sequence (and
    /// so the stream order in the muxed container) follows -- so this is
    /// how a caller reorders an output's streams without touching any
    /// wire's own `from`/`to`.
    pub fn swap_wires(&mut self, a: usize, b: usize) {
        self.wires.swap(a, b);
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

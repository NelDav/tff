use super::chapters::{chapter_edit_modifiers_fed_by, sync_chapter_edit_import};
use super::picker::{disposition_picker_options, field_picker_options, picker_options, prioritize_and_extend, selected_index};
use super::text_input::{path_suggestions, text_input_mode};
use super::{App, ExtraArgsTarget, Focus, Mode, PickerEntry, PickerKind, TextTarget};
use crate::graph::{Codec, Endpoint, ModifierKind, NodeId, OutputNode, StreamKind, Target};

impl App {
    /// Reached via the 'a' node picker's "input file..." entry -- prompts
    /// for a path, and on confirm probes it and adds the input.
    pub fn start_add_input(&mut self) {
        let buffer = String::new();
        let suggestions = path_suggestions(&buffer);
        self.mode = text_input_mode(TextTarget::NewInputPath, buffer, suggestions);
    }

    /// Reached via the 'a' node picker's "output" entry -- adds a new
    /// output node and focuses it immediately, no prompt needed (it starts
    /// with a sensible default path, editable afterward with 'o').
    pub fn add_output_node(&mut self) {
        self.graph.add_output();
        self.set_focus_index(self.node_count() - 1);
        self.log.push("added output node".to_string());
    }

    /// 'a': open a picker to choose what to add to the graph -- an input
    /// file, an output, or any modifier kind, all in one list for "add a
    /// node," full stop.
    pub fn open_add_node_picker(&mut self) {
        let options = vec![
            PickerEntry {
                display: "input file...".to_string(),
                value: Some("input".to_string()),
            },
            PickerEntry {
                display: "output".to_string(),
                value: Some("output".to_string()),
            },
            PickerEntry {
                display: "convert (codec)".to_string(),
                value: Some("convert".to_string()),
            },
            PickerEntry {
                display: "metadata (language / title)".to_string(),
                value: Some("metadata".to_string()),
            },
            PickerEntry {
                display: "disposition (default / forced / ...)".to_string(),
                value: Some("disposition".to_string()),
            },
            PickerEntry {
                display: "chapters (add / edit / remove)".to_string(),
                value: Some("chapters".to_string()),
            },
            PickerEntry {
                display: "shift (audio/video sync delay)".to_string(),
                value: Some("shift".to_string()),
            },
            PickerEntry {
                display: "volume".to_string(),
                value: Some("volume".to_string()),
            },
            PickerEntry {
                display: "scale (resize)".to_string(),
                value: Some("scale".to_string()),
            },
            PickerEntry {
                display: "crop".to_string(),
                value: Some("crop".to_string()),
            },
            PickerEntry {
                display: "fade in/out".to_string(),
                value: Some("fade".to_string()),
            },
            PickerEntry {
                display: "rotate (90/180 degrees)".to_string(),
                value: Some("rotate".to_string()),
            },
            PickerEntry {
                display: "trim (cut to a start/end range)".to_string(),
                value: Some("trim".to_string()),
            },
            PickerEntry {
                display: "concat (join same-kind streams end to end)".to_string(),
                value: Some("concat".to_string()),
            },
        ];
        self.mode = Mode::Picker {
            kind: PickerKind::NewNode,
            title: "add node".to_string(),
            options,
            selected: 0,
            query: String::new(),
            searching: false,
        };
    }

    /// 'o': edit the focused output's path. No-op unless an output is
    /// focused -- deliberately doesn't extend to inputs: an input's path is
    /// where its whole stream list came from, so "editing" it would mean
    /// re-probing and potentially invalidating existing wires. Simpler and
    /// less surprising to just add a new input node for a different file.
    pub fn start_edit_output(&mut self) {
        let Focus::Output(i) = self.focus else {
            self.log
                .push("focus an output node first, then 'o' edits its path".to_string());
            return;
        };
        let Some(output) = self.graph.outputs.get(i) else {
            return;
        };
        let buffer = output.path.clone();
        let suggestions = path_suggestions(&buffer);
        self.mode = text_input_mode(TextTarget::OutputPath(output.id), buffer, suggestions);
    }

    /// 'e': edit the focused node's primary setting, whatever kind of node
    /// it is -- a modifier's own fields (see `activate_modifier`), or an
    /// input/output's extra ffmpeg args. One key for "edit this node"
    /// regardless of kind, rather than 'e' for modifiers and a separate key
    /// for everything else.
    pub fn activate_focused(&mut self) {
        match self.focus {
            Focus::Modifier(_) => self.activate_modifier(),
            Focus::Input(i) => {
                let Some(input) = self.graph.inputs.get(i) else { return };
                self.open_extra_args_picker(ExtraArgsTarget::Input(input.id));
            }
            Focus::Output(i) => {
                let Some(output) = self.graph.outputs.get(i) else { return };
                self.open_extra_args_picker(ExtraArgsTarget::Output(output.id));
            }
        }
    }

    /// The stream kind flowing into a modifier, if it's connected --
    /// traced back through however many other modifiers sit upstream.
    pub(super) fn modifier_input_kind(&self, modifier_id: NodeId) -> Option<StreamKind> {
        let incoming = self
            .graph
            .wires
            .iter()
            .find(|w| w.to == Target::ModifierIn(modifier_id))?;
        self.endpoint_stream_kind(incoming.from)
    }

    /// The stream kind an endpoint ultimately carries -- a `ChapterEdit`
    /// modifier's output is always `Chapter` (its own list is
    /// authoritative regardless of what, if anything, feeds it; see
    /// `graph::ModifierKind::ChapterEdit`), otherwise this traces back
    /// through `resolve` to whatever real input stream is at the root.
    /// Used to decide which of an output's two connection targets an armed
    /// endpoint should land on (see `toggle_connect`'s `Focus::Output` arm).
    pub(super) fn endpoint_stream_kind(&self, ep: Endpoint) -> Option<StreamKind> {
        match ep {
            Endpoint::Stream { node, stream_idx } => {
                self.graph.input(node)?.streams.get(stream_idx).map(|s| s.kind)
            }
            Endpoint::ModifierOut(mid) => match &self.graph.modifier(mid)?.kind {
                ModifierKind::ChapterEdit { .. } => Some(StreamKind::Chapter),
                _ => {
                    let resolved = self.graph.resolve(ep)?;
                    self.graph.resolved_stream_kind(&resolved)
                }
            },
        }
    }

    /// 'e': edit the focused modifier's primary setting -- opens the codec
    /// picker for a Convert node, or opens a picker of metadata fields
    /// (curated for the connected stream's kind, plus a custom-key option)
    /// for a Metadata node.
    pub fn activate_modifier(&mut self) {
        let Focus::Modifier(i) = self.focus else {
            self.log
                .push("focus a convert or metadata node, then 'e' edits its setting".to_string());
            return;
        };
        let Some(m) = self.graph.modifiers.get(i) else {
            return;
        };
        let mid = m.id;
        match &m.kind {
            ModifierKind::Convert(current) => {
                let Some(kind) = self.modifier_input_kind(mid) else {
                    self.log.push(
                        "connect this node's input first ('c'), then 'e' to pick a codec"
                            .to_string(),
                    );
                    return;
                };
                let current = current.clone();

                let mut names: Vec<String> = Codec::curated_fallback(kind)
                    .into_iter()
                    .filter_map(|c| c.ffmpeg_name().map(str::to_string))
                    .collect();
                prioritize_and_extend(
                    &mut names,
                    self.available_encoders
                        .iter()
                        .filter(|(_, k)| *k == kind)
                        .map(|(n, _)| n.as_str()),
                );
                let options = picker_options("copy (no re-encode)", names);
                let selected = selected_index(&options, current.ffmpeg_name());

                self.mode = Mode::Picker {
                    kind: PickerKind::Codec { modifier: mid },
                    title: "convert: codec".to_string(),
                    options,
                    selected,
                    query: String::new(),
                    searching: false,
                };
            }
            ModifierKind::Metadata { fields } => {
                // The curated list is the same regardless of kind right
                // now (see metadata_keys_for's doc comment for why), but
                // stays kind-aware for when a genuinely kind-specific,
                // reliable key is found later. Unconnected nodes fall back
                // to StreamKind::Other's list rather than refusing outright
                // -- metadata is meaningful to set even before wiring is
                // finished, unlike a codec choice.
                let kind = self.modifier_input_kind(mid).unwrap_or(StreamKind::Other);
                let keys = crate::graph::metadata_keys_for(kind);
                let options = field_picker_options(fields, keys, true);

                self.mode = Mode::Picker {
                    kind: PickerKind::MetadataKey { modifier: mid },
                    title: "metadata: choose field".to_string(),
                    options,
                    selected: 0,
                    query: String::new(),
                    searching: false,
                };
            }
            ModifierKind::Disposition { flags } => {
                self.mode = Mode::Picker {
                    kind: PickerKind::DispositionFlags { modifier: mid },
                    title: "disposition: toggle flags".to_string(),
                    options: disposition_picker_options(flags),
                    selected: 0,
                    query: String::new(),
                    searching: false,
                };
            }
            ModifierKind::Filter { name, fields } => {
                let Some(kind) = self.modifier_input_kind(mid) else {
                    self.log.push(format!(
                        "connect this node's input first ('c'), then 'e' to configure {}",
                        name.label()
                    ));
                    return;
                };
                if !name.applies_to(kind) {
                    self.log.push(format!("{} doesn't apply to {} streams", name.label(), kind.noun()));
                    return;
                }
                let options = field_picker_options(fields, name.fields(), false);

                self.mode = Mode::Picker {
                    kind: PickerKind::FilterField { modifier: mid },
                    title: format!("{}: choose field", name.label()),
                    options,
                    selected: 0,
                    query: String::new(),
                    searching: false,
                };
            }
            ModifierKind::ChapterEdit { .. } => {
                self.open_chapter_table(mid);
            }
            ModifierKind::Concat => {
                self.log.push(
                    "nothing to configure -- arm a stream and 'c' to add/reorder segments"
                        .to_string(),
                );
            }
        }
    }

    /// 'x': remove the focused node. An input or modifier can always be
    /// removed; an output can be too, as long as it isn't the last one
    /// (ffmpeg needs somewhere to write to).
    pub fn delete_focused_node(&mut self) {
        match self.focus {
            Focus::Input(i) => {
                let Some(node) = self.graph.inputs.get(i) else {
                    return;
                };
                let id = node.id;
                let path = node.path.clone();
                let affected = chapter_edit_modifiers_fed_by(&self.graph, |w| {
                    matches!(w.from, Endpoint::Stream { node, .. } if node == id)
                });
                self.graph.remove_input(id);
                for mid in affected {
                    sync_chapter_edit_import(&mut self.graph, mid);
                }
                let still_from_this_input = |e: &Endpoint| matches!(e, Endpoint::Stream { node, .. } if *node == id);
                self.armed.retain(|e| !still_from_this_input(e));
                self.selected.retain(|e| !still_from_this_input(e));
                self.log.push(format!("removed input: {path}"));
                let n = self.graph.inputs.len();
                self.set_focus_index(i.min(n));
            }
            Focus::Modifier(i) => {
                let Some(m) = self.graph.modifiers.get(i) else {
                    return;
                };
                let id = m.id;
                self.graph.remove_modifier(id);
                self.armed.retain(|e| *e != Endpoint::ModifierOut(id));
                self.log.push("removed modifier node".to_string());
                let n = self.node_count();
                self.set_focus_index((self.graph.inputs.len() + i).min(n.saturating_sub(1)));
            }
            Focus::Output(i) => {
                if self.graph.outputs.len() <= 1 {
                    self.log.push("can't remove the last output".to_string());
                    return;
                }
                let Some(output) = self.graph.outputs.get(i) else {
                    return;
                };
                let id = output.id;
                let path = output.path.clone();
                self.graph.remove_output(id);
                self.log.push(format!("removed output: {path}"));
                let n = self.node_count();
                self.set_focus_index(
                    (self.graph.inputs.len() + self.graph.modifiers.len() + i)
                        .min(n.saturating_sub(1)),
                );
            }
        }
    }

    /// 'y': duplicate the focused node -- same settings (path/streams/
    /// chapters/extra_args for an input, kind/fields for a modifier, path/
    /// container/extra_args for an output), positioned just below-right of
    /// the original and focused immediately, mirroring the other "add a
    /// node" actions. A modifier's or output's *incoming* wire(s) are
    /// copied too (so a duplicate starts already wired to the same
    /// source(s) -- the usual reason to duplicate one of these is "same
    /// input, slightly different settings", and an output's chapters wire
    /// comes along the same way), but outgoing wires never are, and
    /// neither are an input's (it has none to speak of): a duplicate that
    /// quietly fanned its result into whatever the original already feeds
    /// would create a second, easy-to-miss parallel path into that same
    /// destination.
    pub fn duplicate_focused_node(&mut self) {
        match self.focus {
            Focus::Input(i) => {
                let Some(node) = self.graph.inputs.get(i) else { return };
                let path = node.path.clone();
                // Passed back through `add_input` raw (no synthetic
                // chapter-stream entry, no `extra_args`) -- it derives that
                // entry itself from `chapters`, so re-including one already
                // baked into `node.streams` would double it up.
                let streams: Vec<_> =
                    node.streams.iter().filter(|s| s.kind != StreamKind::Chapter).cloned().collect();
                let chapters = node.chapters.clone();
                let extra_args = node.extra_args.clone();
                let pos = (node.pos.0 + 2.0, node.pos.1 + 2.0);
                let width = node.width;

                let id = self.graph.add_input(path, streams, chapters);
                if let Some(new_node) = self.graph.input_mut(id) {
                    new_node.extra_args = extra_args;
                    new_node.pos = pos;
                    new_node.width = width;
                }
                self.set_focus_index(self.graph.inputs.len() - 1);
                self.log.push("duplicated input node".to_string());
            }
            Focus::Modifier(i) => {
                let Some(node) = self.graph.modifiers.get(i) else { return };
                let old_id = node.id;
                let kind = node.kind.clone();
                let pos = (node.pos.0 + 2.0, node.pos.1 + 2.0);
                let width = node.width;
                let incoming: Vec<Endpoint> = self
                    .graph
                    .incoming(Target::ModifierIn(old_id))
                    .into_iter()
                    .map(|wi| self.graph.wires[wi].from)
                    .collect();

                let id = self.graph.add_modifier(kind);
                if let Some(new_node) = self.graph.modifier_mut(id) {
                    new_node.pos = pos;
                    new_node.width = width;
                }
                for from in incoming {
                    self.graph.connect(from, Target::ModifierIn(id));
                }
                self.set_focus_index(self.graph.inputs.len() + self.graph.modifiers.len() - 1);
                self.log.push("duplicated modifier node".to_string());
            }
            Focus::Output(i) => {
                let Some(node) = self.graph.outputs.get(i) else { return };
                let old_id = node.id;
                let path = duplicate_output_path(&node.path, &self.graph.outputs);
                let container = node.container.clone();
                let extra_args = node.extra_args.clone();
                let pos = (node.pos.0 + 2.0, node.pos.1 + 2.0);
                let width = node.width;
                let incoming: Vec<Endpoint> = self
                    .graph
                    .incoming(Target::Output(old_id))
                    .into_iter()
                    .map(|wi| self.graph.wires[wi].from)
                    .collect();
                let chapter_from = self
                    .graph
                    .incoming(Target::OutputChapters(old_id))
                    .into_iter()
                    .next()
                    .map(|wi| self.graph.wires[wi].from);

                let id = self.graph.add_output();
                if let Some(new_node) = self.graph.output_mut(id) {
                    new_node.path = path;
                    new_node.container = container;
                    new_node.extra_args = extra_args;
                    new_node.pos = pos;
                    new_node.width = width;
                }
                for from in incoming {
                    self.graph.connect(from, Target::Output(id));
                }
                if let Some(from) = chapter_from {
                    self.graph.connect(from, Target::OutputChapters(id));
                }
                self.set_focus_index(self.node_count() - 1);
                self.log.push("duplicated output node".to_string());
            }
        }
    }
}

/// A duplicate output's default path: the original's with "-copy" (or
/// "-copy2", "-copy3", ...) inserted before the extension, stopping at the
/// first variant not already used by another output -- two outputs writing
/// to the exact same path isn't something ffmpeg can do sensibly, so the
/// duplicate needs *some* distinct starting point even though 'o' can
/// always retarget it afterward.
fn duplicate_output_path(original: &str, existing: &[OutputNode]) -> String {
    let (stem, ext) = match original.rfind('.') {
        Some(idx) if idx > 0 => (&original[..idx], &original[idx..]),
        _ => (original, ""),
    };
    let mut n = 1;
    loop {
        let suffix = if n == 1 { "-copy".to_string() } else { format!("-copy{n}") };
        let candidate = format!("{stem}{suffix}{ext}");
        if !existing.iter().any(|o| o.path == candidate) {
            return candidate;
        }
        n += 1;
    }
}

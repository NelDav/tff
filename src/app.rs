use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crate::ffmpeg;
use crate::graph::{Codec, Endpoint, Graph, ModifierKind, NodeId, StreamKind, Target};

/// Common containers offered first in the picker, ahead of the rest of
/// ffmpeg's discovered muxer list. Paired with the file extension to switch
/// the output path to for convenience -- purely cosmetic, since the actual
/// container is set via an explicit `-f` argument regardless of extension.
const COMMON_CONTAINERS: &[(&str, &str)] = &[
    ("matroska", "mkv"),
    ("mp4", "mp4"),
    ("mov", "mov"),
    ("webm", "webm"),
    ("avi", "avi"),
];

/// How much of the focused output's timeline 'p' renders before handing it
/// to ffplay -- long enough to judge codec/metadata choices, short enough
/// to stay fast even with a slow re-encode.
const PREVIEW_SECONDS: u32 = 20;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Input(usize),    // index into graph.inputs
    Modifier(usize), // index into graph.modifiers
    Output(usize),   // index into graph.outputs
}

pub enum TextTarget {
    NewInputPath,
    OutputPath(NodeId),
    /// Typing the value for a specific metadata key (curated or custom) on
    /// a Metadata node.
    ModifierMetadataValue { modifier: NodeId, key: String },
    /// Step one of "custom key...": typing the key name itself, before the
    /// value prompt for it opens.
    ModifierCustomKey(NodeId),
}

pub enum PickerKind {
    /// The modifier whose Convert codec is being chosen.
    Codec { modifier: NodeId },
    Container { output: NodeId },
    /// Choosing which kind of modifier node 'm' should create.
    NewModifier,
    /// Choosing which metadata field to edit on a Metadata node.
    MetadataKey { modifier: NodeId },
}

pub struct PickerEntry {
    pub display: String,
    /// `None` is the "reset" choice: Copy for a codec picker, "infer from
    /// extension" for the container picker. `Some(name)` is an explicit
    /// ffmpeg encoder/muxer name, or (for `NewModifier`) a kind tag.
    pub value: Option<String>,
}

pub enum Mode {
    Normal,
    TextInput {
        target: TextTarget,
        buffer: String,
        /// Files/directories in the buffer's current directory whose name
        /// starts with what's typed after the last '/' -- recomputed on
        /// every keystroke. See `path_suggestions`. Left empty for
        /// free-text fields (e.g. metadata) where path completion doesn't
        /// apply.
        suggestions: Vec<String>,
        selected: usize,
    },
    Picker {
        kind: PickerKind,
        title: String,
        options: Vec<PickerEntry>,
        /// Index into the *filtered* view (see `filtered_indices`), not
        /// directly into `options`.
        selected: usize,
        query: String,
        /// Whether `/` is currently accepting query text, vs. plain list
        /// navigation with a filter already applied (or none).
        searching: bool,
    },
}

/// The options whose display text matches `query` (case-insensitive
/// substring), as indices into `options`. Empty query matches everything.
pub fn filtered_indices(options: &[PickerEntry], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..options.len()).collect();
    }
    let query = query.to_lowercase();
    options
        .iter()
        .enumerate()
        .filter(|(_, o)| o.display.to_lowercase().contains(&query))
        .map(|(i, _)| i)
        .collect()
}

pub struct App {
    pub graph: Graph,
    pub focus: Focus,
    /// Selected row within the focused node: a stream index when an input
    /// is focused, an index into that modifier's outgoing connections when
    /// a modifier is focused, or an index into that output's incoming
    /// connections when an output is focused.
    pub row_idx: usize,
    pub mode: Mode,
    /// A source endpoint the user has armed, waiting to be wired into
    /// whichever modifier or output node gets focused next.
    pub armed: Option<Endpoint>,
    pub log: Vec<String>,
    pub status: String,
    pub running: bool,
    rx: Option<Receiver<String>>,
    /// Set while the in-flight ffmpeg job (tracked via `rx`/`running`) is a
    /// preview render rather than a real one -- the temp file path to hand
    /// to `ffplay` once it finishes successfully.
    preview_target: Option<String>,
    pub should_quit: bool,
    /// The real encoders/muxers this machine's ffmpeg build supports,
    /// queried once at startup. Empty if the query failed, in which case
    /// pickers fall back to a small curated list.
    available_encoders: Vec<(String, StreamKind)>,
    available_muxers: Vec<String>,
}

impl App {
    pub fn new() -> Self {
        let available_encoders = ffmpeg::list_encoders().unwrap_or_default();
        let available_muxers = ffmpeg::list_muxers().unwrap_or_default();
        let mut log = vec!["Press 'a' to add an input file, '?' for help.".to_string()];
        if available_encoders.is_empty() || available_muxers.is_empty() {
            log.push(
                "couldn't query ffmpeg's encoder/muxer list -- pickers will use a small built-in fallback"
                    .to_string(),
            );
        }
        App {
            graph: Graph::new(),
            focus: Focus::Output(0),
            row_idx: 0,
            mode: Mode::Normal,
            armed: None,
            log,
            status: String::new(),
            running: false,
            rx: None,
            preview_target: None,
            should_quit: false,
            available_encoders,
            available_muxers,
        }
    }

    fn node_count(&self) -> usize {
        self.graph.inputs.len() + self.graph.modifiers.len() + self.graph.outputs.len()
    }

    fn focus_index(&self) -> usize {
        match self.focus {
            Focus::Input(i) => i,
            Focus::Modifier(i) => self.graph.inputs.len() + i,
            Focus::Output(i) => self.graph.inputs.len() + self.graph.modifiers.len() + i,
        }
    }

    fn set_focus_index(&mut self, idx: usize) {
        let n_inputs = self.graph.inputs.len();
        let n_modifiers = self.graph.modifiers.len();
        self.focus = if idx < n_inputs {
            Focus::Input(idx)
        } else if idx < n_inputs + n_modifiers {
            Focus::Modifier(idx - n_inputs)
        } else {
            Focus::Output((idx - n_inputs - n_modifiers).min(self.graph.outputs.len().saturating_sub(1)))
        };
        self.row_idx = 0;
    }

    pub fn cycle_focus(&mut self, forward: bool) {
        let n = self.node_count();
        let cur = self.focus_index();
        let next = if forward {
            (cur + 1) % n
        } else {
            (cur + n - 1) % n
        };
        self.set_focus_index(next);
    }

    /// Up/Down while a node is focused: cycles the selected stream (input),
    /// selected outgoing connection (modifier), or selected incoming
    /// connection (output).
    pub fn cycle_row(&mut self, forward: bool) {
        let len = match self.focus {
            Focus::Input(i) => self.graph.inputs.get(i).map_or(0, |n| n.streams.len()),
            Focus::Modifier(i) => self
                .graph
                .modifiers
                .get(i)
                .map_or(0, |m| self.graph.outgoing(Endpoint::ModifierOut(m.id)).len()),
            Focus::Output(i) => self
                .graph
                .outputs
                .get(i)
                .map_or(0, |o| self.graph.incoming(Target::Output(o.id)).len()),
        };
        if len == 0 {
            return;
        }
        self.row_idx = if forward {
            (self.row_idx + 1) % len
        } else {
            (self.row_idx + len - 1) % len
        };
    }

    pub fn move_focused_node(&mut self, dx: f64, dy: f64) {
        let step = 2.0;
        let pos = match self.focus {
            Focus::Input(i) => self.graph.inputs.get_mut(i).map(|n| &mut n.pos),
            Focus::Modifier(i) => self.graph.modifiers.get_mut(i).map(|n| &mut n.pos),
            Focus::Output(i) => self.graph.outputs.get_mut(i).map(|n| &mut n.pos),
        };
        if let Some(pos) = pos {
            pos.0 = (pos.0 + dx * step).max(0.0);
            pos.1 = (pos.1 + dy * step).max(0.0);
        }
    }

    pub fn start_add_input(&mut self) {
        let buffer = String::new();
        let suggestions = path_suggestions(&buffer);
        self.mode = Mode::TextInput {
            target: TextTarget::NewInputPath,
            buffer,
            suggestions,
            selected: 0,
        };
    }

    /// 'O': add a new output node and focus it.
    pub fn add_output_node(&mut self) {
        self.graph.add_output();
        self.set_focus_index(self.node_count() - 1);
        self.log.push("added output node".to_string());
    }

    /// 'm': open a picker to choose which kind of modifier node to add.
    pub fn open_add_modifier_picker(&mut self) {
        let options = vec![
            PickerEntry { display: "convert (codec)".to_string(), value: Some("convert".to_string()) },
            PickerEntry { display: "metadata (language / title)".to_string(), value: Some("metadata".to_string()) },
        ];
        self.mode = Mode::Picker {
            kind: PickerKind::NewModifier,
            title: "add modifier node".to_string(),
            options,
            selected: 0,
            query: String::new(),
            searching: false,
        };
    }

    /// 'o': edit the focused output's path. No-op unless an output is
    /// focused -- there's nothing to edit on an input or modifier node.
    pub fn start_edit_output(&mut self) {
        let Focus::Output(i) = self.focus else {
            self.log.push("focus an output node first, then 'o' edits its path".to_string());
            return;
        };
        let Some(output) = self.graph.outputs.get(i) else { return };
        let buffer = output.path.clone();
        let suggestions = path_suggestions(&buffer);
        self.mode = Mode::TextInput {
            target: TextTarget::OutputPath(output.id),
            buffer,
            suggestions,
            selected: 0,
        };
    }

    /// The stream kind flowing into a modifier, if it's connected --
    /// traced back through however many other modifiers sit upstream.
    fn modifier_input_kind(&self, modifier_id: NodeId) -> Option<StreamKind> {
        let incoming = self.graph.wires.iter().find(|w| w.to == Target::ModifierIn(modifier_id))?;
        let resolved = self.graph.resolve(incoming.from)?;
        let input = self.graph.input(resolved.from_node)?;
        input.streams.get(resolved.from_stream_idx).map(|s| s.kind)
    }

    /// 'e': edit the focused modifier's primary setting -- opens the codec
    /// picker for a Convert node, or opens a picker of metadata fields
    /// (curated for the connected stream's kind, plus a custom-key option)
    /// for a Metadata node.
    pub fn activate_modifier(&mut self) {
        let Focus::Modifier(i) = self.focus else {
            self.log.push("focus a convert or metadata node, then 'e' edits its setting".to_string());
            return;
        };
        let Some(m) = self.graph.modifiers.get(i) else { return };
        let mid = m.id;
        match &m.kind {
            ModifierKind::Convert(current) => {
                let Some(kind) = self.modifier_input_kind(mid) else {
                    self.log
                        .push("connect this node's input first ('c'), then 'e' to pick a codec".to_string());
                    return;
                };
                let current = current.clone();

                let mut names: Vec<String> = Codec::curated_fallback(kind)
                    .into_iter()
                    .filter_map(|c| c.ffmpeg_name().map(str::to_string))
                    .collect();
                prioritize_and_extend(
                    &mut names,
                    self.available_encoders.iter().filter(|(_, k)| *k == kind).map(|(n, _)| n.as_str()),
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
                let mut options: Vec<PickerEntry> = keys
                    .iter()
                    .map(|k| {
                        let display = match fields.get(*k) {
                            Some(v) => format!("{k}: {v}"),
                            None => format!("{k}: (not set)"),
                        };
                        PickerEntry { display, value: Some((*k).to_string()) }
                    })
                    .collect();
                // Any already-set keys outside the curated list (set via a
                // previous "custom key...") should still be visible/editable.
                for (k, v) in fields {
                    if !keys.contains(&k.as_str()) {
                        options.push(PickerEntry { display: format!("{k}: {v}"), value: Some(k.clone()) });
                    }
                }
                options.push(PickerEntry { display: "custom key…".to_string(), value: None });

                self.mode = Mode::Picker {
                    kind: PickerKind::MetadataKey { modifier: mid },
                    title: "metadata: choose field".to_string(),
                    options,
                    selected: 0,
                    query: String::new(),
                    searching: false,
                };
            }
        }
    }

    pub fn cancel_text_input(&mut self) {
        self.mode = Mode::Normal;
    }

    pub fn confirm_text_input(&mut self) {
        let Mode::TextInput { target, buffer, .. } =
            std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            return;
        };
        match target {
            TextTarget::NewInputPath => {
                let path = clean_path_input(&buffer);
                if path.is_empty() {
                    return;
                }
                match ffmpeg::probe(&path) {
                    Ok(streams) => {
                        let id = self.graph.add_input(path.clone(), streams);
                        self.log.push(format!("added input: {path}"));
                        let idx = self.graph.inputs.len() - 1;
                        debug_assert_eq!(self.graph.inputs[idx].id, id);
                        self.set_focus_index(idx);
                    }
                    Err(e) => {
                        self.log.push(format!("error probing '{path}': {e}"));
                    }
                }
            }
            TextTarget::OutputPath(output_id) => {
                let path = clean_path_input(&buffer);
                if !path.is_empty()
                    && let Some(node) = self.graph.output_mut(output_id) {
                        node.path = path;
                    }
            }
            TextTarget::ModifierMetadataValue { modifier, key } => {
                let value = buffer.trim().to_string();
                if let Some(m) = self.graph.modifier_mut(modifier)
                    && let ModifierKind::Metadata { fields } = &mut m.kind {
                        if value.is_empty() {
                            fields.remove(&key);
                            self.log.push(format!("{key} cleared"));
                        } else {
                            fields.insert(key.clone(), value.clone());
                            self.log.push(format!("{key} set to {value}"));
                        }
                    }
            }
            TextTarget::ModifierCustomKey(modifier) => {
                let key = buffer.trim().to_string();
                if key.is_empty() {
                    return;
                }
                let current = self
                    .graph
                    .modifier(modifier)
                    .and_then(|m| match &m.kind {
                        ModifierKind::Metadata { fields } => fields.get(&key).cloned(),
                        ModifierKind::Convert(_) => None,
                    })
                    .unwrap_or_default();
                self.mode = Mode::TextInput {
                    target: TextTarget::ModifierMetadataValue { modifier, key },
                    buffer: current,
                    suggestions: Vec::new(),
                    selected: 0,
                };
            }
        }
    }

    pub fn text_input_char(&mut self, c: char) {
        if let Mode::TextInput { target, buffer, suggestions, selected } = &mut self.mode {
            buffer.push(c);
            if matches!(target, TextTarget::NewInputPath | TextTarget::OutputPath(_)) {
                *suggestions = path_suggestions(buffer);
                *selected = 0;
            }
        }
    }

    pub fn text_input_backspace(&mut self) {
        if let Mode::TextInput { target, buffer, suggestions, selected } = &mut self.mode {
            buffer.pop();
            if matches!(target, TextTarget::NewInputPath | TextTarget::OutputPath(_)) {
                *suggestions = path_suggestions(buffer);
                *selected = 0;
            }
        }
    }

    /// Up/Down while typing a path: move the highlighted suggestion.
    pub fn text_input_move_suggestion(&mut self, delta: isize) {
        if let Mode::TextInput { suggestions, selected, .. } = &mut self.mode {
            let len = suggestions.len() as isize;
            if len == 0 {
                return;
            }
            *selected = (*selected as isize + delta).rem_euclid(len) as usize;
        }
    }

    /// Tab: replace the buffer's current path segment with the highlighted
    /// suggestion (shell-style completion), and refresh suggestions against
    /// the new, longer buffer so drilling into a directory keeps working.
    pub fn text_input_accept_suggestion(&mut self) {
        if let Mode::TextInput { buffer, suggestions, selected, .. } = &mut self.mode
            && let Some(chosen) = suggestions.get(*selected).cloned() {
                *buffer = chosen;
                *suggestions = path_suggestions(buffer);
                *selected = 0;
            }
    }

    /// 'c': arm the focused input stream or modifier output, or -- when
    /// something is armed and a modifier/output node is focused --
    /// wire it in. Pressing 'c' again on the exact thing that's currently
    /// armed disarms it.
    pub fn toggle_connect(&mut self) {
        match self.focus {
            Focus::Input(i) => {
                let Some(node) = self.graph.inputs.get(i) else { return };
                let Some(stream) = node.streams.get(self.row_idx) else { return };
                let ep = Endpoint::Stream { node: node.id, stream_idx: self.row_idx };
                if self.armed == Some(ep) {
                    self.armed = None; // disarm
                } else {
                    self.log.push(format!(
                        "armed {} from {} — focus a modifier or output, press 'c' to connect",
                        stream.label(),
                        node.path
                    ));
                    self.armed = Some(ep);
                }
            }
            Focus::Modifier(i) => {
                let Some(m) = self.graph.modifiers.get(i) else { return };
                let this_output = Endpoint::ModifierOut(m.id);
                match self.armed {
                    Some(source) if source == this_output => {
                        self.armed = None; // disarm
                    }
                    Some(source) => {
                        self.graph.connect(source, Target::ModifierIn(m.id));
                        self.armed = None;
                        self.log.push("connected".to_string());
                    }
                    None => {
                        self.armed = Some(this_output);
                        self.log
                            .push("armed this node's output — focus the next node, press 'c' to connect".to_string());
                    }
                }
            }
            Focus::Output(i) => {
                let Some(output) = self.graph.outputs.get(i) else { return };
                match self.armed.take() {
                    Some(source) => {
                        self.graph.connect(source, Target::Output(output.id));
                        self.log.push("connected to output".to_string());
                    }
                    None => {
                        self.log.push("nothing armed -- arm a stream or modifier output first ('c')".to_string());
                    }
                }
            }
        }
    }

    /// 'd': on an input port, disconnect it from everything downstream; on
    /// a modifier, disconnect just the selected outgoing connection; on an
    /// output, disconnect just the selected incoming connection.
    pub fn disconnect_focused(&mut self) {
        match self.focus {
            Focus::Input(i) => {
                let Some(node) = self.graph.inputs.get(i) else { return };
                let Some(stream) = node.streams.get(self.row_idx) else { return };
                let ep = Endpoint::Stream { node: node.id, stream_idx: self.row_idx };
                let label = stream.label();
                let before = self.graph.wires.len();
                self.graph.wires.retain(|w| w.from != ep);
                if self.graph.wires.len() != before {
                    self.log.push(format!("disconnected {label} from everything downstream"));
                }
            }
            Focus::Modifier(i) => {
                let Some(m) = self.graph.modifiers.get(i) else { return };
                let ep = Endpoint::ModifierOut(m.id);
                let outgoing = self.graph.outgoing(ep);
                let Some(&wi) = outgoing.get(self.row_idx) else { return };
                self.graph.remove_wire_at(wi);
                self.log.push("disconnected".to_string());
                let new_len = self.graph.outgoing(ep).len();
                if new_len > 0 && self.row_idx >= new_len {
                    self.row_idx = new_len - 1;
                }
            }
            Focus::Output(i) => {
                let Some(output) = self.graph.outputs.get(i) else { return };
                let target = Target::Output(output.id);
                let incoming = self.graph.incoming(target);
                let Some(&wi) = incoming.get(self.row_idx) else { return };
                self.graph.remove_wire_at(wi);
                self.log.push("disconnected".to_string());
                let new_len = self.graph.incoming(target).len();
                if new_len > 0 && self.row_idx >= new_len {
                    self.row_idx = new_len - 1;
                }
            }
        }
    }

    /// 'f': open a picker listing ffmpeg's available output containers for
    /// the focused output node.
    pub fn open_container_picker(&mut self) {
        let Focus::Output(i) = self.focus else {
            self.log.push("focus an output node first, then 'f' picks its container".to_string());
            return;
        };
        let Some(output) = self.graph.outputs.get(i) else { return };
        let output_id = output.id;
        let current = output.container.clone();

        let mut names: Vec<String> = COMMON_CONTAINERS.iter().map(|(name, _)| name.to_string()).collect();
        prioritize_and_extend(&mut names, self.available_muxers.iter().map(String::as_str));

        let options = picker_options("auto (infer from file extension)", names);
        let selected = selected_index(&options, current.as_deref());

        self.mode = Mode::Picker {
            kind: PickerKind::Container { output: output_id },
            title: "output container".to_string(),
            options,
            selected,
            query: String::new(),
            searching: false,
        };
    }

    /// Up/Down (or j/k) while a picker is open. Moves within the filtered
    /// view, so it only ever lands on something currently visible.
    pub fn picker_move(&mut self, delta: isize) {
        if let Mode::Picker { options, selected, query, .. } = &mut self.mode {
            let len = filtered_indices(options, query).len() as isize;
            if len == 0 {
                return;
            }
            *selected = (*selected as isize + delta).rem_euclid(len) as usize;
        }
    }

    /// '/': start typing a query to filter the picker's options.
    pub fn picker_start_search(&mut self) {
        if let Mode::Picker { query, searching, selected, .. } = &mut self.mode {
            query.clear();
            *searching = true;
            *selected = 0;
        }
    }

    pub fn picker_search_char(&mut self, c: char) {
        if let Mode::Picker { query, selected, .. } = &mut self.mode {
            query.push(c);
            *selected = 0;
        }
    }

    pub fn picker_search_backspace(&mut self) {
        if let Mode::Picker { query, selected, .. } = &mut self.mode {
            query.pop();
            *selected = 0;
        }
    }

    /// Enter while typing a query: stop typing, keep the filter applied so
    /// arrow keys go back to navigating the (now filtered) list.
    pub fn picker_confirm_search(&mut self) {
        if let Mode::Picker { searching, .. } = &mut self.mode {
            *searching = false;
        }
    }

    /// Esc: while typing a query, cancel it outright. Otherwise, clear an
    /// already-applied filter first (mirrors vim's "clear search" on a bare
    /// Esc); only close the picker once there's no filter left to clear.
    pub fn picker_escape(&mut self) {
        let Mode::Picker { kind, title, options, mut query, searching, .. } =
            std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            return;
        };
        if searching || !query.is_empty() {
            query.clear();
            self.mode = Mode::Picker {
                kind,
                title,
                options,
                selected: 0,
                query,
                searching: false,
            };
        }
        // else: leave as Mode::Normal, set by the replace above -- this is
        // the "close the picker" case.
    }

    pub fn picker_confirm(&mut self) {
        let Mode::Picker { kind, options, selected, query, .. } =
            std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            return;
        };
        let real_idx = filtered_indices(&options, &query).get(selected).copied();
        let Some(entry) = real_idx.and_then(|i| options.into_iter().nth(i)) else {
            return;
        };

        match kind {
            PickerKind::Codec { modifier } => {
                let codec = match entry.value {
                    None => Codec::Copy,
                    Some(name) => Codec::Encode(name),
                };
                self.log.push(match &codec {
                    Codec::Copy => "codec set to copy (no re-encode)".to_string(),
                    Codec::Encode(_) => format!("codec set to {}", codec.label()),
                });
                if let Some(m) = self.graph.modifier_mut(modifier) {
                    m.kind = ModifierKind::Convert(codec);
                }
            }
            PickerKind::Container { output } => {
                let Some(node) = self.graph.output_mut(output) else { return };
                node.container = entry.value.clone();
                match &entry.value {
                    Some(name) => {
                        if let Some((_, ext)) = COMMON_CONTAINERS.iter().find(|(n, _)| n == name) {
                            let stem = std::path::Path::new(&node.path).with_extension("");
                            node.path = format!("{}.{ext}", stem.to_string_lossy());
                        }
                        self.log.push(format!("output container set to {name} ({})", node.path));
                    }
                    None => self
                        .log
                        .push("output container set to auto (inferred from file extension)".to_string()),
                }
            }
            PickerKind::NewModifier => {
                let (kind, name) = match entry.value.as_deref() {
                    Some("metadata") => (ModifierKind::Metadata { fields: BTreeMap::new() }, "metadata"),
                    _ => (ModifierKind::Convert(Codec::Copy), "convert"),
                };
                self.graph.add_modifier(kind);
                self.set_focus_index(self.node_count() - self.graph.outputs.len() - 1);
                self.log.push(format!("added {name} node"));
            }
            PickerKind::MetadataKey { modifier } => match entry.value {
                Some(key) => {
                    let current = self
                        .graph
                        .modifier(modifier)
                        .and_then(|m| match &m.kind {
                            ModifierKind::Metadata { fields } => fields.get(&key).cloned(),
                            ModifierKind::Convert(_) => None,
                        })
                        .unwrap_or_default();
                    self.mode = Mode::TextInput {
                        target: TextTarget::ModifierMetadataValue { modifier, key },
                        buffer: current,
                        suggestions: Vec::new(),
                        selected: 0,
                    };
                }
                None => {
                    // "custom key..." -- first ask for the key name itself.
                    self.mode = Mode::TextInput {
                        target: TextTarget::ModifierCustomKey(modifier),
                        buffer: String::new(),
                        suggestions: Vec::new(),
                        selected: 0,
                    };
                }
            },
        }
    }

    /// 'x': remove the focused node. An input or modifier can always be
    /// removed; an output can be too, as long as it isn't the last one
    /// (ffmpeg needs somewhere to write to).
    pub fn delete_focused_node(&mut self) {
        match self.focus {
            Focus::Input(i) => {
                let Some(node) = self.graph.inputs.get(i) else { return };
                let id = node.id;
                let path = node.path.clone();
                self.graph.remove_input(id);
                self.armed = self.armed.filter(|e| !matches!(e, Endpoint::Stream { node, .. } if *node == id));
                self.log.push(format!("removed input: {path}"));
                let n = self.graph.inputs.len();
                self.set_focus_index(i.min(n));
            }
            Focus::Modifier(i) => {
                let Some(m) = self.graph.modifiers.get(i) else { return };
                let id = m.id;
                self.graph.remove_modifier(id);
                self.armed = self.armed.filter(|e| *e != Endpoint::ModifierOut(id));
                self.log.push("removed modifier node".to_string());
                let n = self.node_count();
                self.set_focus_index((self.graph.inputs.len() + i).min(n.saturating_sub(1)));
            }
            Focus::Output(i) => {
                if self.graph.outputs.len() <= 1 {
                    self.log.push("can't remove the last output".to_string());
                    return;
                }
                let Some(output) = self.graph.outputs.get(i) else { return };
                let id = output.id;
                let path = output.path.clone();
                self.graph.remove_output(id);
                self.log.push(format!("removed output: {path}"));
                let n = self.node_count();
                self.set_focus_index((self.graph.inputs.len() + self.graph.modifiers.len() + i).min(n.saturating_sub(1)));
            }
        }
    }

    pub fn start_render(&mut self) {
        if self.running {
            return;
        }
        if self.graph.wires.is_empty() {
            self.log.push(
                "nothing mapped yet — arm a stream with 'c', then focus a modifier or output and press 'c' again"
                    .to_string(),
            );
            return;
        }
        let (tx, rx): (Sender<String>, Receiver<String>) = mpsc::channel();
        self.rx = Some(rx);
        self.running = true;
        self.status = "running ffmpeg...".to_string();
        let args = self.graph.build_ffmpeg_args();
        self.log.push(format!("$ ffmpeg {}", args.join(" ")));
        thread::spawn(move || {
            ffmpeg::run_args(args, tx);
        });
    }

    /// 'p': render the first `PREVIEW_SECONDS` of the focused output's
    /// current mapping to a temp file, then hand it to ffplay once that
    /// finishes -- lets the user see how codec/metadata choices actually
    /// turn out without waiting for (or overwriting) the real output.
    pub fn start_preview(&mut self) {
        if self.running {
            self.log.push("already running ffmpeg — wait for it to finish before previewing".to_string());
            return;
        }
        let Focus::Output(i) = self.focus else {
            self.log.push("focus an output node first, then 'p' previews it".to_string());
            return;
        };
        let Some(output) = self.graph.outputs.get(i) else { return };
        let output_id = output.id;
        let ext = std::path::Path::new(&output.path).extension().and_then(|e| e.to_str()).unwrap_or("mkv");
        let preview_path =
            std::env::temp_dir().join(format!("tff-preview-{output_id}.{ext}")).to_string_lossy().into_owned();

        let Some(args) = self.graph.build_preview_args(output_id, &preview_path, PREVIEW_SECONDS) else {
            self.log.push(
                "nothing mapped to this output yet — arm a stream with 'c', then focus it and press 'c' again"
                    .to_string(),
            );
            return;
        };

        let (tx, rx): (Sender<String>, Receiver<String>) = mpsc::channel();
        self.rx = Some(rx);
        self.running = true;
        self.status = format!("rendering {PREVIEW_SECONDS}s preview...");
        self.preview_target = Some(preview_path);
        self.log.push(format!("$ ffmpeg {}", args.join(" ")));
        thread::spawn(move || {
            ffmpeg::run_args(args, tx);
        });
    }

    pub fn poll_ffmpeg(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut done = None;
        while let Ok(line) = rx.try_recv() {
            if let Some(code) = line.strip_prefix("__DONE__") {
                done = Some(code.to_string());
            } else {
                self.log.push(line);
            }
        }
        if let Some(code) = done {
            self.running = false;
            self.rx = None;
            if let Some(path) = self.preview_target.take() {
                if code == "0" {
                    self.status = "preview ready".to_string();
                    self.log.push(format!("$ ffplay {path}"));
                    if let Err(e) = ffmpeg::play(&path) {
                        self.status = format!("couldn't launch ffplay: {e:#}");
                        self.log.push(self.status.clone());
                    }
                } else {
                    self.status = format!("preview render failed (exit code {code})");
                    self.log.push(self.status.clone());
                }
            } else {
                self.status = format!("ffmpeg exited with code {code}");
                self.log.push(self.status.clone());
            }
        }
    }
}

/// Paths typed into the text field are passed straight to `ffprobe`/`ffmpeg`
/// via `Command`, with no shell in between — so `~` never gets expanded and
/// stray wrapping quotes (common when pasting from a file manager) are taken
/// literally. Clean those up here, once, at the point of entry.
fn clean_path_input(raw: &str) -> String {
    let mut s = raw.trim();
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            s = &s[1..s.len() - 1];
        }
    }
    expand_tilde(s)
}

/// Expands a leading `~` or `~/...` to `$HOME`. Anything else is returned
/// unchanged (including a bare relative path, which is left for the OS to
/// resolve against the process's own working directory).
fn expand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    } else if s == "~"
        && let Ok(home) = std::env::var("HOME") {
            return home;
        }
    s.to_string()
}

/// Files/directories matching what's typed after the last '/' in `buffer`,
/// each returned as a full replacement value for the buffer (so accepting
/// one is just `buffer = suggestion`). Directories get a trailing '/' so
/// the user can keep completing deeper. Mirrors familiar shell completion:
/// entries are listed relative to whatever the user already typed (so `~`
/// notation is preserved in the buffer even though listing itself needs
/// the expanded path), sorted alphabetically, and dotfiles are hidden
/// unless the user is already typing a dot-prefix.
pub fn path_suggestions(buffer: &str) -> Vec<String> {
    // A bare "~" (no '/' yet) should offer the home directory's contents,
    // same as "~/" -- normalize once and recurse rather than duplicating
    // the split/scan logic for that one case.
    if buffer.starts_with('~') && !buffer.contains('/') {
        return path_suggestions(&format!("~/{}", &buffer[1..]));
    }

    let (dir_part, prefix) = match buffer.rfind('/') {
        Some(idx) => (&buffer[..idx + 1], &buffer[idx + 1..]),
        None => ("", buffer),
    };

    let scan_target = if dir_part.is_empty() { ".".to_string() } else { expand_tilde(dir_part) };
    let Ok(read_dir) = std::fs::read_dir(&scan_target) else {
        return Vec::new();
    };

    let show_hidden = prefix.starts_with('.');
    let mut entries: Vec<(String, bool)> = read_dir
        .filter_map(|e| e.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(prefix) || (name.starts_with('.') && !show_hidden) {
                return None;
            }
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            Some((name, is_dir))
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    entries
        .into_iter()
        .map(|(name, is_dir)| format!("{dir_part}{name}{}", if is_dir { "/" } else { "" }))
        .collect()
}

/// If ffmpeg actually reported a discovered list, keep only the curated
/// `names` that are really present in it (never claim support for
/// something the local build lacks), then append the rest of the
/// discovered list alphabetically. Leaves `names` untouched if discovery
/// failed (empty), so the curated fallback is still offered.
fn prioritize_and_extend<'a>(names: &mut Vec<String>, discovered: impl Iterator<Item = &'a str>) {
    let discovered: Vec<&str> = discovered.collect();
    if discovered.is_empty() {
        return;
    }
    names.retain(|n| discovered.contains(&n.as_str()));
    let mut rest: Vec<String> = discovered
        .iter()
        .filter(|d| !names.iter().any(|n| n == *d))
        .map(|d| d.to_string())
        .collect();
    rest.sort();
    names.extend(rest);
}

fn picker_options(reset_label: &str, names: Vec<String>) -> Vec<PickerEntry> {
    let mut options = vec![PickerEntry {
        display: reset_label.to_string(),
        value: None,
    }];
    options.extend(
        names
            .into_iter()
            .map(|n| PickerEntry { display: n.clone(), value: Some(n) }),
    );
    options
}

fn selected_index(options: &[PickerEntry], current: Option<&str>) -> usize {
    match current {
        None => 0,
        Some(name) => options.iter().position(|o| o.value.as_deref() == Some(name)).unwrap_or(0),
    }
}

use std::collections::{BTreeMap, BTreeSet};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crossterm::event::{Event as CrosstermEvent, KeyEvent};
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use crate::ffmpeg;
use crate::graph::{Chapter, Codec, Endpoint, FilterName, Graph, ModifierKind, NodeId, StreamKind, Target};

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
    ModifierMetadataValue {
        modifier: NodeId,
        key: String,
    },
    /// Step one of "custom key...": typing the key name itself, before the
    /// value prompt for it opens.
    ModifierCustomKey(NodeId),
    /// Typing the value for a specific parameter of a Filter node.
    ModifierFilterValue {
        modifier: NodeId,
        key: String,
    },
    /// Typing the value for a specific extra-ffmpeg-arg key (curated or
    /// custom) on an input or output node.
    ExtraArgValue {
        target: ExtraArgsTarget,
        key: String,
    },
    /// Step one of extra-args' "custom key...": typing the key name itself,
    /// before the value prompt for it opens.
    ExtraArgCustomKey(ExtraArgsTarget),
    /// Typing a chapter's start or end time (see `ChapterTimeField`), on a
    /// `ChapterEdit` modifier node.
    ChapterTime {
        modifier: NodeId,
        index: usize,
        field: ChapterTimeField,
    },
    /// Typing a chapter's title, on a `ChapterEdit` modifier node.
    ChapterTitle {
        modifier: NodeId,
        index: usize,
    },
}

/// Which of a chapter's two time fields a `ChapterTime` text-input session
/// is editing.
#[derive(Clone, Copy)]
pub enum ChapterTimeField {
    Start,
    End,
}

/// Which node's `extra_args` map a picker/text-input session is editing --
/// input and output nodes share the exact same editing flow (curated-keys
/// picker with a custom-key escape hatch, see `graph::input_extra_arg_keys`
/// / `output_extra_arg_keys`), so this lets one set of picker/text-input
/// plumbing serve both instead of duplicating it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ExtraArgsTarget {
    Input(NodeId),
    Output(NodeId),
}

pub enum PickerKind {
    /// The modifier whose Convert codec is being chosen.
    Codec {
        modifier: NodeId,
    },
    Container {
        output: NodeId,
    },
    /// Choosing which kind of node 'a' should add -- an input, an output,
    /// or any modifier kind. Input/output aren't structurally special
    /// enough to deserve their own dedicated add-keys the way they used to
    /// (that was 'a' + 'O'); this is the same list a modifier picks from,
    /// just with two more entries at the top.
    NewNode,
    /// Choosing which metadata field to edit on a Metadata node.
    MetadataKey {
        modifier: NodeId,
    },
    /// Toggling disposition flags on a Disposition node. Unlike every other
    /// picker kind, this is a multi-select: confirming an entry toggles it
    /// and leaves the picker open instead of closing it.
    DispositionFlags {
        modifier: NodeId,
    },
    /// Choosing which parameter to edit on a Filter node.
    FilterField {
        modifier: NodeId,
    },
    /// Choosing a value for a Filter parameter that only accepts a fixed
    /// set of values (see `FilterName::value_options`), e.g. Rotate's
    /// "direction" -- a selection instead of `ModifierFilterValue`'s free
    /// text, since anything else typed there is simply invalid.
    FilterFieldValue {
        modifier: NodeId,
        key: String,
    },
    /// Choosing which extra ffmpeg arg to edit on an input or output node.
    /// Like `DispositionFlags`, a curated valueless key (e.g. `shortest`,
    /// `re`) toggles in place and leaves the picker open instead of
    /// closing it; a value-taking key (curated or custom) opens
    /// `ExtraArgValue`'s text input instead, same as `FilterField`.
    ExtraArgField {
        target: ExtraArgsTarget,
    },
    /// Choosing which chapter to edit on a `ChapterEdit` modifier, or to
    /// add a new one / import from whatever's connected to its input.
    /// Reached via 'e' on a focused `ChapterEdit` node.
    ChapterList {
        modifier: NodeId,
    },
    /// Editing one chapter's start/end/title, or deleting it.
    ChapterField {
        modifier: NodeId,
        index: usize,
    },
}

pub struct PickerEntry {
    pub display: String,
    /// `None` is the "reset" choice: Copy for a codec picker, "infer from
    /// extension" for the container picker. `Some(name)` is an explicit
    /// ffmpeg encoder/muxer name, or (for `NewNode`) a kind tag.
    pub value: Option<String>,
}

pub enum Mode {
    Normal,
    TextInput {
        target: TextTarget,
        input: Input,
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
    /// Source endpoints the user has armed, waiting to be wired into
    /// whichever modifier or output node gets focused next -- 'c' on that
    /// next node connects every one of them in a single action. A
    /// modifier's input only ever holds one wire, so connecting rejects
    /// this if it holds more than one entry; an output has no such limit.
    pub armed: BTreeSet<Endpoint>,
    /// Ports explicitly picked (via Space or Shift+Up/Down) but not yet
    /// armed -- a staging area so several can be gathered, from one input
    /// node or several, before committing them to `armed` in one 'c'
    /// press. See `toggle_port_selection`/`extend_port_selection`.
    pub selected: BTreeSet<Endpoint>,
    /// The row Shift+Up/Down range-selection started extending from, so
    /// repeated presses grow/shrink a contiguous range instead of just
    /// toggling one row at a time. Only meaningful for the input node it
    /// was set on; reset by anything that isn't itself a Shift+Up/Down
    /// press (see `extend_port_selection`'s doc comment).
    selection_anchor: Option<usize>,
    pub log: Vec<String>,
    pub status: String,
    pub running: bool,
    rx: Option<Receiver<String>>,
    /// Set while the in-flight ffmpeg job (tracked via `rx`/`running`) is a
    /// preview render rather than a real one -- the temp file path it's
    /// rendering to, so `poll_ffmpeg` knows what finished.
    preview_target: Option<String>,
    /// Set once a preview render finishes successfully -- App has no
    /// terminal to play it on (only main.rs does, and only main.rs can
    /// yield the TUI's alternate screen for mpv's fallback rendering), so
    /// this just hands off the finished path; main.rs takes it and decides
    /// how to actually show it.
    pub preview_ready: Option<String>,
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
            armed: BTreeSet::new(),
            selected: BTreeSet::new(),
            selection_anchor: None,
            log,
            status: String::new(),
            running: false,
            rx: None,
            preview_target: None,
            preview_ready: None,
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
            Focus::Output(
                (idx - n_inputs - n_modifiers).min(self.graph.outputs.len().saturating_sub(1)),
            )
        };
        self.row_idx = 0;
        self.selection_anchor = None; // a fresh Shift+range on the new node should start from here, not carry over
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
    /// connection (output) -- an output's row list includes one more row
    /// than its mapped-stream count when its chapters slot is connected
    /// (see `disconnect_focused`'s Output branch), same as an unconnected
    /// chapters slot doesn't get a row at all in the UI.
    pub fn cycle_row(&mut self, forward: bool) {
        let len = match self.focus {
            Focus::Input(i) => self.graph.inputs.get(i).map_or(0, |n| n.streams.len()),
            Focus::Modifier(i) => self.graph.modifiers.get(i).map_or(0, |m| {
                self.graph.outgoing(Endpoint::ModifierOut(m.id)).len()
            }),
            Focus::Output(i) => self.graph.outputs.get(i).map_or(0, |o| {
                let mapped = self.graph.incoming(Target::Output(o.id)).len();
                let has_chapters = !self.graph.incoming(Target::OutputChapters(o.id)).is_empty();
                mapped + usize::from(has_chapters)
            }),
        };
        if len == 0 {
            return;
        }
        self.row_idx = if forward {
            (self.row_idx + 1) % len
        } else {
            (self.row_idx + len - 1) % len
        };
        self.selection_anchor = None; // plain navigation ends any in-progress Shift+range
    }

    /// Space: toggle the currently-hovered stream's membership in the
    /// pending selection -- only meaningful on an input node, since that's
    /// the only place multiple distinct ports genuinely exist to choose
    /// among (a modifier has exactly one output port already toggled
    /// directly by 'c', see `toggle_connect`). Building up a selection
    /// this way lets several ports -- even across different input nodes --
    /// be armed together in one 'c' press instead of one at a time.
    pub fn toggle_port_selection(&mut self) {
        let Focus::Input(i) = self.focus else { return };
        let Some(node) = self.graph.inputs.get(i) else { return };
        if node.streams.get(self.row_idx).is_none() {
            return;
        }
        let ep = Endpoint::Stream { node: node.id, stream_idx: self.row_idx };
        self.selection_anchor = None; // an explicit single toggle isn't part of a range
        if !self.selected.insert(ep) {
            self.selected.remove(&ep);
        }
    }

    /// Shift+Up/Down: extend the pending selection as a contiguous range
    /// from wherever it started to the row now under the cursor -- the
    /// same anchor-then-extend model a text editor uses for shift-click
    /// range selection. Recomputes the range from the anchor on every
    /// press (rather than incrementally toggling) so shrinking it back
    /// (Shift+Down then Shift+Up) correctly un-selects rows outside the
    /// new range instead of leaving stale selections behind; only this
    /// node's own rows are touched, so a range already picked on a
    /// different input node is left alone.
    pub fn extend_port_selection(&mut self, forward: bool) {
        let Focus::Input(i) = self.focus else { return };
        let Some(node) = self.graph.inputs.get(i) else { return };
        let len = node.streams.len();
        if len == 0 {
            return;
        }
        let node_id = node.id;
        let anchor = *self.selection_anchor.get_or_insert(self.row_idx);
        self.row_idx = if forward { (self.row_idx + 1).min(len - 1) } else { self.row_idx.saturating_sub(1) };

        let (lo, hi) = if anchor <= self.row_idx { (anchor, self.row_idx) } else { (self.row_idx, anchor) };
        self.selected.retain(|ep| !matches!(ep, Endpoint::Stream { node, .. } if *node == node_id));
        for idx in lo..=hi {
            self.selected.insert(Endpoint::Stream { node: node_id, stream_idx: idx });
        }
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
    /// file, an output, or any modifier kind. Input/output aren't
    /// structurally special enough to deserve their own dedicated keys (that
    /// used to be 'a' + 'O', separate from 'm' for modifiers); this is one
    /// list for "add a node," full stop.
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

    /// Opens the extra-ffmpeg-args picker (the advanced escape hatch for
    /// options the node graph doesn't model, e.g. `-itsoffset 2.5` on an
    /// input, `-max_interleave_delta 5000000` on an output) for a given
    /// input or output.
    fn open_extra_args_picker(&mut self, target: ExtraArgsTarget) {
        self.mode = Mode::Picker {
            kind: PickerKind::ExtraArgField { target },
            title: "extra ffmpeg args: choose flag".to_string(),
            options: extra_args_picker_options(&self.graph, target),
            selected: 0,
            query: String::new(),
            searching: false,
        };
    }

    /// Opens the chapter list for a `ChapterEdit` modifier: one row per
    /// existing chapter, plus "add chapter..." and, if something's wired
    /// into this node's input, "import from connected input...".
    fn open_chapter_list_picker(&mut self, modifier: NodeId) {
        self.mode = Mode::Picker {
            kind: PickerKind::ChapterList { modifier },
            title: "chapters".to_string(),
            options: chapter_list_picker_options(&self.graph, modifier),
            selected: 0,
            query: String::new(),
            searching: false,
        };
    }

    /// Opens the field editor (start/end/title/delete) for one chapter.
    /// No-op if `index` is out of range (e.g. the chapter was just
    /// deleted).
    fn open_chapter_field_picker(&mut self, modifier: NodeId, index: usize) {
        let Some(chapter) = chapter_edit_chapters(&self.graph, modifier).and_then(|cs| cs.get(index)) else {
            return;
        };
        let options = vec![
            PickerEntry {
                display: format!("start: {}", crate::graph::format_time(chapter.start_secs)),
                value: Some("start".to_string()),
            },
            PickerEntry {
                display: format!("end: {}", crate::graph::format_time(chapter.end_secs)),
                value: Some("end".to_string()),
            },
            PickerEntry { display: format!("title: {}", chapter.title), value: Some("title".to_string()) },
            PickerEntry { display: "delete this chapter".to_string(), value: Some("delete".to_string()) },
        ];
        self.mode = Mode::Picker {
            kind: PickerKind::ChapterField { modifier, index },
            title: "edit chapter".to_string(),
            options,
            selected: 0,
            query: String::new(),
            searching: false,
        };
    }


    /// The stream kind flowing into a modifier, if it's connected --
    /// traced back through however many other modifiers sit upstream.
    fn modifier_input_kind(&self, modifier_id: NodeId) -> Option<StreamKind> {
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
    fn endpoint_stream_kind(&self, ep: Endpoint) -> Option<StreamKind> {
        match ep {
            Endpoint::Stream { node, stream_idx } => {
                self.graph.input(node)?.streams.get(stream_idx).map(|s| s.kind)
            }
            Endpoint::ModifierOut(mid) => match &self.graph.modifier(mid)?.kind {
                ModifierKind::ChapterEdit { .. } => Some(StreamKind::Chapter),
                _ => {
                    let resolved = self.graph.resolve(ep)?;
                    let input = self.graph.input(resolved.from_node)?;
                    input.streams.get(resolved.from_stream_idx).map(|s| s.kind)
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
                self.open_chapter_list_picker(mid);
            }
        }
    }

    pub fn cancel_text_input(&mut self) {
        self.mode = Mode::Normal;
    }

    pub fn confirm_text_input(&mut self) {
        let Mode::TextInput { target, input, .. } =
            std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            return;
        };
        let buffer = input.value().to_string();
        match target {
            TextTarget::NewInputPath => {
                let path = clean_path_input(&buffer);
                if path.is_empty() {
                    return;
                }
                match ffmpeg::probe(&path) {
                    Ok(result) => {
                        let chapter_count = result.chapters.len();
                        let id = self.graph.add_input(path.clone(), result.streams, result.chapters);
                        self.log.push(format!("added input: {path}"));
                        if chapter_count > 0 {
                            self.log.push(format!("found {chapter_count} chapter(s) in {path}"));
                        }
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
                    && let Some(node) = self.graph.output_mut(output_id)
                {
                    node.path = path;
                }
            }
            TextTarget::ModifierMetadataValue { modifier, key } => {
                let value = buffer.trim().to_string();
                if let Some(m) = self.graph.modifier_mut(modifier)
                    && let ModifierKind::Metadata { fields } = &mut m.kind
                {
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
                        ModifierKind::Convert(_)
                        | ModifierKind::Disposition { .. }
                        | ModifierKind::Filter { .. }
                        | ModifierKind::ChapterEdit { .. } => None,
                    })
                    .unwrap_or_default();
                self.mode = text_input_mode(TextTarget::ModifierMetadataValue { modifier, key }, current, Vec::new());
            }
            TextTarget::ModifierFilterValue { modifier, key } => {
                let value = buffer.trim().to_string();
                if let Some(m) = self.graph.modifier_mut(modifier)
                    && let ModifierKind::Filter { fields, .. } = &mut m.kind
                {
                    if value.is_empty() {
                        fields.remove(&key);
                        self.log.push(format!("{key} cleared"));
                    } else {
                        fields.insert(key.clone(), value.clone());
                        self.log.push(format!("{key} set to {value}"));
                    }
                }
            }
            TextTarget::ExtraArgValue { target, key } => {
                let value = buffer.trim().to_string();
                if let Some(fields) = extra_args_of_mut(&mut self.graph, target) {
                    if value.is_empty() {
                        fields.remove(&key);
                        self.log.push(format!("{key} cleared"));
                    } else {
                        fields.insert(key.clone(), value.clone());
                        self.log.push(format!("{key} set to {value}"));
                    }
                }
            }
            TextTarget::ExtraArgCustomKey(target) => {
                let key = buffer.trim().to_string();
                if key.is_empty() {
                    return;
                }
                let current =
                    extra_args_of(&self.graph, target).and_then(|f| f.get(&key).cloned()).unwrap_or_default();
                self.mode = text_input_mode(TextTarget::ExtraArgValue { target, key }, current, Vec::new());
            }
            TextTarget::ChapterTime { modifier, index, field } => {
                match crate::graph::parse_time(&buffer) {
                    Some(secs) => {
                        if let Some(chapter) = chapter_edit_chapters_mut(&mut self.graph, modifier)
                            .and_then(|cs| cs.get_mut(index))
                        {
                            match field {
                                ChapterTimeField::Start => chapter.start_secs = secs,
                                ChapterTimeField::End => chapter.end_secs = secs,
                            }
                        }
                        self.log.push(format!("chapter time set to {}", crate::graph::format_time(secs)));
                    }
                    None => {
                        self.log.push(format!(
                            "couldn't parse '{}' as a time -- try seconds (12.5) or HH:MM:SS",
                            buffer.trim()
                        ));
                    }
                }
                // Return to the field editor either way, so a mistyped
                // time doesn't lose the user's place in the chapter.
                self.open_chapter_field_picker(modifier, index);
            }
            TextTarget::ChapterTitle { modifier, index } => {
                if let Some(chapter) =
                    chapter_edit_chapters_mut(&mut self.graph, modifier).and_then(|cs| cs.get_mut(index))
                {
                    chapter.title = buffer.trim().to_string();
                }
                self.log.push("chapter title set".to_string());
                self.open_chapter_field_picker(modifier, index);
            }
        }
    }

    /// Forwards a key event straight to `tui_input`'s own key handling --
    /// covers typing, Backspace/Delete, Left/Right, Home/End, and (if ever
    /// bound in `main.rs`) word-jump/kill-line, all via its
    /// `to_input_request` mapping -- then refreshes path suggestions if
    /// this field is one that offers them. `main.rs` routes every
    /// `TextInput`-mode key here except Enter/Esc/Tab/Up/Down, which
    /// `to_input_request` leaves unmapped anyway (they're mode transitions
    /// and suggestion-list navigation, not text edits).
    pub fn text_input_handle_key(&mut self, key: KeyEvent) {
        if let Mode::TextInput { target, input, suggestions, selected } = &mut self.mode {
            input.handle_event(&CrosstermEvent::Key(key));
            if matches!(target, TextTarget::NewInputPath | TextTarget::OutputPath(_)) {
                *suggestions = path_suggestions(input.value());
                *selected = 0;
            }
        }
    }

    /// Up/Down while typing a path: move the highlighted suggestion.
    pub fn text_input_move_suggestion(&mut self, delta: isize) {
        if let Mode::TextInput {
            suggestions,
            selected,
            ..
        } = &mut self.mode
        {
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
        if let Mode::TextInput {
            input,
            suggestions,
            selected,
            ..
        } = &mut self.mode
            && let Some(chosen) = suggestions.get(*selected).cloned()
        {
            *input = Input::new(chosen);
            *suggestions = path_suggestions(input.value());
            *selected = 0;
        }
    }

    /// 'c': on an input, arm the whole pending selection at once if
    /// there's one (see `toggle_port_selection`/`extend_port_selection`),
    /// else just toggle the single hovered stream in or out of `armed`
    /// (the original, low-friction one-at-a-time behavior). On a
    /// modifier, arm/disarm its own single output, or -- if something
    /// else is armed -- wire it in (rejecting more than one, since a
    /// modifier's input only ever holds one wire). On an output, connect
    /// every currently-armed source in one action.
    pub fn toggle_connect(&mut self) {
        match self.focus {
            Focus::Input(i) => {
                let Some(node) = self.graph.inputs.get(i) else {
                    return;
                };
                if !self.selected.is_empty() {
                    let n = self.selected.len();
                    self.armed.append(&mut self.selected);
                    self.log.push(format!(
                        "armed {n} port(s) — focus a modifier or output, press 'c' to connect"
                    ));
                    return;
                }
                let Some(stream) = node.streams.get(self.row_idx) else {
                    return;
                };
                let ep = Endpoint::Stream {
                    node: node.id,
                    stream_idx: self.row_idx,
                };
                if !self.armed.insert(ep) {
                    self.armed.remove(&ep); // was already armed -- toggle off
                } else {
                    self.log.push(format!(
                        "armed {} from {} — focus a modifier or output, press 'c' to connect",
                        stream.label(),
                        node.path
                    ));
                }
            }
            Focus::Modifier(i) => {
                let Some(m) = self.graph.modifiers.get(i) else {
                    return;
                };
                let mid = m.id;
                let this_output = Endpoint::ModifierOut(mid);
                if self.armed.is_empty() {
                    self.armed.insert(this_output);
                    self.log.push(
                        "armed this node's output — focus the next node, press 'c' to connect"
                            .to_string(),
                    );
                } else if self.armed.len() == 1 && self.armed.contains(&this_output) {
                    self.armed.remove(&this_output); // disarm
                } else if self.armed.len() > 1 {
                    self.log.push(format!(
                        "can't connect {} streams to a modifier -- it only accepts one",
                        self.armed.len()
                    ));
                } else {
                    let source = *self.armed.iter().next().expect("checked non-empty above");
                    self.graph.connect(source, Target::ModifierIn(mid));
                    sync_chapter_edit_import(&mut self.graph, mid);
                    self.armed.clear();
                    self.log.push("connected".to_string());
                }
            }
            Focus::Output(i) => {
                let Some(output_id) = self.graph.outputs.get(i).map(|o| o.id) else {
                    return;
                };
                if self.armed.is_empty() {
                    self.log.push(
                        "nothing armed -- arm a stream or modifier output first ('c')".to_string(),
                    );
                    return;
                }
                let n = self.armed.len();
                for source in std::mem::take(&mut self.armed) {
                    let target = if self.endpoint_stream_kind(source) == Some(StreamKind::Chapter) {
                        Target::OutputChapters(output_id)
                    } else {
                        Target::Output(output_id)
                    };
                    self.graph.connect(source, target);
                }
                self.log.push(if n == 1 {
                    "connected to output".to_string()
                } else {
                    format!("connected {n} ports to output")
                });
            }
        }
    }

    /// 'd': on an input port, disconnect it from everything downstream --
    /// or, with a pending selection (see `toggle_port_selection`/
    /// `extend_port_selection`), disconnect *every* selected port from
    /// everything downstream in one action, the same way 'c' arms them all
    /// at once, and regardless of which input node is currently focused
    /// (the selection is global, same as `armed`). On a modifier,
    /// disconnect just the selected outgoing connection; on an output,
    /// disconnect just the selected incoming connection.
    pub fn disconnect_focused(&mut self) {
        match self.focus {
            Focus::Input(i) => {
                let Some(node) = self.graph.inputs.get(i) else {
                    return;
                };
                if !self.selected.is_empty() {
                    let ports = std::mem::take(&mut self.selected);
                    let n = ports.len();
                    let affected = chapter_edit_modifiers_fed_by(&self.graph, |w| ports.contains(&w.from));
                    let before = self.graph.wires.len();
                    self.graph.wires.retain(|w| !ports.contains(&w.from));
                    let removed = before != self.graph.wires.len();
                    self.log.push(if removed {
                        format!("disconnected {n} port(s) from everything downstream")
                    } else {
                        format!("{n} port(s) had nothing connected")
                    });
                    for mid in affected {
                        sync_chapter_edit_import(&mut self.graph, mid);
                    }
                    return;
                }
                let Some(stream) = node.streams.get(self.row_idx) else {
                    return;
                };
                let ep = Endpoint::Stream {
                    node: node.id,
                    stream_idx: self.row_idx,
                };
                let label = stream.label();
                let affected = chapter_edit_modifiers_fed_by(&self.graph, |w| w.from == ep);
                let before = self.graph.wires.len();
                self.graph.wires.retain(|w| w.from != ep);
                if self.graph.wires.len() != before {
                    self.log
                        .push(format!("disconnected {label} from everything downstream"));
                }
                for mid in affected {
                    sync_chapter_edit_import(&mut self.graph, mid);
                }
            }
            Focus::Modifier(i) => {
                let Some(m) = self.graph.modifiers.get(i) else {
                    return;
                };
                let ep = Endpoint::ModifierOut(m.id);
                let outgoing = self.graph.outgoing(ep);
                let Some(&wi) = outgoing.get(self.row_idx) else {
                    return;
                };
                self.graph.remove_wire_at(wi);
                self.log.push("disconnected".to_string());
                let new_len = self.graph.outgoing(ep).len();
                if new_len > 0 && self.row_idx >= new_len {
                    self.row_idx = new_len - 1;
                }
            }
            Focus::Output(i) => {
                let Some(output_id) = self.graph.outputs.get(i).map(|o| o.id) else {
                    return;
                };
                let incoming = self.graph.incoming(Target::Output(output_id));
                // The chapters slot is always one more row after the
                // mapped-stream rows (see `cycle_row`'s Output arm).
                if self.row_idx >= incoming.len() {
                    let chapter_wires = self.graph.incoming(Target::OutputChapters(output_id));
                    if let Some(&wi) = chapter_wires.first() {
                        self.graph.remove_wire_at(wi);
                        self.log.push("chapters disconnected".to_string());
                    }
                    return;
                }
                let wi = incoming[self.row_idx];
                self.graph.remove_wire_at(wi);
                self.log.push("disconnected".to_string());
                let new_len = self.graph.incoming(Target::Output(output_id)).len();
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
            self.log
                .push("focus an output node first, then 'f' picks its container".to_string());
            return;
        };
        let Some(output) = self.graph.outputs.get(i) else {
            return;
        };
        let output_id = output.id;
        let current = output.container.clone();

        let mut names: Vec<String> = COMMON_CONTAINERS
            .iter()
            .map(|(name, _)| name.to_string())
            .collect();
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
        if let Mode::Picker {
            options,
            selected,
            query,
            ..
        } = &mut self.mode
        {
            let len = filtered_indices(options, query).len() as isize;
            if len == 0 {
                return;
            }
            *selected = (*selected as isize + delta).rem_euclid(len) as usize;
        }
    }

    /// '/': start typing a query to filter the picker's options.
    pub fn picker_start_search(&mut self) {
        if let Mode::Picker {
            query,
            searching,
            selected,
            ..
        } = &mut self.mode
        {
            query.clear();
            *searching = true;
            *selected = 0;
        }
    }

    pub fn picker_search_char(&mut self, c: char) {
        if let Mode::Picker {
            query, selected, ..
        } = &mut self.mode
        {
            query.push(c);
            *selected = 0;
        }
    }

    pub fn picker_search_backspace(&mut self) {
        if let Mode::Picker {
            query, selected, ..
        } = &mut self.mode
        {
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
        let Mode::Picker {
            kind,
            title,
            options,
            mut query,
            searching,
            ..
        } = std::mem::replace(&mut self.mode, Mode::Normal)
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
        let Mode::Picker {
            kind,
            title,
            options,
            selected,
            query,
            searching,
        } = std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            return;
        };
        let real_idx = filtered_indices(&options, &query).get(selected).copied();

        // Unlike every other picker kind, toggling a disposition flag
        // doesn't close the picker -- it's a multi-select, so Enter here
        // just flips the flag and redraws the same list with its checkbox
        // updated, leaving the user in the picker to toggle more.
        if let PickerKind::DispositionFlags { modifier } = kind {
            if let Some(flag) = real_idx
                .and_then(|i| options.get(i))
                .and_then(|e| e.value.clone())
                && let Some(m) = self.graph.modifier_mut(modifier)
                && let ModifierKind::Disposition { flags } = &mut m.kind
            {
                if !flags.remove(&flag) {
                    flags.insert(flag.clone());
                }
                self.log.push(format!("{flag} toggled"));
            }
            let options = match self.graph.modifier(modifier).map(|m| &m.kind) {
                Some(ModifierKind::Disposition { flags }) => disposition_picker_options(flags),
                _ => Vec::new(),
            };
            self.mode = Mode::Picker {
                kind: PickerKind::DispositionFlags { modifier },
                title,
                options,
                selected,
                query,
                searching,
            };
            return;
        }

        // A curated valueless extra-arg key (e.g. "shortest", "re") toggles
        // in place, same idea as a disposition flag -- everything else
        // (a value-taking curated key, an already-set custom key, or
        // "custom key..." itself) falls through to the normal match below,
        // which opens a value text input instead.
        if let PickerKind::ExtraArgField { target } = kind {
            let selected_key = real_idx.and_then(|i| options.get(i)).and_then(|e| e.value.clone());
            let is_boolean = selected_key
                .as_deref()
                .is_some_and(|k| curated_extra_arg_keys(target).iter().any(|&(ck, b)| ck == k && b));
            if is_boolean {
                let key = selected_key.unwrap();
                if let Some(fields) = extra_args_of_mut(&mut self.graph, target) {
                    if fields.remove(&key).is_some() {
                        self.log.push(format!("{key} disabled"));
                    } else {
                        fields.insert(key.clone(), String::new());
                        self.log.push(format!("{key} enabled"));
                    }
                }
                self.mode = Mode::Picker {
                    kind: PickerKind::ExtraArgField { target },
                    title,
                    options: extra_args_picker_options(&self.graph, target),
                    selected,
                    query,
                    searching,
                };
                return;
            }
        }

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
                let Some(node) = self.graph.output_mut(output) else {
                    return;
                };
                node.container = entry.value.clone();
                match &entry.value {
                    Some(name) => {
                        if let Some((_, ext)) = COMMON_CONTAINERS.iter().find(|(n, _)| n == name) {
                            let stem = std::path::Path::new(&node.path).with_extension("");
                            node.path = format!("{}.{ext}", stem.to_string_lossy());
                        }
                        self.log
                            .push(format!("output container set to {name} ({})", node.path));
                    }
                    None => self.log.push(
                        "output container set to auto (inferred from file extension)".to_string(),
                    ),
                }
            }
            PickerKind::NewNode => {
                match entry.value.as_deref() {
                    Some("input") => {
                        self.start_add_input();
                        return;
                    }
                    Some("output") => {
                        self.add_output_node();
                        return;
                    }
                    _ => {}
                }
                let (kind, name) = match entry.value.as_deref() {
                    Some("metadata") => (
                        ModifierKind::Metadata {
                            fields: BTreeMap::new(),
                        },
                        "metadata",
                    ),
                    Some("disposition") => (
                        ModifierKind::Disposition {
                            flags: BTreeSet::new(),
                        },
                        "disposition",
                    ),
                    Some("chapters") => (ModifierKind::ChapterEdit { chapters: Vec::new() }, "chapters"),
                    Some("shift") => (filter_modifier(FilterName::Shift), "shift"),
                    Some("volume") => (filter_modifier(FilterName::Volume), "volume"),
                    Some("scale") => (filter_modifier(FilterName::Scale), "scale"),
                    Some("crop") => (filter_modifier(FilterName::Crop), "crop"),
                    Some("fade") => (filter_modifier(FilterName::Fade), "fade"),
                    Some("rotate") => (filter_modifier(FilterName::Rotate), "rotate"),
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
                            ModifierKind::Convert(_)
                            | ModifierKind::Disposition { .. }
                            | ModifierKind::Filter { .. }
                            | ModifierKind::ChapterEdit { .. } => None,
                        })
                        .unwrap_or_default();
                    self.mode = text_input_mode(TextTarget::ModifierMetadataValue { modifier, key }, current, Vec::new());
                }
                None => {
                    // "custom key..." -- first ask for the key name itself.
                    self.mode = text_input_mode(TextTarget::ModifierCustomKey(modifier), String::new(), Vec::new());
                }
            },
            PickerKind::DispositionFlags { .. } => {
                unreachable!("handled above before `entry` is computed")
            }
            PickerKind::FilterField { modifier } => {
                // No "custom key..." entry for Filter fields (see
                // field_picker_options), so entry.value is always Some.
                let Some(key) = entry.value else { return };
                let Some(ModifierKind::Filter { name, fields }) =
                    self.graph.modifier(modifier).map(|m| &m.kind)
                else {
                    return;
                };
                let current = fields.get(&key).cloned();

                // A field with a fixed set of valid values (e.g. Rotate's
                // "direction") gets a selection instead of free text --
                // anything else typed there is simply invalid, not just an
                // unusual choice.
                if let Some(values) = name.value_options(&key) {
                    let options =
                        picker_options("(not set)", values.iter().map(|v| v.to_string()).collect());
                    let selected = selected_index(&options, current.as_deref());
                    self.mode = Mode::Picker {
                        kind: PickerKind::FilterFieldValue { modifier, key: key.clone() },
                        title: format!("{}: {key}", name.label()),
                        options,
                        selected,
                        query: String::new(),
                        searching: false,
                    };
                    return;
                }

                self.mode = text_input_mode(TextTarget::ModifierFilterValue { modifier, key }, current.unwrap_or_default(), Vec::new());
            }
            PickerKind::FilterFieldValue { modifier, key } => {
                if let Some(m) = self.graph.modifier_mut(modifier)
                    && let ModifierKind::Filter { fields, .. } = &mut m.kind
                {
                    match entry.value {
                        Some(value) => {
                            fields.insert(key.clone(), value.clone());
                            self.log.push(format!("{key} set to {value}"));
                        }
                        None => {
                            fields.remove(&key);
                            self.log.push(format!("{key} cleared"));
                        }
                    }
                }
            }
            PickerKind::ExtraArgField { target } => {
                let current =
                    entry.value.as_ref().and_then(|key| extra_args_of(&self.graph, target).and_then(|f| f.get(key).cloned()));
                match entry.value {
                    Some(key) => {
                        self.mode = text_input_mode(TextTarget::ExtraArgValue { target, key }, current.unwrap_or_default(), Vec::new());
                    }
                    None => {
                        // "custom key..." -- first ask for the key name itself.
                        self.mode = text_input_mode(TextTarget::ExtraArgCustomKey(target), String::new(), Vec::new());
                    }
                }
            }
            PickerKind::ChapterList { modifier } => match entry.value.as_deref() {
                Some("add") => {
                    let index = chapter_edit_chapters(&self.graph, modifier).map_or(0, |cs| cs.len());
                    let start = chapter_edit_chapters(&self.graph, modifier)
                        .and_then(|cs| cs.last())
                        .map_or(0.0, |c| c.end_secs);
                    if let Some(chapters) = chapter_edit_chapters_mut(&mut self.graph, modifier) {
                        chapters.push(Chapter::new(start, start, String::new()));
                    }
                    self.log.push("added chapter".to_string());
                    self.open_chapter_field_picker(modifier, index);
                }
                Some(idx_str) => {
                    if let Ok(index) = idx_str.parse::<usize>() {
                        self.open_chapter_field_picker(modifier, index);
                    }
                }
                None => {}
            },
            PickerKind::ChapterField { modifier, index } => match entry.value.as_deref() {
                Some("start") | Some("end") => {
                    let Some(chapter) = chapter_edit_chapters(&self.graph, modifier).and_then(|cs| cs.get(index))
                    else {
                        return;
                    };
                    let field = if entry.value.as_deref() == Some("start") {
                        ChapterTimeField::Start
                    } else {
                        ChapterTimeField::End
                    };
                    let current = match field {
                        ChapterTimeField::Start => chapter.start_secs,
                        ChapterTimeField::End => chapter.end_secs,
                    };
                    self.mode = text_input_mode(TextTarget::ChapterTime { modifier, index, field }, crate::graph::format_time(current), Vec::new());
                }
                Some("title") => {
                    let current = chapter_edit_chapters(&self.graph, modifier)
                        .and_then(|cs| cs.get(index))
                        .map(|c| c.title.clone());
                    self.mode = text_input_mode(TextTarget::ChapterTitle { modifier, index }, current.unwrap_or_default(), Vec::new());
                }
                Some("delete") => {
                    if let Some(chapters) = chapter_edit_chapters_mut(&mut self.graph, modifier)
                        && index < chapters.len()
                    {
                        chapters.remove(index);
                        self.log.push("chapter deleted".to_string());
                    }
                    self.open_chapter_list_picker(modifier);
                }
                _ => {}
            },
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

    /// Writes each `ChapterEdit` modifier's chapter list (if it has one)
    /// out as a temp FFMETADATA file, keyed by that node's id, ready to
    /// hand to `Graph::build_ffmpeg_args`/`build_preview_args` as an extra
    /// input -- an output whose chapters trace straight back to a real
    /// input file (no `ChapterEdit` node in the chain) needs no file at
    /// all, since `-map_chapters` can just point at that input directly
    /// (see `Graph::resolve_chapters`). `Graph` itself does no file I/O, so
    /// this lives here rather than there. A write failure just skips that
    /// node's chapters (logged) instead of blocking the whole render --
    /// consistent with how a filtergraph error or similar isn't treated as
    /// fatal until ffmpeg itself reports it.
    fn write_chapter_files(&mut self) -> BTreeMap<NodeId, String> {
        let mut files = BTreeMap::new();
        for modifier in &self.graph.modifiers {
            let ModifierKind::ChapterEdit { chapters } = &modifier.kind else { continue };
            if chapters.is_empty() {
                continue;
            }
            let path = std::env::temp_dir().join(format!("tff-chapters-{}.ffmeta", modifier.id));
            let content = crate::graph::chapters_ffmetadata(chapters);
            match std::fs::write(&path, content) {
                Ok(()) => {
                    files.insert(modifier.id, path.to_string_lossy().into_owned());
                }
                Err(e) => {
                    self.log.push(format!("couldn't write chapter metadata: {e}"));
                }
            }
        }
        files
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
        let chapter_files = self.write_chapter_files();
        let (tx, rx): (Sender<String>, Receiver<String>) = mpsc::channel();
        self.rx = Some(rx);
        self.running = true;
        self.status = "running ffmpeg...".to_string();
        let args = self.graph.build_ffmpeg_args(&chapter_files);
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
            self.log.push(
                "already running ffmpeg — wait for it to finish before previewing".to_string(),
            );
            return;
        }
        let Focus::Output(i) = self.focus else {
            self.log
                .push("focus an output node first, then 'p' previews it".to_string());
            return;
        };
        let Some(output) = self.graph.outputs.get(i) else {
            return;
        };
        let output_id = output.id;
        let ext = std::path::Path::new(&output.path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mkv");
        let preview_path = std::env::temp_dir()
            .join(format!("tff-preview-{output_id}.{ext}"))
            .to_string_lossy()
            .into_owned();

        let chapter_files = self.write_chapter_files();
        let Some(args) = self
            .graph
            .build_preview_args(output_id, &preview_path, PREVIEW_SECONDS, &chapter_files)
        else {
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
                    self.preview_ready = Some(path);
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
        && let Ok(home) = std::env::var("HOME")
    {
        return home;
    }
    s.to_string()
}

/// Builds a `Mode::TextInput` with the cursor placed at the end of
/// `buffer` -- the natural starting position whether the field opens empty
/// or prefilled with an existing value (e.g. re-editing a metadata field).
fn text_input_mode(target: TextTarget, buffer: String, suggestions: Vec<String>) -> Mode {
    Mode::TextInput { target, input: Input::new(buffer), suggestions, selected: 0 }
}

/// The byte offset in `s` where its `char_idx`-th character starts --
/// `tui_input::Input::cursor()` returns a char index (safe for multi-byte
/// UTF-8), but rendering the cursor (see `ui::draw_status_line`) needs a
/// byte offset to split the string around it. `char_idx ==
/// s.chars().count()` (one past the last character, the end-of-buffer
/// position) falls through to `s.len()`, since there's no char at that
/// index to report a start byte for.
pub(crate) fn char_byte_offset(s: &str, char_idx: usize) -> usize {
    s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(s.len())
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

    let scan_target = if dir_part.is_empty() {
        ".".to_string()
    } else {
        expand_tilde(dir_part)
    };
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
    options.extend(names.into_iter().map(|n| PickerEntry {
        display: n.clone(),
        value: Some(n),
    }));
    options
}

fn selected_index(options: &[PickerEntry], current: Option<&str>) -> usize {
    match current {
        None => 0,
        Some(name) => options
            .iter()
            .position(|o| o.value.as_deref() == Some(name))
            .unwrap_or(0),
    }
}

/// Shared by Metadata and Filter nodes: one entry per curated key, showing
/// its current value or "(not set)". `allow_custom` additionally lists any
/// already-set key outside the curated list (reachable only via Metadata's
/// "custom key..." escape hatch) plus that escape hatch itself -- a Filter
/// node's parameter set is fixed, so it never needs one.
fn filter_modifier(name: FilterName) -> ModifierKind {
    ModifierKind::Filter { name, fields: BTreeMap::new() }
}

fn field_picker_options(fields: &BTreeMap<String, String>, keys: &[&str], allow_custom: bool) -> Vec<PickerEntry> {
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
    if allow_custom {
        for (k, v) in fields {
            if !keys.contains(&k.as_str()) {
                options.push(PickerEntry { display: format!("{k}: {v}"), value: Some(k.clone()) });
            }
        }
        options.push(PickerEntry { display: "custom key…".to_string(), value: None });
    }
    options
}

/// The disposition picker's option list: one entry per curated flag, with a
/// checkbox reflecting whether it's currently set on this node -- rebuilt
/// after every toggle so the display stays in sync.
fn disposition_picker_options(flags: &BTreeSet<String>) -> Vec<PickerEntry> {
    crate::graph::disposition_flags()
        .iter()
        .map(|f| {
            let mark = if flags.contains(*f) { "x" } else { " " };
            PickerEntry {
                display: format!("[{mark}] {f}"),
                value: Some((*f).to_string()),
            }
        })
        .collect()
}

fn curated_extra_arg_keys(target: ExtraArgsTarget) -> &'static [(&'static str, bool)] {
    match target {
        ExtraArgsTarget::Input(_) => crate::graph::input_extra_arg_keys(),
        ExtraArgsTarget::Output(_) => crate::graph::output_extra_arg_keys(),
    }
}

fn extra_args_of(graph: &Graph, target: ExtraArgsTarget) -> Option<&BTreeMap<String, String>> {
    match target {
        ExtraArgsTarget::Input(id) => graph.input(id).map(|n| &n.extra_args),
        ExtraArgsTarget::Output(id) => graph.output(id).map(|n| &n.extra_args),
    }
}

fn extra_args_of_mut(graph: &mut Graph, target: ExtraArgsTarget) -> Option<&mut BTreeMap<String, String>> {
    match target {
        ExtraArgsTarget::Input(id) => graph.input_mut(id).map(|n| &mut n.extra_args),
        ExtraArgsTarget::Output(id) => graph.output_mut(id).map(|n| &mut n.extra_args),
    }
}

/// The extra-args picker's option list: one entry per curated key --
/// a `[x]`/`[ ]` checkbox for a valueless switch flag (toggled in place),
/// or "key: value"/"key: (not set)" for one that takes an operand -- plus
/// any already-set custom key outside the curated list and the "custom
/// key..." escape hatch itself, mirroring `field_picker_options`.
fn extra_args_picker_options(graph: &Graph, target: ExtraArgsTarget) -> Vec<PickerEntry> {
    let empty = BTreeMap::new();
    let fields = extra_args_of(graph, target).unwrap_or(&empty);
    let curated = curated_extra_arg_keys(target);

    let mut options: Vec<PickerEntry> = curated.iter().map(|&(key, is_boolean)| {
        let display = if is_boolean {
            let mark = if fields.contains_key(key) { "x" } else { " " };
            format!("[{mark}] {key}")
        } else {
            match fields.get(key) {
                Some(v) => format!("{key}: {v}"),
                None => format!("{key}: (not set)"),
            }
        };
        PickerEntry { display, value: Some(key.to_string()) }
    }).collect();
    for (k, v) in fields {
        if !curated.iter().any(|&(ck, _)| ck == k) {
            options.push(PickerEntry { display: format!("{k}: {v}"), value: Some(k.clone()) });
        }
    }
    options.push(PickerEntry { display: "custom key…".to_string(), value: None });
    options
}

/// A `ChapterEdit` modifier's own chapter list, if `id` refers to one.
fn chapter_edit_chapters(graph: &Graph, id: NodeId) -> Option<&Vec<Chapter>> {
    match graph.modifier(id).map(|m| &m.kind) {
        Some(ModifierKind::ChapterEdit { chapters }) => Some(chapters),
        _ => None,
    }
}

fn chapter_edit_chapters_mut(graph: &mut Graph, id: NodeId) -> Option<&mut Vec<Chapter>> {
    match graph.modifier_mut(id).map(|m| &mut m.kind) {
        Some(ModifierKind::ChapterEdit { chapters }) => Some(chapters),
        _ => None,
    }
}

/// The chapters connected to a `ChapterEdit` node's input, if its single
/// incoming wire resolves to a chapter-kind source -- used by
/// `sync_chapter_edit_import` to keep that node's auto-imported entries in
/// sync with whatever's currently wired in.
fn connected_input_chapters(graph: &Graph, modifier: NodeId) -> Option<Vec<Chapter>> {
    let wire = graph.wires.iter().find(|w| w.to == Target::ModifierIn(modifier))?;
    let Endpoint::Stream { node, stream_idx } = wire.from else { return None };
    let input = graph.input(node)?;
    (input.streams.get(stream_idx)?.kind == StreamKind::Chapter).then(|| input.chapters.clone())
}

/// Reconciles a `ChapterEdit` node's auto-imported entries against
/// whatever's *currently* wired into its input: strips every entry
/// previously tagged `imported` (see `graph::Chapter::imported`'s doc
/// comment), then, if a chapter-kind source is connected, re-imports its
/// current chapters fresh, merging them in alongside whatever manually
/// added entries remain untouched.
///
/// Idempotent and safe to call any time this node's own incoming wire
/// might have changed -- covers connecting a new source, reconnecting to a
/// different one (a modifier's input only ever holds one wire, so this
/// naturally replaces the old imported set with the new one), and
/// disconnecting entirely (nothing to re-import, so the old set is simply
/// removed). No-op if `id` isn't a `ChapterEdit` node. Deliberately *not*
/// called on every graph mutation -- only from the handful of call sites
/// that can actually change *this* node's own incoming wire -- since
/// running it unconditionally would silently discard a user's edits to an
/// already-imported chapter the next time anything else in the graph
/// happened to change.
fn sync_chapter_edit_import(graph: &mut Graph, id: NodeId) {
    let Some(chapters) = chapter_edit_chapters_mut(graph, id) else { return };
    chapters.retain(|c| !c.imported);
    if let Some(imported) = connected_input_chapters(graph, id) {
        let Some(chapters) = chapter_edit_chapters_mut(graph, id) else { return };
        chapters.extend(imported.into_iter().map(|c| Chapter::imported(c.start_secs, c.end_secs, c.title)));
    }
}

/// The `ChapterEdit` modifier ids fed by any wire matching `predicate`,
/// collected *before* a bulk wire removal so the affected nodes' imported
/// chapters can be resynced afterward via `sync_chapter_edit_import` --
/// removing the wire first would make it impossible to tell which nodes
/// were affected.
fn chapter_edit_modifiers_fed_by(graph: &Graph, predicate: impl Fn(&crate::graph::Wire) -> bool) -> Vec<NodeId> {
    graph
        .wires
        .iter()
        .filter(|w| predicate(w))
        .filter_map(|w| match w.to {
            Target::ModifierIn(mid) if matches!(graph.modifier(mid).map(|m| &m.kind), Some(ModifierKind::ChapterEdit { .. })) => {
                Some(mid)
            }
            _ => None,
        })
        .collect()
}

/// The chapter-list picker's option list: one row per existing chapter
/// (tagged with its index, as a string, so `picker_confirm` can parse it
/// back out) formatted as "start–end  title", plus "add chapter..." and,
/// only if this node's input is actually fed by a chapter-kind source,
/// "import from connected input...". Neither collides with a real index,
/// since indices are always plain digit strings.
fn chapter_list_picker_options(graph: &Graph, modifier: NodeId) -> Vec<PickerEntry> {
    let mut options = Vec::new();
    if let Some(chapters) = chapter_edit_chapters(graph, modifier) {
        for (i, c) in chapters.iter().enumerate() {
            let label = if c.title.is_empty() { "(untitled)" } else { &c.title };
            let imported_tag = if c.imported { " [imported]" } else { "" };
            let display = format!(
                "{}–{}  {label}{imported_tag}",
                crate::graph::format_time(c.start_secs),
                crate::graph::format_time(c.end_secs)
            );
            options.push(PickerEntry { display, value: Some(i.to_string()) });
        }
    }
    options.push(PickerEntry { display: "add chapter…".to_string(), value: Some("add".to_string()) });
    options
}

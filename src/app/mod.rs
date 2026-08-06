mod chapters;
mod connect;
mod navigation;
mod nodes;
mod picker;
mod render;
mod text_input;

pub use chapters::ChapterColumn;
pub use picker::{extra_arg_label, filtered_indices};
pub(crate) use text_input::char_byte_offset;
// Internal callers reach this through `text_input::path_suggestions`
// directly; this re-export exists solely so `tests.rs` can call it as
// `crate::app::path_suggestions`, which -- being outside `#[cfg(test)]` --
// a plain `cargo build` can't see using either path.
#[allow(unused_imports)]
pub use text_input::path_suggestions;

use std::collections::BTreeSet;
use std::sync::mpsc::Receiver;

use crate::ffmpeg;
use crate::graph::{Endpoint, Graph, NodeId, StreamKind};

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
    /// or any modifier kind. Input and output share the same picker list
    /// as every modifier kind, just with two more entries at the top,
    /// rather than each getting its own dedicated add-key.
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
        input: tui_input::Input,
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
    /// A `ChapterEdit` modifier's chapter list, shown as a table (one row
    /// per chapter, columns for start/end/title) navigated directly rather
    /// than through a picker -- Enter on a cell edits it in place, and
    /// Enter on the trailing "add chapter" row (`row == chapters.len()`)
    /// just appends one, so adding a chapter is a single keystroke instead
    /// of a chain of nested menus. Reached via 'e' on a focused
    /// `ChapterEdit` node.
    ChapterTable {
        modifier: NodeId,
        row: usize,
        col: ChapterColumn,
    },
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
    /// How many characters the focused node's title and body text are
    /// scrolled left by, for reading text a fixed-width box would
    /// otherwise truncate (see `scroll_node_text`). A view concern only --
    /// nothing here affects the ffmpeg command built from the graph --
    /// so it lives on `App`, not on the node itself, and resets whenever
    /// focus moves to a different node (see `set_focus_index`).
    pub text_scroll: u16,
    /// The log pane's scroll position: `None` means pinned to the live
    /// bottom (always showing the newest lines, today's default behavior,
    /// auto-following as more get pushed) -- `Some(start)` freezes the view
    /// at that absolute line index instead, so reading old output isn't
    /// disrupted by new lines arriving mid-read. See `scroll_log`.
    pub log_scroll: Option<usize>,
    /// How many columns the log pane's lines are scrolled left by, for
    /// reading a long line (e.g. the full `$ ffmpeg ...` command) a
    /// terminal-width-limited pane would otherwise truncate. Independent
    /// of `log_scroll` (which line-range is shown) -- this is which
    /// horizontal slice of whatever's currently shown. See
    /// `scroll_log_horizontal`.
    pub log_hscroll: u16,
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
            text_scroll: 0,
            log_scroll: None,
            log_hscroll: 0,
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
        self.text_scroll = 0; // scrolling belongs to whichever node was focused, not the one we're leaving
    }
}

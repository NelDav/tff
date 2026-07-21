use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crate::ffmpeg;
use crate::graph::{Codec, Graph, NodeId, StreamKind};

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

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Input(usize),  // index into graph.inputs
    Output(usize), // index into graph.outputs
}

pub enum TextTarget {
    NewInputPath,
    OutputPath(NodeId),
}

pub enum PickerKind {
    /// Index into `Graph::edges`. Safe to hold onto across the picker's
    /// lifetime because Picker mode captures all key input itself, so
    /// nothing else can mutate `edges` (and shift indices) while it's open.
    Codec { edge_idx: usize },
    Container { output: NodeId },
}

pub struct PickerEntry {
    pub display: String,
    /// `None` is the "reset" choice: Copy for a codec picker, "infer from
    /// extension" for the container picker. `Some(name)` is an explicit
    /// ffmpeg encoder/muxer name.
    pub value: Option<String>,
}

pub enum Mode {
    Normal,
    TextInput { target: TextTarget, buffer: String },
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
    /// is focused, or an index into that output's incoming edges (see
    /// `Graph::edge_indices_for_output`) when an output is focused.
    pub row_idx: usize,
    pub mode: Mode,
    /// A stream port the user has armed, waiting to be connected to
    /// whichever output node gets focused next.
    pub armed: Option<(NodeId, usize)>,
    pub log: Vec<String>,
    pub status: String,
    pub running: bool,
    rx: Option<Receiver<String>>,
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
            should_quit: false,
            available_encoders,
            available_muxers,
        }
    }

    fn node_count(&self) -> usize {
        self.graph.inputs.len() + self.graph.outputs.len()
    }

    fn focus_index(&self) -> usize {
        match self.focus {
            Focus::Input(i) => i,
            Focus::Output(i) => self.graph.inputs.len() + i,
        }
    }

    fn set_focus_index(&mut self, idx: usize) {
        let n_inputs = self.graph.inputs.len();
        self.focus = if idx < n_inputs {
            Focus::Input(idx)
        } else {
            Focus::Output((idx - n_inputs).min(self.graph.outputs.len().saturating_sub(1)))
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

    /// Up/Down while a node is focused: cycles the selected stream (input)
    /// or the selected incoming connection (output).
    pub fn cycle_row(&mut self, forward: bool) {
        let len = match self.focus {
            Focus::Input(i) => self.graph.inputs.get(i).map_or(0, |n| n.streams.len()),
            Focus::Output(i) => self
                .graph
                .outputs
                .get(i)
                .map_or(0, |o| self.graph.edge_indices_for_output(o.id).len()),
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
            Focus::Output(i) => self.graph.outputs.get_mut(i).map(|n| &mut n.pos),
        };
        if let Some(pos) = pos {
            pos.0 = (pos.0 + dx * step).max(0.0);
            pos.1 = (pos.1 + dy * step).max(0.0);
        }
    }

    pub fn start_add_input(&mut self) {
        self.mode = Mode::TextInput {
            target: TextTarget::NewInputPath,
            buffer: String::new(),
        };
    }

    /// 'O': add a new output node and focus it.
    pub fn add_output_node(&mut self) {
        self.graph.add_output();
        let idx = self.graph.inputs.len() + self.graph.outputs.len() - 1;
        self.set_focus_index(idx);
        self.log.push("added output node".to_string());
    }

    /// 'o': edit the focused output's path. No-op unless an output is
    /// focused -- there's nothing to edit on an input node.
    pub fn start_edit_output(&mut self) {
        let Focus::Output(i) = self.focus else {
            self.log.push("focus an output node first, then 'o' edits its path".to_string());
            return;
        };
        let Some(output) = self.graph.outputs.get(i) else { return };
        self.mode = Mode::TextInput {
            target: TextTarget::OutputPath(output.id),
            buffer: output.path.clone(),
        };
    }

    pub fn cancel_text_input(&mut self) {
        self.mode = Mode::Normal;
    }

    pub fn confirm_text_input(&mut self) {
        let Mode::TextInput { target, buffer } =
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
        }
    }

    pub fn text_input_char(&mut self, c: char) {
        if let Mode::TextInput { buffer, .. } = &mut self.mode {
            buffer.push(c);
        }
    }

    pub fn text_input_backspace(&mut self) {
        if let Mode::TextInput { buffer, .. } = &mut self.mode {
            buffer.pop();
        }
    }

    /// 'c': arm the focused input stream, or -- when something is armed
    /// and an output node is focused -- connect/disconnect it to that
    /// specific output. ffmpeg can write several output files in one run,
    /// so which output a stream feeds is a real choice once more than one
    /// exists; focusing the destination is how you make that choice.
    pub fn toggle_connect(&mut self) {
        match self.focus {
            Focus::Input(i) => {
                let Some(node) = self.graph.inputs.get(i) else { return };
                let Some(stream) = node.streams.get(self.row_idx) else { return };
                let key = (node.id, self.row_idx);
                if self.armed == Some(key) {
                    self.armed = None; // disarm
                } else {
                    self.log.push(format!(
                        "armed {} from {} — focus an output, press 'c' to connect",
                        stream.label(),
                        node.path
                    ));
                    self.armed = Some(key);
                }
            }
            Focus::Output(i) => {
                let Some(output) = self.graph.outputs.get(i) else { return };
                let output_id = output.id;
                if let Some((node_id, stream_idx)) = self.armed.take() {
                    let was_connected = self.graph.has_edge(node_id, stream_idx, output_id);
                    self.graph.toggle_edge(node_id, stream_idx, output_id);
                    self.log.push(if was_connected {
                        "disconnected".to_string()
                    } else {
                        "connected to output".to_string()
                    });
                }
            }
        }
    }

    /// 'd': on an input port, disconnect it from every output it feeds; on
    /// an output node, disconnect just the selected incoming connection.
    pub fn disconnect_focused(&mut self) {
        match self.focus {
            Focus::Input(i) => {
                let Some(node) = self.graph.inputs.get(i) else { return };
                let Some(stream) = node.streams.get(self.row_idx) else { return };
                let (id, idx) = (node.id, self.row_idx);
                let label = stream.label();
                let before = self.graph.edges.len();
                self.graph.edges.retain(|e| !(e.from_node == id && e.from_stream_idx == idx));
                if self.graph.edges.len() != before {
                    self.log.push(format!("disconnected {label} from all outputs"));
                }
            }
            Focus::Output(i) => {
                let Some(output) = self.graph.outputs.get(i) else { return };
                let output_id = output.id;
                let edge_idxs = self.graph.edge_indices_for_output(output_id);
                let Some(&ei) = edge_idxs.get(self.row_idx) else { return };
                self.graph.remove_edge_at(ei);
                self.log.push("disconnected".to_string());
                let new_len = self.graph.edge_indices_for_output(output_id).len();
                if new_len > 0 && self.row_idx >= new_len {
                    self.row_idx = new_len - 1;
                }
            }
        }
    }

    /// 'e': open a picker listing codecs for a specific connection. On an
    /// input port with exactly one outgoing connection, that's unambiguous;
    /// with more than one (fanned out to several outputs) or none, this
    /// asks the user to be precise via the output side instead. On an
    /// output node, it targets whichever connection row is selected.
    pub fn open_codec_picker(&mut self) {
        let (edge_idx, kind, label) = match self.focus {
            Focus::Input(i) => {
                let Some(node) = self.graph.inputs.get(i) else { return };
                let Some(stream) = node.streams.get(self.row_idx) else { return };
                let (id, idx) = (node.id, self.row_idx);
                let matches: Vec<usize> = self
                    .graph
                    .edges
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.from_node == id && e.from_stream_idx == idx)
                    .map(|(ei, _)| ei)
                    .collect();
                match matches.len() {
                    0 => {
                        self.log
                            .push("connect this stream first ('c'), then 'e' to pick a codec".to_string());
                        return;
                    }
                    1 => (matches[0], stream.kind, stream.label()),
                    _ => {
                        self.log.push(
                            "connected to multiple outputs — focus the specific output row to set its codec"
                                .to_string(),
                        );
                        return;
                    }
                }
            }
            Focus::Output(i) => {
                let Some(output) = self.graph.outputs.get(i) else { return };
                let edge_idxs = self.graph.edge_indices_for_output(output.id);
                let Some(&ei) = edge_idxs.get(self.row_idx) else {
                    self.log.push("nothing mapped to this output yet".to_string());
                    return;
                };
                let edge = &self.graph.edges[ei];
                let Some(input) = self.graph.input(edge.from_node) else { return };
                let Some(stream) = input.streams.get(edge.from_stream_idx) else { return };
                (ei, stream.kind, stream.label())
            }
        };

        let current = self.graph.edges[edge_idx].codec.clone();

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
            kind: PickerKind::Codec { edge_idx },
            title: format!("codec for {label}"),
            options,
            selected,
            query: String::new(),
            searching: false,
        };
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
            PickerKind::Codec { edge_idx } => {
                let codec = match entry.value {
                    None => Codec::Copy,
                    Some(name) => Codec::Encode(name),
                };
                self.log.push(match &codec {
                    Codec::Copy => "codec set to copy (no re-encode)".to_string(),
                    Codec::Encode(_) => format!("codec set to {}", codec.label()),
                });
                self.graph.set_edge_codec_at(edge_idx, codec);
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
        }
    }

    /// 'x': remove the focused node -- an input entirely, or an output as
    /// long as it isn't the last one (ffmpeg needs somewhere to write to).
    pub fn delete_focused_node(&mut self) {
        match self.focus {
            Focus::Input(i) => {
                let Some(node) = self.graph.inputs.get(i) else { return };
                let id = node.id;
                let path = node.path.clone();
                self.graph.remove_input(id);
                self.armed = self.armed.filter(|(n, _)| *n != id);
                self.log.push(format!("removed input: {path}"));
                let n = self.graph.inputs.len();
                self.set_focus_index(i.min(n));
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
                self.set_focus_index((self.graph.inputs.len() + i).min(n.saturating_sub(1)));
            }
        }
    }

    pub fn start_render(&mut self) {
        if self.running {
            return;
        }
        if self.graph.edges.is_empty() {
            self.log.push(
                "nothing mapped yet — arm a stream with 'c', then focus an output and press 'c' again"
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
            self.status = format!("ffmpeg exited with code {code}");
            self.log.push(self.status.clone());
            self.rx = None;
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

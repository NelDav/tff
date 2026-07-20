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
    Input(usize), // index into graph.inputs
    Output,
}

pub enum TextTarget {
    NewInputPath,
    OutputPath,
}

pub enum PickerKind {
    Codec { node: NodeId, stream_idx: usize },
    Container,
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
    pub port_idx: usize,
    pub mode: Mode,
    /// A stream port the user has armed, waiting to be connected to the output.
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
            focus: Focus::Output,
            port_idx: 0,
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
        self.graph.inputs.len() + 1 // + output node
    }

    fn focus_index(&self) -> usize {
        match self.focus {
            Focus::Input(i) => i,
            Focus::Output => self.graph.inputs.len(),
        }
    }

    fn set_focus_index(&mut self, idx: usize) {
        self.focus = if idx >= self.graph.inputs.len() {
            Focus::Output
        } else {
            Focus::Input(idx)
        };
        self.port_idx = 0;
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

    pub fn cycle_port(&mut self, forward: bool) {
        if let Focus::Input(i) = self.focus
            && let Some(node) = self.graph.inputs.get(i) {
                let len = node.streams.len();
                if len == 0 {
                    return;
                }
                self.port_idx = if forward {
                    (self.port_idx + 1) % len
                } else {
                    (self.port_idx + len - 1) % len
                };
            }
    }

    pub fn move_focused_node(&mut self, dx: f64, dy: f64) {
        let step = 2.0;
        match self.focus {
            Focus::Input(i) => {
                if let Some(node) = self.graph.inputs.get_mut(i) {
                    node.pos.0 = (node.pos.0 + dx * step).max(0.0);
                    node.pos.1 = (node.pos.1 + dy * step).max(0.0);
                }
            }
            Focus::Output => {
                self.graph.output.pos.0 = (self.graph.output.pos.0 + dx * step).max(0.0);
                self.graph.output.pos.1 = (self.graph.output.pos.1 + dy * step).max(0.0);
            }
        }
    }

    pub fn start_add_input(&mut self) {
        self.mode = Mode::TextInput {
            target: TextTarget::NewInputPath,
            buffer: String::new(),
        };
    }

    pub fn start_edit_output(&mut self) {
        self.mode = Mode::TextInput {
            target: TextTarget::OutputPath,
            buffer: self.graph.output.path.clone(),
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
            TextTarget::OutputPath => {
                let path = clean_path_input(&buffer);
                if !path.is_empty() {
                    self.graph.output.path = path;
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

    /// 'c': arm the focused stream port, or complete a connection to the output.
    pub fn toggle_connect(&mut self) {
        match self.focus {
            Focus::Input(i) => {
                let Some(node) = self.graph.inputs.get(i) else {
                    return;
                };
                let Some(stream) = node.streams.get(self.port_idx) else {
                    return;
                };
                let key = (node.id, self.port_idx);
                if self.armed == Some(key) {
                    self.armed = None; // disarm
                } else {
                    self.log
                        .push(format!("armed {} from {} — move to output, press 'c'", stream.label(), node.path));
                    self.armed = Some(key);
                }
            }
            Focus::Output => {
                if let Some((node_id, stream_idx)) = self.armed.take() {
                    self.graph.toggle_edge(node_id, stream_idx);
                    self.log.push("connected to output".to_string());
                }
            }
        }
    }

    /// 'd': disconnect the edge on the currently focused port, if any.
    pub fn disconnect_focused(&mut self) {
        if let Focus::Input(i) = self.focus
            && let Some(node) = self.graph.inputs.get(i)
                && let Some(stream) = node.streams.get(self.port_idx) {
                    let (id, idx) = (node.id, self.port_idx);
                    let label = stream.label();
                    if self.graph.is_connected(id, idx) {
                        self.graph.toggle_edge(id, idx);
                        self.log.push(format!("disconnected {label}"));
                    }
                }
    }

    /// 'e': open a picker listing codecs available for the focused port's
    /// connection. No-op on an unconnected port -- codec only matters for
    /// streams actually being muxed into the output.
    pub fn open_codec_picker(&mut self) {
        let Focus::Input(i) = self.focus else { return };
        let Some(node) = self.graph.inputs.get(i) else { return };
        let Some(stream) = node.streams.get(self.port_idx) else { return };
        let (id, idx, kind) = (node.id, self.port_idx, stream.kind);

        if !self.graph.is_connected(id, idx) {
            self.log
                .push("connect this stream first ('c'), then 'e' to pick a codec".to_string());
            return;
        }

        let current = self
            .graph
            .edges
            .iter()
            .find(|e| e.from_node == id && e.from_stream_idx == idx)
            .map(|e| e.codec.clone())
            .unwrap_or(Codec::Copy);

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
            kind: PickerKind::Codec { node: id, stream_idx: idx },
            title: format!("codec for {}", stream.label()),
            options,
            selected,
            query: String::new(),
            searching: false,
        };
    }

    /// 'f': open a picker listing ffmpeg's available output containers.
    pub fn open_container_picker(&mut self) {
        let current = self.graph.output.container.clone();

        let mut names: Vec<String> = COMMON_CONTAINERS.iter().map(|(name, _)| name.to_string()).collect();
        prioritize_and_extend(&mut names, self.available_muxers.iter().map(String::as_str));

        let options = picker_options("auto (infer from file extension)", names);
        let selected = selected_index(&options, current.as_deref());

        self.mode = Mode::Picker {
            kind: PickerKind::Container,
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
            PickerKind::Codec { node, stream_idx } => {
                let codec = match entry.value {
                    None => Codec::Copy,
                    Some(name) => Codec::Encode(name),
                };
                self.log.push(match &codec {
                    Codec::Copy => "codec set to copy (no re-encode)".to_string(),
                    Codec::Encode(_) => format!("codec set to {}", codec.label()),
                });
                self.graph.set_edge_codec(node, stream_idx, codec);
            }
            PickerKind::Container => {
                self.graph.output.container = entry.value.clone();
                match &entry.value {
                    Some(name) => {
                        if let Some((_, ext)) = COMMON_CONTAINERS.iter().find(|(n, _)| n == name) {
                            let stem = std::path::Path::new(&self.graph.output.path).with_extension("");
                            self.graph.output.path = format!("{}.{ext}", stem.to_string_lossy());
                        }
                        self.log
                            .push(format!("output container set to {name} ({})", self.graph.output.path));
                    }
                    None => self
                        .log
                        .push("output container set to auto (inferred from file extension)".to_string()),
                }
            }
        }
    }

    /// 'x': remove the whole focused input node.
    pub fn delete_focused_node(&mut self) {
        if let Focus::Input(i) = self.focus
            && let Some(node) = self.graph.inputs.get(i) {
                let id = node.id;
                let path = node.path.clone();
                self.graph.remove_input(id);
                self.armed = self.armed.filter(|(n, _)| *n != id);
                self.log.push(format!("removed input: {path}"));
                let n = self.graph.inputs.len();
                self.set_focus_index(i.min(n));
            }
    }

    pub fn start_render(&mut self) {
        if self.running {
            return;
        }
        if self.graph.edges.is_empty() {
            self.log
                .push("nothing mapped yet — arm a stream with 'c', connect it to the output".to_string());
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

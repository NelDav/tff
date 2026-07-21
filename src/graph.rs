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

/// A codec choice for one mapped stream: either pass it through unchanged
/// ("stream copy", the fast/lossless default) or re-encode it with a
/// specific ffmpeg encoder name. The name is an owned `String` because the
/// real option list is discovered at runtime from `ffmpeg -encoders`
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

/// A mapping from one input stream to one output file. ffmpeg supports
/// several output files in a single invocation, and the same source stream
/// can be mapped into more than one of them (e.g. a full-quality copy and a
/// re-encoded preview) -- so `to_output` identifies which output this edge
/// feeds, and a given (from_node, from_stream_idx) pair may appear in more
/// than one edge.
#[derive(Clone, Debug)]
pub struct Edge {
    pub from_node: NodeId,
    pub from_stream_idx: usize, // index into InputNode::streams
    pub to_output: NodeId,
    pub codec: Codec,
}

pub struct Graph {
    pub inputs: Vec<InputNode>,
    pub outputs: Vec<OutputNode>,
    pub edges: Vec<Edge>,
    next_id: NodeId,
}

impl Graph {
    pub fn new() -> Self {
        let mut graph = Graph {
            inputs: Vec::new(),
            outputs: Vec::new(),
            edges: Vec::new(),
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
            pos: (48.0, y),
            width: 30,
        });
        id
    }

    pub fn remove_input(&mut self, id: NodeId) {
        self.inputs.retain(|n| n.id != id);
        self.edges.retain(|e| e.from_node != id);
        // Re-derive file_index so ffmpeg -i ordering stays contiguous.
        for (idx, node) in self.inputs.iter_mut().enumerate() {
            node.file_index = idx;
        }
    }

    pub fn remove_output(&mut self, id: NodeId) {
        self.outputs.retain(|n| n.id != id);
        self.edges.retain(|e| e.to_output != id);
    }

    pub fn input(&self, id: NodeId) -> Option<&InputNode> {
        self.inputs.iter().find(|n| n.id == id)
    }

    pub fn output_mut(&mut self, id: NodeId) -> Option<&mut OutputNode> {
        self.outputs.iter_mut().find(|n| n.id == id)
    }

    /// Whether the given stream is mapped to this *specific* output.
    pub fn has_edge(&self, node: NodeId, stream_idx: usize, to_output: NodeId) -> bool {
        self.edges
            .iter()
            .any(|e| e.from_node == node && e.from_stream_idx == stream_idx && e.to_output == to_output)
    }

    /// Indices into `self.edges` for edges feeding a specific output, in
    /// stable order. This is what output-side row navigation/selection
    /// indexes into (a given output's Nth listed connection = edge_idxs[N]).
    pub fn edge_indices_for_output(&self, output_id: NodeId) -> Vec<usize> {
        self.edges
            .iter()
            .enumerate()
            .filter(|(_, e)| e.to_output == output_id)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn toggle_edge(&mut self, node: NodeId, stream_idx: usize, to_output: NodeId) {
        let existing = self
            .edges
            .iter()
            .position(|e| e.from_node == node && e.from_stream_idx == stream_idx && e.to_output == to_output);
        match existing {
            Some(i) => {
                self.edges.remove(i);
            }
            None => self.edges.push(Edge {
                from_node: node,
                from_stream_idx: stream_idx,
                to_output,
                codec: Codec::Copy,
            }),
        }
    }

    pub fn remove_edge_at(&mut self, edge_idx: usize) {
        if edge_idx < self.edges.len() {
            self.edges.remove(edge_idx);
        }
    }

    pub fn set_edge_codec_at(&mut self, edge_idx: usize, codec: Codec) {
        if let Some(edge) = self.edges.get_mut(edge_idx) {
            edge.codec = codec;
        }
    }

    /// Build the `ffmpeg` argument list for the current graph: all inputs
    /// up front, then one output "section" per output node that has at
    /// least one mapped stream (`-map`s, `-c copy` plus any per-stream
    /// overrides, `-f` if an explicit container was chosen, then the output
    /// path) -- mirroring ffmpeg's own multi-output command syntax. Outputs
    /// with no edges are skipped entirely: handing ffmpeg an output path
    /// with no `-map` would trigger its default stream auto-selection,
    /// which isn't what an empty output node means here.
    pub fn build_ffmpeg_args(&self) -> Vec<String> {
        let mut args = vec!["-y".to_string()];
        for input in &self.inputs {
            args.push("-i".to_string());
            args.push(input.path.clone());
        }
        for output in &self.outputs {
            let edge_idxs = self.edge_indices_for_output(output.id);
            if edge_idxs.is_empty() {
                continue;
            }
            for &ei in &edge_idxs {
                let edge = &self.edges[ei];
                if let Some(input) = self.input(edge.from_node)
                    && let Some(stream) = input.streams.get(edge.from_stream_idx) {
                        args.push("-map".to_string());
                        args.push(format!("{}:{}", input.file_index, stream.index));
                    }
            }
            args.push("-c".to_string());
            args.push("copy".to_string());
            // Stream specifiers like -c:0 are scoped to the *current*
            // output section, so the index here is local to this output's
            // own edge list, not a position in the global self.edges.
            for (local_i, &ei) in edge_idxs.iter().enumerate() {
                if let Some(name) = self.edges[ei].codec.ffmpeg_name() {
                    args.push(format!("-c:{local_i}"));
                    args.push(name.to_string());
                }
            }
            if let Some(container) = &output.container {
                args.push("-f".to_string());
                args.push(container.clone());
            }
            args.push(output.path.clone());
        }
        args
    }
}

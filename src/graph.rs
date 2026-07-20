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
    pub path: String,
    /// Explicit muxer override (e.g. "webm", "matroska"), passed to ffmpeg
    /// as `-f <name>`. `None` means "infer from the output path's
    /// extension", ffmpeg's own default behavior.
    pub container: Option<String>,
    pub pos: (f64, f64),
    pub width: u16,
}

/// A mapping from one input stream onto the (single) output file.
#[derive(Clone, Debug)]
pub struct Edge {
    pub from_node: NodeId,
    pub from_stream_idx: usize, // index into InputNode::streams
    pub codec: Codec,
}

pub struct Graph {
    pub inputs: Vec<InputNode>,
    pub output: OutputNode,
    pub edges: Vec<Edge>,
    next_id: NodeId,
}

impl Graph {
    pub fn new() -> Self {
        Graph {
            inputs: Vec::new(),
            output: OutputNode {
                path: "output.mkv".to_string(),
                container: None,
                pos: (48.0, 2.0),
                width: 30,
            },
            edges: Vec::new(),
            next_id: 1,
        }
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

    pub fn remove_input(&mut self, id: NodeId) {
        self.inputs.retain(|n| n.id != id);
        self.edges.retain(|e| e.from_node != id);
        // Re-derive file_index so ffmpeg -i ordering stays contiguous.
        for (idx, node) in self.inputs.iter_mut().enumerate() {
            node.file_index = idx;
        }
    }

    pub fn input(&self, id: NodeId) -> Option<&InputNode> {
        self.inputs.iter().find(|n| n.id == id)
    }

    pub fn is_connected(&self, node: NodeId, stream_idx: usize) -> bool {
        self.edges
            .iter()
            .any(|e| e.from_node == node && e.from_stream_idx == stream_idx)
    }

    pub fn toggle_edge(&mut self, node: NodeId, stream_idx: usize) {
        if self.is_connected(node, stream_idx) {
            self.edges
                .retain(|e| !(e.from_node == node && e.from_stream_idx == stream_idx));
        } else {
            self.edges.push(Edge {
                from_node: node,
                from_stream_idx: stream_idx,
                codec: Codec::Copy,
            });
        }
    }

    pub fn set_edge_codec(&mut self, node: NodeId, stream_idx: usize, codec: Codec) {
        if let Some(edge) = self
            .edges
            .iter_mut()
            .find(|e| e.from_node == node && e.from_stream_idx == stream_idx)
        {
            edge.codec = codec;
        }
    }

    /// Build the `ffmpeg` argument list for the current graph. Streams
    /// default to `-c copy`; edges with a non-Copy codec get a per-output-
    /// stream override (`-c:<i>`), where `<i>` is the edge's position in
    /// `self.edges` -- which is exactly the output stream index, since
    /// `-map` options are emitted in that same order.
    pub fn build_ffmpeg_args(&self) -> Vec<String> {
        let mut args = vec!["-y".to_string()];
        for input in &self.inputs {
            args.push("-i".to_string());
            args.push(input.path.clone());
        }
        for edge in &self.edges {
            if let Some(input) = self.input(edge.from_node)
                && let Some(stream) = input.streams.get(edge.from_stream_idx) {
                    args.push("-map".to_string());
                    args.push(format!("{}:{}", input.file_index, stream.index));
                }
        }
        args.push("-c".to_string());
        args.push("copy".to_string());
        for (i, edge) in self.edges.iter().enumerate() {
            if let Some(name) = edge.codec.ffmpeg_name() {
                args.push(format!("-c:{i}"));
                args.push(name.to_string());
            }
        }
        if let Some(container) = &self.output.container {
            args.push("-f".to_string());
            args.push(container.clone());
        }
        args.push(self.output.path.clone());
        args
    }
}

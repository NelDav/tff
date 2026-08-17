use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::node::FilterName;
use super::stream::{Codec, StreamKind};
use super::NodeId;

/// The source side of a connection: either a specific stream on an input
/// file, or the (single, always-transformed) output of a modifier node.
/// Ordered so a set of these (see `App::armed`/`App::selected`) has a
/// stable iteration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Endpoint {
    Stream { node: NodeId, stream_idx: usize },
    ModifierOut(NodeId),
}

/// The destination side of a connection: a modifier's input slot (a single
/// wire at a time for every kind except `ModifierKind::Concat`, which
/// accepts any number, appended in wire order -- see `Graph::connect`), an
/// output file's mapped-stream list (also any number), or an output's
/// chapters slot (single wire, like an ordinary modifier's input).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Target {
    ModifierIn(NodeId),
    Output(NodeId),
    OutputChapters(NodeId),
}

/// A connection in the graph. Deliberately carries no settings of its own
/// -- all transformation happens in the modifier nodes along the chain a
/// wire is part of, resolved by walking backward from an output (see
/// `Graph::resolve`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wire {
    pub from: Endpoint,
    pub to: Target,
}

/// The result of walking a chain of wires/modifiers back to its ultimate
/// source(s): either a single real stream (`Stream`), or -- once the chain
/// passes through a `ModifierKind::Concat` node -- the join of however many
/// segments feed it (`Concat`), each itself a fully-resolved chain. Either
/// way, `codec`/`metadata`/`disposition`/`filters` are whatever accumulated
/// *downstream* of that source (same accumulation rule as before: metadata
/// merges, filters accumulate in order, codec/disposition are first-wins
/// walking backward from the output) -- a `Concat` node's own segments are
/// each resolved independently and never see settings applied after the
/// concat.
pub enum Resolved {
    Stream {
        from_node: NodeId,
        from_stream_idx: usize,
        codec: Codec,
        metadata: BTreeMap<String, String>,
        disposition: Option<BTreeSet<String>>,
        /// In source-to-output order (the order the filters should actually
        /// be applied), even though this is built up walking backward from
        /// the output.
        filters: Vec<(FilterName, BTreeMap<String, String>)>,
    },
    Concat {
        /// The stream kind shared by every segment -- validated when the
        /// chain was resolved (see `Graph::resolve`), so this is always
        /// consistent with each segment's own resolved kind.
        kind: StreamKind,
        /// In concat order (the order ffmpeg's `concat` filter joins them),
        /// which is also playback order in the joined result.
        segments: Vec<Resolved>,
        codec: Codec,
        metadata: BTreeMap<String, String>,
        disposition: Option<BTreeSet<String>>,
        filters: Vec<(FilterName, BTreeMap<String, String>)>,
    },
}

impl Resolved {
    /// Accessors for the fields both variants carry (whatever was applied
    /// downstream of this resolved source) -- lets a caller that doesn't
    /// care which variant it has (e.g. building `-c`/`-metadata`/
    /// `-disposition` args, which are the same regardless) avoid matching.
    pub fn codec(&self) -> &Codec {
        match self {
            Resolved::Stream { codec, .. } | Resolved::Concat { codec, .. } => codec,
        }
    }

    pub fn metadata(&self) -> &BTreeMap<String, String> {
        match self {
            Resolved::Stream { metadata, .. } | Resolved::Concat { metadata, .. } => metadata,
        }
    }

    pub fn disposition(&self) -> &Option<BTreeSet<String>> {
        match self {
            Resolved::Stream { disposition, .. } | Resolved::Concat { disposition, .. } => disposition,
        }
    }
}

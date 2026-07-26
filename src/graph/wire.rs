use std::collections::{BTreeMap, BTreeSet};

use super::node::FilterName;
use super::stream::Codec;
use super::NodeId;

/// The source side of a connection: either a specific stream on an input
/// file, or the (single, always-transformed) output of a modifier node.
/// Ordered so a set of these (see `App::armed`/`App::selected`) has a
/// stable iteration order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Endpoint {
    Stream { node: NodeId, stream_idx: usize },
    ModifierOut(NodeId),
}

/// The destination side of a connection: a modifier's single input slot,
/// an output file's mapped-stream list (any number of incoming wires), or
/// an output's chapters slot (like `ModifierIn`, only one wire at a time --
/// see `Graph::connect`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    ModifierIn(NodeId),
    Output(NodeId),
    OutputChapters(NodeId),
}

/// A connection in the graph. Deliberately carries no settings of its own
/// -- all transformation happens in the modifier nodes along the chain a
/// wire is part of, resolved by walking backward from an output (see
/// `Graph::resolve`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wire {
    pub from: Endpoint,
    pub to: Target,
}

/// The result of walking a chain of wires/modifiers back to its ultimate
/// source stream: which stream it started from, and the effective codec,
/// metadata, disposition, and filter chain accumulated from every modifier
/// along the way. Metadata fields merge across the whole chain (they're
/// independent named slots), and filters accumulate as an ordered list
/// (each is a distinct pipeline stage, so e.g. two Scale nodes both apply,
/// in chain order) -- but codec and disposition are each an all-or-nothing
/// setting for the stream, so whichever modifier sets one first walking
/// backward -- i.e. closest to the output -- wins outright, matching how a
/// real pipeline's last stage wins.
pub struct Resolved {
    pub from_node: NodeId,
    pub from_stream_idx: usize,
    pub codec: Codec,
    pub metadata: BTreeMap<String, String>,
    pub disposition: Option<BTreeSet<String>>,
    /// In source-to-output order (the order the filters should actually be
    /// applied), even though this is built up walking backward from the
    /// output.
    pub filters: Vec<(FilterName, BTreeMap<String, String>)>,
}

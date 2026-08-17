use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::chapter::Chapter;
use super::node::{InputNode, ModifierNode, OutputNode};
use super::stream::StreamInfo;
use super::wire::Wire;
use super::NodeId;

/// Bumped whenever a change to this shape (a field added/removed/renamed,
/// or a variant added to one of the tagged enums it embeds) could make an
/// older or newer tff unable to read a file the other one wrote --
/// `crate::project::load` checks this before trusting the rest of the file,
/// so a mismatch is reported clearly ("this file is from a different tff
/// version") instead of surfacing as a raw deserialize error somewhere deep
/// in the middle of reconstructing the graph.
pub const PROJECT_FORMAT_VERSION: u32 = 1;

/// The on-disk shape of a saved project -- everything `Graph::to_project_file`/
/// `Graph::from_project_file` need to round-trip a `Graph`, serialized as-is
/// via `serde_json` by `crate::project` (which owns the actual file I/O;
/// `graph` stays free of that, same convention `chapters_ffmetadata`'s doc
/// comment already documents for chapter files).
#[derive(Serialize, Deserialize)]
pub struct ProjectFile {
    pub version: u32,
    pub inputs: Vec<SavedInput>,
    pub modifiers: Vec<ModifierNode>,
    pub outputs: Vec<OutputNode>,
    pub wires: Vec<Wire>,
    pub next_id: NodeId,
}

/// An `InputNode`, minus the two fields that don't make sense to save:
/// `file_index` (recomputed from position in `inputs` on load, same as
/// `add_input`/`remove_input` already maintain it) and `file_missing`
/// (meaningless before a load has even been attempted). `streams` is the
/// *raw* probed list, without the synthetic chapter-stream entry
/// `add_input` appends -- `Graph::to_project_file`/`from_project_file`
/// share that derivation via `streams_with_chapter_marker` rather than
/// saving its result directly, so there's exactly one place that logic
/// lives.
#[derive(Serialize, Deserialize)]
pub struct SavedInput {
    pub id: NodeId,
    pub path: String,
    pub streams: Vec<StreamInfo>,
    pub chapters: Vec<Chapter>,
    pub extra_args: BTreeMap<String, String>,
    pub pos: (f64, f64),
    pub width: u16,
}

impl SavedInput {
    /// `node.streams` has the synthetic chapter-stream entry already baked
    /// in (if `node.chapters` is non-empty) -- filtered back out here so a
    /// reload runs it through `streams_with_chapter_marker` exactly once,
    /// rather than saving a copy of its output that could drift from what
    /// that function would derive fresh.
    pub(super) fn from_node(node: &InputNode) -> Self {
        SavedInput {
            id: node.id,
            path: node.path.clone(),
            streams: node.streams.iter().filter(|s| s.kind != super::StreamKind::Chapter).cloned().collect(),
            chapters: node.chapters.clone(),
            extra_args: node.extra_args.clone(),
            pos: node.pos,
            width: node.width,
        }
    }
}

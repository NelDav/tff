use anyhow::{bail, Context, Result};

use crate::ffmpeg;
use crate::graph::{Graph, ProjectFile, PROJECT_FORMAT_VERSION};

/// Writes `graph` to `path` as pretty-printed JSON (see `graph::ProjectFile`
/// for the shape) -- human-readable/diffable by design, since this is
/// meant to be an ordinary file a user might keep in version control or
/// just open in a text editor out of curiosity, not an opaque blob.
pub fn save(graph: &Graph, path: &str) -> Result<()> {
    let file = graph.to_project_file();
    let json = serde_json::to_string_pretty(&file).context("failed to serialize the project")?;
    std::fs::write(path, json).with_context(|| format!("failed to write '{path}'"))?;
    Ok(())
}

/// What loading a project reports back, beyond the reconstructed `Graph`
/// itself -- `missing_inputs` lists any inputs that couldn't be re-probed
/// (moved, deleted, ...), so the caller can log a clear summary rather than
/// leaving the user to notice the grayed-out nodes (see
/// `InputNode::file_missing`) on their own.
pub struct LoadResult {
    pub graph: Graph,
    pub missing_inputs: Vec<String>,
}

/// Reads and parses `path`, then re-probes every input it references (see
/// `Graph::from_project_file`) before handing back the reconstructed graph
/// -- `Graph` itself stays free of file I/O, so this is the one place that
/// actually calls `ffmpeg::probe` on a project's behalf.
pub fn load(path: &str) -> Result<LoadResult> {
    let json = std::fs::read_to_string(path).with_context(|| format!("failed to read '{path}'"))?;
    let file: ProjectFile = serde_json::from_str(&json).context("failed to parse the project file")?;
    if file.version != PROJECT_FORMAT_VERSION {
        bail!(
            "'{path}' is project format version {}, but this tff build only supports version {}",
            file.version,
            PROJECT_FORMAT_VERSION
        );
    }

    let mut missing_inputs = Vec::new();
    let graph = Graph::from_project_file(file, |input_path| match ffmpeg::probe(input_path) {
        Ok(result) => Some((result.streams, result.chapters)),
        Err(_) => {
            missing_inputs.push(input_path.to_string());
            None
        }
    });
    Ok(LoadResult { graph, missing_inputs })
}

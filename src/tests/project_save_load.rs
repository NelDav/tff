use super::*;

use crate::graph::ModifierKind;

/// A graph with an input routed through a Convert modifier to an output,
/// wires and all -- the round-trip fixture shared by the tests below.
fn sample_graph() -> Graph {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
    let modifier = graph.add_modifier(ModifierKind::Convert(Codec::Encode("libx265".to_string())));
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));
    graph.connect(Endpoint::Stream { node: id, stream_idx: 1 }, Target::Output(out));
    graph
}

/// Saving then loading a graph (with a reprobe that "succeeds" and hands
/// back the same stream data) should reproduce the same wiring: node ids
/// are preserved verbatim across the round trip, so the built ffmpeg
/// invocation -- which is entirely id/wire driven -- should come out
/// identical.
#[test]
fn round_trip_preserves_wiring_and_stream_data() {
    let original = sample_graph();
    let before_args = original.build_ffmpeg_args(&BTreeMap::new());

    let file = original.to_project_file();
    let reloaded = Graph::from_project_file(file, |_path| Some((video_audio_streams(), Vec::new())));
    let after_args = reloaded.build_ffmpeg_args(&BTreeMap::new());

    assert_eq!(before_args, after_args);
    assert_eq!(reloaded.inputs.len(), 1);
    assert_eq!(reloaded.inputs[0].path, "in.mp4");
    assert!(!reloaded.inputs[0].file_missing);
    assert_eq!(reloaded.wires.len(), original.wires.len());
}

/// When the reprobe callback can't find the source (moved/deleted file),
/// the reloaded input should fall back to the streams/chapters that were
/// saved rather than coming back empty, and should be flagged so the UI
/// can gray it out (see `ui::canvas::draw_input_node`).
#[test]
fn missing_source_falls_back_to_saved_streams_and_flags_the_node() {
    let original = sample_graph();
    let file = original.to_project_file();

    let reloaded = Graph::from_project_file(file, |_path| None);

    assert_eq!(reloaded.inputs.len(), 1);
    let input = &reloaded.inputs[0];
    assert!(input.file_missing);
    // The saved streams (minus the synthetic chapter marker, which isn't
    // saved raw) should still be there, re-derived the same way `add_input`
    // would from the raw saved data.
    assert_eq!(input.streams.len(), 2);
    assert_eq!(input.streams[0].kind, StreamKind::Video);
    assert_eq!(input.streams[1].kind, StreamKind::Audio);
}

/// `src/project.rs::load` should refuse a project file written by some
/// future, incompatible format version instead of silently misreading it.
#[test]
fn load_rejects_a_future_format_version() {
    let dir = std::env::temp_dir().join(format!(
        "tff-test-project-version-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("future.tffproj");
    std::fs::write(
        &path,
        r#"{"version":9999,"inputs":[],"modifiers":[],"outputs":[],"wires":[],"next_id":1}"#,
    )
    .unwrap();

    let message = match crate::project::load(path.to_str().unwrap()) {
        Ok(_) => panic!("a future format version should have been rejected"),
        Err(e) => format!("{e:#}"),
    };
    assert!(message.contains("9999"), "{message}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// End-to-end through `src/project.rs`'s real file I/O: saving a graph and
/// loading it back (source file present, so the reprobe succeeds for real)
/// should round-trip the input's path and produce an identical ffmpeg
/// invocation.
#[test]
fn save_then_load_round_trips_through_real_files() {
    let dir = std::env::temp_dir().join(format!(
        "tff-test-project-roundtrip-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source = make_test_source(&dir, 1, 32, 32);

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input(source.to_str().unwrap().to_string(), video_audio_streams(), Vec::new());
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));
    let before_args = graph.build_ffmpeg_args(&BTreeMap::new());

    let project_path = dir.join("project.tffproj");
    crate::project::save(&graph, project_path.to_str().unwrap()).expect("save should succeed");

    let result = crate::project::load(project_path.to_str().unwrap()).expect("load should succeed");
    assert!(result.missing_inputs.is_empty(), "source file exists, reprobe should succeed");
    assert!(!result.graph.inputs[0].file_missing);
    assert_eq!(result.graph.build_ffmpeg_args(&BTreeMap::new()), before_args);

    let _ = std::fs::remove_dir_all(&dir);
}

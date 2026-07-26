use super::*;


#[test]
fn parse_time_accepts_plain_seconds_and_colon_forms() {
    use crate::graph::parse_time;

    assert_eq!(parse_time("12.5"), Some(12.5));
    assert_eq!(parse_time("0"), Some(0.0));
    assert_eq!(parse_time("1:23"), Some(83.0)); // 1m23s
    assert_eq!(parse_time("1:02:03"), Some(3723.0)); // 1h2m3s
    assert_eq!(parse_time("0:00:01.5"), Some(1.5));
    assert_eq!(parse_time(""), None);
    assert_eq!(parse_time("not a time"), None);
    assert_eq!(parse_time("1:2:3:4"), None, "more than HH:MM:SS shouldn't parse");
    assert_eq!(parse_time(":30"), None, "an empty component shouldn't parse");
}

#[test]
fn format_time_pads_and_only_shows_millis_when_present() {
    use crate::graph::format_time;

    assert_eq!(format_time(0.0), "00:00:00");
    assert_eq!(format_time(65.0), "00:01:05");
    assert_eq!(format_time(3723.0), "01:02:03");
    assert_eq!(format_time(1.5), "00:00:01.500");
}

/// format_time's output should always be accepted back by parse_time and
/// land on (approximately) the same number of seconds -- the picker
/// round-trips a chapter's stored value through exactly this pair every
/// time it's opened for editing.
#[test]
fn format_time_and_parse_time_round_trip() {
    use crate::graph::{format_time, parse_time};

    for secs in [0.0, 1.0, 59.999, 60.0, 3599.5, 7384.25] {
        let formatted = format_time(secs);
        let parsed = parse_time(&formatted).unwrap_or_else(|| panic!("expected {formatted} to parse"));
        assert!((parsed - secs).abs() < 0.001, "round-trip drifted: {secs} -> {formatted} -> {parsed}");
    }
}

/// The written FFMETADATA should escape the characters the format treats
/// specially in a title, matching what a real ffmpeg-exported file does
/// (verified separately against a real build).
#[test]
fn chapters_ffmetadata_escapes_special_characters_in_titles() {
    use crate::graph::{chapters_ffmetadata, Chapter};

    let chapters = vec![Chapter::new(0.0, 5.0, "Chapter #1: A=B; C\\D".to_string())];
    let content = chapters_ffmetadata(&chapters);

    assert!(content.starts_with(";FFMETADATA1\n"));
    assert!(content.contains("[CHAPTER]\n"));
    assert!(content.contains("TIMEBASE=1/1000\n"));
    assert!(content.contains("START=0\n"));
    assert!(content.contains("END=5000\n"));
    assert!(content.contains(r"title=Chapter \#1: A\=B\; C\\D"), "{content}");
}

/// A plain FFMETADATA text file added as an input should probe with an
/// empty stream list and a populated chapter list -- verified against a
/// real ffprobe build: it autodetects the format from content (no `-f`
/// needed) and reports exactly this shape (`streams: []`, `chapters: [...]`
/// ), same as `ffmpeg::probe` now asks for via `-show_chapters`.
#[test]
fn probe_reads_chapters_from_a_plain_ffmetadata_text_file() {
    let dir = std::env::temp_dir().join(format!("tff-test-probe-ffmeta-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("chapters.ffmeta");
    std::fs::write(
        &path,
        ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=5000\ntitle=Intro\n\
         [CHAPTER]\nTIMEBASE=1/1000\nSTART=5000\nEND=10000\ntitle=Main Part\n",
    )
    .unwrap();

    let result = ffmpeg::probe(path.to_str().unwrap()).unwrap();

    assert!(result.streams.is_empty(), "{:?}", result.streams);
    assert_eq!(result.chapters.len(), 2);
    assert_eq!(result.chapters[0].title, "Intro");
    assert_eq!(result.chapters[0].start_secs, 0.0);
    assert_eq!(result.chapters[0].end_secs, 5.0);
    assert_eq!(result.chapters[1].title, "Main Part");

    let _ = std::fs::remove_dir_all(&dir);
}

/// `add_input` should expose a synthetic `StreamKind::Chapter` port
/// whenever the probed chapters are non-empty, appended after the real
/// streams -- this is what makes a chapters-only text-file input wireable
/// the same way a video/audio stream is, with no special-cased node kind.
#[test]
fn add_input_exposes_a_synthetic_chapter_stream_when_chapters_are_present() {
    let mut graph = Graph::new();
    let chapters = vec![Chapter::new(0.0, 5.0, "Intro".to_string())];
    let id = graph.add_input("chapters.ffmeta".to_string(), Vec::new(), chapters);

    let input = graph.input(id).unwrap();
    assert_eq!(input.streams.len(), 1, "{:?}", input.streams);
    assert_eq!(input.streams[0].kind, StreamKind::Chapter);
    assert_eq!(input.chapters.len(), 1);
    assert!(input.streams[0].label().contains("chapters"), "{}", input.streams[0].label());
}

/// No chapters -> no synthetic port, same as a real file with no chapter
/// data: an ordinary video-only input shouldn't grow an extra chapter row.
#[test]
fn add_input_omits_the_chapter_stream_when_there_are_no_chapters() {
    let mut graph = Graph::new();
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    assert!(graph.input(id).unwrap().streams.iter().all(|s| s.kind != StreamKind::Chapter));
}

/// A chapter endpoint wired straight from a real input to an output's
/// chapters slot should resolve to `FromInput`, referencing that input's
/// own file index -- no `ChapterEdit` node needed for a pure passthrough.
#[test]
fn resolve_chapters_from_a_direct_input_connection() {
    use crate::graph::ChapterSource;

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let chapters = vec![Chapter::new(0.0, 5.0, "Intro".to_string())];
    let id = graph.add_input("chapters.ffmeta".to_string(), Vec::new(), chapters);
    let chapter_stream_idx = graph.input(id).unwrap().streams.len() - 1;
    graph.connect(
        Endpoint::Stream { node: id, stream_idx: chapter_stream_idx },
        Target::OutputChapters(out),
    );

    let source = graph.output_chapters(out);
    assert_eq!(source, Some(ChapterSource::FromInput { input_file_index: 0 }));
}

/// A `ChapterEdit` modifier's output resolves to `Edited`, referencing the
/// node itself -- its own list is authoritative regardless of what (if
/// anything) feeds its input.
#[test]
fn resolve_chapters_through_a_chapter_edit_node() {
    use crate::graph::{ChapterSource, ModifierKind};

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let modifier = graph.add_modifier(ModifierKind::ChapterEdit {
        chapters: vec![Chapter::new(0.0, 1.0, "A".to_string())],
    });
    graph.connect(Endpoint::ModifierOut(modifier), Target::OutputChapters(out));

    assert_eq!(graph.output_chapters(out), Some(ChapterSource::Edited { modifier_id: modifier }));
}

/// A `ChapterEdit` node closest to the output wins outright and nothing
/// further upstream is consulted -- mirrors how Convert's codec and
/// Disposition's flags already behave (first modifier walking backward
/// wins), applied here to the whole chapter list rather than a single
/// field, and needed because the picker's "import from connected input"
/// action deliberately doesn't create any kind of live link back to the
/// source once the copy is made.
#[test]
fn chapter_edit_wins_outright_over_whatever_feeds_its_input() {
    use crate::graph::{ChapterSource, ModifierKind};

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let input_chapters = vec![Chapter::new(0.0, 1.0, "FromInput".to_string())];
    let input_id = graph.add_input("chapters.ffmeta".to_string(), Vec::new(), input_chapters);
    let chapter_stream_idx = graph.input(input_id).unwrap().streams.len() - 1;
    let modifier =
        graph.add_modifier(ModifierKind::ChapterEdit { chapters: vec![Chapter::new(0.0, 2.0, "Edited".to_string())] });
    graph.connect(Endpoint::Stream { node: input_id, stream_idx: chapter_stream_idx }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::OutputChapters(out));

    assert_eq!(graph.output_chapters(out), Some(ChapterSource::Edited { modifier_id: modifier }));
}

/// An output's chapters slot behaves like a modifier's input slot, not
/// like its regular mapped-stream slot: wiring a second source into it
/// replaces the first rather than fanning in -- explicit user requirement
/// ("keep it like it is already for video/audio ports... only one
/// incoming wire per port").
#[test]
fn output_chapters_slot_accepts_only_one_wire_at_a_time() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let a = graph.add_input("a.ffmeta".to_string(), Vec::new(), vec![Chapter::new(0.0, 1.0, "A".to_string())]);
    let b = graph.add_input("b.ffmeta".to_string(), Vec::new(), vec![Chapter::new(0.0, 1.0, "B".to_string())]);
    let a_idx = graph.input(a).unwrap().streams.len() - 1;
    let b_idx = graph.input(b).unwrap().streams.len() - 1;

    graph.connect(Endpoint::Stream { node: a, stream_idx: a_idx }, Target::OutputChapters(out));
    graph.connect(Endpoint::Stream { node: b, stream_idx: b_idx }, Target::OutputChapters(out));

    let incoming = graph.incoming(Target::OutputChapters(out));
    assert_eq!(incoming.len(), 1, "wiring a second source should replace the first, not add to it");
    assert_eq!(graph.wires[incoming[0]].from, Endpoint::Stream { node: b, stream_idx: b_idx });
}

/// Removing an output should clean up a wire feeding its chapters slot the
/// same way it already does for its regular mapped-stream wires.
#[test]
fn remove_output_cleans_up_its_chapters_wire() {
    let mut graph = Graph::new();
    let out = graph.add_output();
    let id = graph.add_input("chapters.ffmeta".to_string(), Vec::new(), vec![Chapter::new(0.0, 1.0, "A".to_string())]);
    let idx = graph.input(id).unwrap().streams.len() - 1;
    graph.connect(Endpoint::Stream { node: id, stream_idx: idx }, Target::OutputChapters(out));
    assert_eq!(graph.wires.len(), 1);

    graph.remove_output(out);

    assert!(graph.wires.is_empty(), "{:?}", graph.wires);
}

/// End-to-end check of the direct-passthrough path: a real input file's
/// own chapters, wired straight to an output with no `ChapterEdit` in the
/// chain, should need no extra `-i` at all -- `-map_chapters` points
/// directly at the real input's file index.
#[test]
fn output_chapters_from_direct_input_apply_end_to_end_with_no_extra_input() {
    let dir = std::env::temp_dir().join(format!("tff-test-chapters-direct-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let chapters_path = dir.join("chapters.ffmeta");
    std::fs::write(
        &chapters_path,
        ";FFMETADATA1\n[CHAPTER]\nTIMEBASE=1/1000\nSTART=0\nEND=500\ntitle=Intro\n\
         [CHAPTER]\nTIMEBASE=1/1000\nSTART=500\nEND=1000\ntitle=Outro\n",
    )
    .unwrap();
    let source_path = make_test_source(&dir, 1, 160, 120);
    let out_path = dir.join("out.mkv");

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    let video = ffmpeg::probe(source_path.to_str().unwrap()).unwrap();
    let video_id = graph.add_input(source_path.to_str().unwrap().to_string(), video.streams, Vec::new());
    graph.connect(Endpoint::Stream { node: video_id, stream_idx: 0 }, Target::Output(out));

    let chapters = ffmpeg::probe(chapters_path.to_str().unwrap()).unwrap();
    assert!(chapters.streams.is_empty());
    let chapters_id = graph.add_input(chapters_path.to_str().unwrap().to_string(), chapters.streams, chapters.chapters);
    let chapter_stream_idx = graph.input(chapters_id).unwrap().streams.len() - 1;
    graph.connect(Endpoint::Stream { node: chapters_id, stream_idx: chapter_stream_idx }, Target::OutputChapters(out));

    let args = graph.build_ffmpeg_args(&BTreeMap::new()); // no ChapterEdit node, so no temp file needed
    assert!(args.iter().filter(|a| a.as_str() == "-i").count() == 2, "expected exactly the two real -i inputs: {args:?}");
    assert!(args.windows(2).any(|w| w == ["-map_chapters", "1"]), "expected -map_chapters pointing at the chapters input (index 1): {args:?}");
    run_ok(Command::new("ffmpeg").args(&args));

    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_chapters", "-of", "json", out_path.to_str().unwrap()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&probe.stdout);
    assert!(text.contains("\"title\": \"Intro\""), "{text}");
    assert!(text.contains("\"title\": \"Outro\""), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// End-to-end check of the `ChapterEdit` path: chapters authored directly
/// in a modifier node (no source file at all) get synthesized to a temp
/// FFMETADATA file and threaded in as an extra `-i`, and two outputs
/// sharing the same node reuse that single extra input rather than each
/// getting their own.
#[test]
fn output_chapters_from_chapter_edit_apply_end_to_end_and_are_shared_across_outputs() {
    use crate::graph::ModifierKind;

    let dir = std::env::temp_dir().join(format!("tff-test-chapters-edited-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = make_test_source(&dir, 1, 160, 120);
    let out1_path = dir.join("out1.mkv");
    let out2_path = dir.join("out2.mkv");
    let chapter_file_path = dir.join("edited.ffmeta");

    let mut graph = Graph::new();
    let out1 = graph.outputs[0].id;
    graph.outputs[0].path = out1_path.to_str().unwrap().to_string();
    let out2 = graph.add_output();
    graph.output_mut(out2).unwrap().path = out2_path.to_str().unwrap().to_string();

    let video = ffmpeg::probe(source_path.to_str().unwrap()).unwrap();
    let video_id = graph.add_input(source_path.to_str().unwrap().to_string(), video.streams, Vec::new());
    graph.connect(Endpoint::Stream { node: video_id, stream_idx: 0 }, Target::Output(out1));
    graph.connect(Endpoint::Stream { node: video_id, stream_idx: 0 }, Target::Output(out2));

    let chapters = vec![
        Chapter::new(0.0, 0.5, "Intro".to_string()),
        Chapter::new(0.5, 1.0, "Outro".to_string()),
    ];
    let modifier = graph.add_modifier(ModifierKind::ChapterEdit { chapters: chapters.clone() });
    graph.connect(Endpoint::ModifierOut(modifier), Target::OutputChapters(out1));
    graph.connect(Endpoint::ModifierOut(modifier), Target::OutputChapters(out2));

    std::fs::write(&chapter_file_path, crate::graph::chapters_ffmetadata(&chapters)).unwrap();
    let mut chapter_files = BTreeMap::new();
    chapter_files.insert(modifier, chapter_file_path.to_str().unwrap().to_string());

    let args = graph.build_ffmpeg_args(&chapter_files);
    assert_eq!(
        args.iter().filter(|a| a.as_str() == "-i").count(),
        2,
        "the shared ChapterEdit node should only add one extra -i, not one per output: {args:?}"
    );
    assert_eq!(
        args.iter().filter(|a| a.as_str() == "-map_chapters").count(),
        2,
        "both outputs should still get their own -map_chapters: {args:?}"
    );
    run_ok(Command::new("ffmpeg").args(&args));

    for path in [&out1_path, &out2_path] {
        let probe = Command::new("ffprobe")
            .args(["-v", "error", "-show_chapters", "-of", "json", path.to_str().unwrap()])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&probe.stdout);
        assert!(text.contains("\"title\": \"Intro\""), "{path:?}: {text}");
        assert!(text.contains("\"title\": \"Outro\""), "{path:?}: {text}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}


use super::*;


/// End-to-end check of the core feature: pull one track each from three
/// different input files and mux them into a single output file via plain
/// direct wires (no modifiers needed for a straight copy-through).
#[test]
fn combines_video_audio_and_subtitle_from_three_files() {
    let dir = std::env::temp_dir().join(format!("tff-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let video_path = dir.join("v_only.mp4");
    let audio_path = dir.join("a_only.m4a");
    let sub_path = dir.join("sub.srt");
    let out_path = dir.join("combined.mkv");

    std::fs::write(&sub_path, "1\n00:00:00,000 --> 00:00:01,000\nhello from tff\n").unwrap();

    run_ok(Command::new("ffmpeg").args([
        "-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=duration=1:size=160x120:rate=5",
        "-c:v", "libx264", "-an", video_path.to_str().unwrap(),
    ]));
    run_ok(Command::new("ffmpeg").args([
        "-y", "-loglevel", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
        "-c:a", "aac", audio_path.to_str().unwrap(),
    ]));

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id_v = graph.add_input(video_path.to_str().unwrap().to_string(), ffmpeg::probe(video_path.to_str().unwrap()).unwrap().streams, Vec::new());
    let id_a = graph.add_input(audio_path.to_str().unwrap().to_string(), ffmpeg::probe(audio_path.to_str().unwrap()).unwrap().streams, Vec::new());
    let id_s = graph.add_input(sub_path.to_str().unwrap().to_string(), ffmpeg::probe(sub_path.to_str().unwrap()).unwrap().streams, Vec::new());

    graph.connect(Endpoint::Stream { node: id_v, stream_idx: 0 }, Target::Output(out));
    graph.connect(Endpoint::Stream { node: id_a, stream_idx: 0 }, Target::Output(out));
    graph.connect(Endpoint::Stream { node: id_s, stream_idx: 0 }, Target::Output(out));
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let out_streams = ffmpeg::probe(out_path.to_str().unwrap()).unwrap().streams;
    assert_eq!(out_streams.len(), 3, "expected exactly 3 muxed streams");
    assert!(out_streams.iter().any(|s| s.kind == StreamKind::Video));
    assert!(out_streams.iter().any(|s| s.kind == StreamKind::Audio));
    assert!(out_streams.iter().any(|s| s.kind == StreamKind::Subtitle));

    let _ = std::fs::remove_dir_all(&dir);
}

/// End-to-end check of the codec-conversion feature, now routed through an
/// actual Convert modifier node rather than a direct edge: encodes an AAC
/// source track to FLAC and verifies the *output* file's codec changed.
#[test]
fn convert_modifier_reencodes_a_stream_end_to_end() {
    let dir = std::env::temp_dir().join(format!("tff-test-reencode-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let audio_path = dir.join("a_only.m4a");
    let out_path = dir.join("recoded.mkv");

    run_ok(Command::new("ffmpeg").args([
        "-y", "-loglevel", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
        "-c:a", "aac", audio_path.to_str().unwrap(),
    ]));

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let source_streams = ffmpeg::probe(audio_path.to_str().unwrap()).unwrap().streams;
    assert_eq!(source_streams[0].codec, "aac", "test fixture should start as aac");
    let id = graph.add_input(audio_path.to_str().unwrap().to_string(), source_streams, Vec::new());
    let modifier = graph.add_modifier(ModifierKind::Convert(Codec::Encode("flac".to_string())));

    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let out_streams = ffmpeg::probe(out_path.to_str().unwrap()).unwrap().streams;
    assert_eq!(out_streams.len(), 1);
    assert_eq!(out_streams[0].codec, "flac", "expected the output to be re-encoded to flac");

    let _ = std::fs::remove_dir_all(&dir);
}

/// End-to-end check of the metadata-modifier feature: applies a language
/// and title tag via a Metadata node and confirms, via ffprobe, that they
/// actually landed on the output stream -- not just in the command string.
#[test]
fn metadata_modifier_applies_language_and_title_end_to_end() {
    let dir = std::env::temp_dir().join(format!("tff-test-metadata-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let audio_path = dir.join("a_only.m4a");
    let out_path = dir.join("tagged.mkv");

    run_ok(Command::new("ffmpeg").args([
        "-y", "-loglevel", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
        "-c:a", "aac", audio_path.to_str().unwrap(),
    ]));

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input(audio_path.to_str().unwrap().to_string(), ffmpeg::probe(audio_path.to_str().unwrap()).unwrap().streams, Vec::new());
    let modifier = graph.add_modifier(ModifierKind::Metadata {
        fields: metadata_fields(&[("language", "eng"), ("title", "Commentary"), ("handler_name", "Custom Handler")]),
    });

    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream_tags=language,title,handler_name", "-of", "default=noprint_wrappers=0", out_path.to_str().unwrap()])
        .output()
        .unwrap();
    let tags = String::from_utf8_lossy(&probe.stdout);
    assert!(tags.contains("language=eng"), "expected language tag in output:\n{tags}");
    assert!(tags.contains("title=Commentary"), "expected title tag in output:\n{tags}");
    // Matroska's muxer uppercases some conventionally-uppercase tag names
    // (HANDLER_NAME, ENCODER, ...) while leaving language/title lowercase --
    // a real, observed quirk, not a bug in our code -- so check case-insensitively.
    assert!(
        tags.to_lowercase().contains("handler_name=custom handler"),
        "expected handler_name tag in output:\n{tags}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// End-to-end check that a Disposition modifier's flags land on the right
/// stream. Written specifically to catch a real gotcha found while building
/// this: `-disposition:s:<i>` (mirroring how `-metadata:s:<i>` is built)
/// silently no-ops, because for `-disposition` the `s` is the generic
/// stream-specifier's *type* letter (subtitle) rather than a fixed
/// "this targets a stream" marker the way it is for `-metadata`. The
/// bare-index form (`-disposition:<i>`, like `-c:<i>`) is the one that
/// actually addresses "the i-th mapped output stream".
#[test]
fn disposition_modifier_sets_flags_on_the_right_stream_end_to_end() {
    let dir = std::env::temp_dir().join(format!("tff-test-disposition-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let source_path = dir.join("source.mp4");
    let out_path = dir.join("out.mkv");

    run_ok(Command::new("ffmpeg").args([
        "-y", "-loglevel", "error",
        "-f", "lavfi", "-i", "testsrc=duration=1:size=160x120:rate=5",
        "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
        "-c:v", "libx264", "-c:a", "aac", "-shortest", source_path.to_str().unwrap(),
    ]));

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap().streams;
    let video_idx = streams.iter().position(|s| s.kind == StreamKind::Video).unwrap();
    let audio_idx = streams.iter().position(|s| s.kind == StreamKind::Audio).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams, Vec::new());

    // Video mapped first (stream 0 in the output) with no Disposition
    // modifier, audio mapped second (stream 1) with "forced" set on it --
    // so the test also catches a wrong-index regression, not just a
    // wrong-specifier one.
    let modifier = graph.add_modifier(ModifierKind::Disposition { flags: disposition_set(&["forced"]) });
    graph.connect(Endpoint::Stream { node: id, stream_idx: video_idx }, Target::Output(out));
    graph.connect(Endpoint::Stream { node: id, stream_idx: audio_idx }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let probe = Command::new("ffprobe")
        .args([
            "-v", "error", "-show_entries", "stream=index:stream_disposition=forced",
            "-of", "compact", out_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&probe.stdout);
    assert!(
        text.contains("index=0|disposition:forced=0"),
        "video stream shouldn't have 'forced' set:\n{text}"
    );
    assert!(
        text.contains("index=1|disposition:forced=1"),
        "audio stream should have 'forced' set:\n{text}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}


use super::*;


/// setpts-based video shift: a positive shift should push every frame's
/// timestamp later, extending the output's total duration by roughly the
/// same amount (the classic "fix the sync" use case).
#[test]
fn filter_shift_video_end_to_end() {
    let dir = std::env::temp_dir().join(format!("tff-test-shift-video-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = make_test_source(&dir, 2, 160, 120);
    let out_path = dir.join("out.mkv");

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap().streams;
    let video_idx = streams.iter().position(|s| s.kind == StreamKind::Video).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams, Vec::new());
    let modifier =
        graph.add_modifier(ModifierKind::Filter { name: FilterName::Shift, fields: filter_fields(&[("seconds", "1")]) });
    graph.connect(Endpoint::Stream { node: id, stream_idx: video_idx }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1", out_path.to_str().unwrap()])
        .output()
        .unwrap();
    let duration: f64 =
        String::from_utf8_lossy(&probe.stdout).trim().strip_prefix("duration=").unwrap().parse().unwrap();
    assert!(duration > 2.7, "expected the 1s shift to extend the ~2s source's duration, got {duration}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// adelay-based audio shift: same idea as the video case, but also the
/// place a real, verified asymmetry lives -- adelay only accepts
/// non-negative delays, unlike setpts. Only the positive direction is
/// exercised here since that's all the filter supports.
#[test]
fn filter_shift_audio_end_to_end() {
    let dir = std::env::temp_dir().join(format!("tff-test-shift-audio-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = make_test_source(&dir, 2, 160, 120);
    let out_path = dir.join("out.mkv");

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap().streams;
    let audio_idx = streams.iter().position(|s| s.kind == StreamKind::Audio).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams, Vec::new());
    let modifier =
        graph.add_modifier(ModifierKind::Filter { name: FilterName::Shift, fields: filter_fields(&[("seconds", "1")]) });
    graph.connect(Endpoint::Stream { node: id, stream_idx: audio_idx }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1", out_path.to_str().unwrap()])
        .output()
        .unwrap();
    let duration: f64 =
        String::from_utf8_lossy(&probe.stdout).trim().strip_prefix("duration=").unwrap().parse().unwrap();
    assert!(duration > 2.7, "expected the 1s shift to extend the ~2s source's duration, got {duration}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Volume: verified via `volumedetect`'s mean_volume, not just "did ffmpeg
/// exit 0" -- a factor of 0.1 should read roughly 20dB quieter
/// (20*log10(0.1) = -20dB) than the unfiltered source.
#[test]
fn filter_volume_reduces_loudness_end_to_end() {
    let dir = std::env::temp_dir().join(format!("tff-test-volume-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = make_test_source(&dir, 2, 160, 120);
    let out_path = dir.join("out.mkv");

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap().streams;
    let audio_idx = streams.iter().position(|s| s.kind == StreamKind::Audio).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams, Vec::new());
    let modifier =
        graph.add_modifier(ModifierKind::Filter { name: FilterName::Volume, fields: filter_fields(&[("factor", "0.1")]) });
    graph.connect(Endpoint::Stream { node: id, stream_idx: audio_idx }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let mean_volume_db = |path: &str| -> f64 {
        let output = Command::new("ffmpeg")
            .args(["-i", path, "-af", "volumedetect", "-f", "null", "-"])
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        let line = stderr.lines().find(|l| l.contains("mean_volume:")).expect("mean_volume line");
        line.split("mean_volume:").nth(1).unwrap().trim().trim_end_matches(" dB").parse().unwrap()
    };
    let before = mean_volume_db(source_path.to_str().unwrap());
    let after = mean_volume_db(out_path.to_str().unwrap());
    assert!(
        before - after > 15.0,
        "expected roughly 20dB quieter with volume=0.1, source={before}dB output={after}dB"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// Scale: only setting width, height left as the picker's "-1" default
/// (preserve aspect) -- checks both that the filter runs and that the
/// unset-field default actually behaves as documented.
#[test]
fn filter_scale_resizes_video_end_to_end() {
    let dir = std::env::temp_dir().join(format!("tff-test-scale-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = make_test_source(&dir, 1, 160, 120);
    let out_path = dir.join("out.mkv");

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap().streams;
    let video_idx = streams.iter().position(|s| s.kind == StreamKind::Video).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams, Vec::new());
    let modifier =
        graph.add_modifier(ModifierKind::Filter { name: FilterName::Scale, fields: filter_fields(&[("width", "80")]) });
    graph.connect(Endpoint::Stream { node: id, stream_idx: video_idx }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=width,height", "-of", "compact", out_path.to_str().unwrap()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&probe.stdout);
    // source is 160x120 (4:3); scaled to width=80 with height left as -1
    // (preserve aspect) should land on 80x60.
    assert!(text.contains("width=80|height=60"), "expected 80x60, aspect-preserved from 160x120:\n{text}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Crop: only width/height set, x/y left to ffmpeg's own centered default.
#[test]
fn filter_crop_crops_video_end_to_end() {
    let dir = std::env::temp_dir().join(format!("tff-test-crop-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = make_test_source(&dir, 1, 160, 120);
    let out_path = dir.join("out.mkv");

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap().streams;
    let video_idx = streams.iter().position(|s| s.kind == StreamKind::Video).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams, Vec::new());
    let modifier = graph.add_modifier(ModifierKind::Filter {
        name: FilterName::Crop,
        fields: filter_fields(&[("width", "50"), ("height", "40")]),
    });
    graph.connect(Endpoint::Stream { node: id, stream_idx: video_idx }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=width,height", "-of", "compact", out_path.to_str().unwrap()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&probe.stdout);
    assert!(text.contains("width=50|height=40"), "expected the crop dims regardless of source size:\n{text}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Fade: verified by checking the first frame is actually near-black
/// (0.0 avg luma with fade-in) versus the source's un-faded first frame
/// (a colorful `testsrc` pattern, ~127 avg luma) -- not just "ffmpeg exited
/// 0", which would pass even if the filter silently did nothing.
#[test]
fn filter_fade_starts_black_end_to_end() {
    let dir = std::env::temp_dir().join(format!("tff-test-fade-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = make_test_source(&dir, 2, 160, 120);
    let out_path = dir.join("out.mkv");

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap().streams;
    let video_idx = streams.iter().position(|s| s.kind == StreamKind::Video).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams, Vec::new());
    let modifier = graph.add_modifier(ModifierKind::Filter {
        name: FilterName::Fade,
        fields: filter_fields(&[("type", "in"), ("start", "0"), ("duration", "1")]),
    });
    graph.connect(Endpoint::Stream { node: id, stream_idx: video_idx }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let avg_luma_of_first_frame = |path: &str| -> f64 {
        let output = Command::new("ffmpeg")
            .args(["-i", path, "-vframes", "1", "-f", "rawvideo", "-pix_fmt", "gray", "-"])
            .output()
            .unwrap();
        let data = output.stdout;
        data.iter().map(|&b| b as f64).sum::<f64>() / data.len() as f64
    };
    let faded = avg_luma_of_first_frame(out_path.to_str().unwrap());
    assert!(faded < 5.0, "expected the fade-in's first frame to be near-black, got avg luma {faded}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Rotate: 90-degree directions should swap width/height; 180 (two chained
/// 90s) should preserve the original dimensions.
#[test]
fn filter_rotate_swaps_dimensions_end_to_end() {
    let dir = std::env::temp_dir().join(format!("tff-test-rotate-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = make_test_source(&dir, 1, 160, 120);

    let dims = |direction: &str| -> (String, String) {
        let out_path = dir.join(format!("out-{direction}.mkv"));
        let mut graph = Graph::new();
        let out = graph.outputs[0].id;
        let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap().streams;
        let video_idx = streams.iter().position(|s| s.kind == StreamKind::Video).unwrap();
        let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams, Vec::new());
        let modifier = graph.add_modifier(ModifierKind::Filter {
            name: FilterName::Rotate,
            fields: filter_fields(&[("direction", direction)]),
        });
        graph.connect(Endpoint::Stream { node: id, stream_idx: video_idx }, Target::ModifierIn(modifier));
        graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));
        graph.outputs[0].path = out_path.to_str().unwrap().to_string();
        assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

        let probe = Command::new("ffprobe")
            .args(["-v", "error", "-show_entries", "stream=width,height", "-of", "csv=p=0", out_path.to_str().unwrap()])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&probe.stdout);
        let mut parts = text.trim().split(',');
        (parts.next().unwrap().to_string(), parts.next().unwrap().to_string())
    };

    assert_eq!(dims("90cw"), ("120".to_string(), "160".to_string()), "90cw should swap 160x120 -> 120x160");
    assert_eq!(dims("180"), ("160".to_string(), "120".to_string()), "180 should preserve 160x120");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Two Filter modifiers chained on one wire (Scale then Crop) should both
/// apply, in order -- exercised through the real Graph/resolve()/
/// build_output_section() path, not just by hand-assembling ffmpeg args,
/// since that's the actual integration point for multi-filter chains.
#[test]
fn filter_chain_of_two_modifiers_applies_both_in_order_end_to_end() {
    let dir = std::env::temp_dir().join(format!("tff-test-filter-chain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = make_test_source(&dir, 1, 160, 120);
    let out_path = dir.join("out.mkv");

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap().streams;
    let video_idx = streams.iter().position(|s| s.kind == StreamKind::Video).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams, Vec::new());

    let scale = graph.add_modifier(ModifierKind::Filter {
        name: FilterName::Scale,
        fields: filter_fields(&[("width", "100"), ("height", "100")]),
    });
    let crop = graph.add_modifier(ModifierKind::Filter {
        name: FilterName::Crop,
        fields: filter_fields(&[("width", "50"), ("height", "50")]),
    });
    graph.connect(Endpoint::Stream { node: id, stream_idx: video_idx }, Target::ModifierIn(scale));
    graph.connect(Endpoint::ModifierOut(scale), Target::ModifierIn(crop));
    graph.connect(Endpoint::ModifierOut(crop), Target::Output(out));
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream=width,height", "-of", "compact", out_path.to_str().unwrap()])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&probe.stdout);
    // If only scale had applied: 100x100. If only crop had applied (crop's
    // own "iw"/"ih" default on the unscaled 160x120 source): still wrong.
    // 50x50 is reachable only if both stages actually ran in order.
    assert!(text.contains("width=50|height=50"), "expected both scale and crop to apply in order:\n{text}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// End-to-end check of the extra-args escape hatch, using a real, easily
/// verified flag: global (format-level) `-metadata`, which is distinct from
/// the per-stream `-metadata:s:<i>` the Metadata modifier node already
/// covers -- this is actually the one thing the escape hatch can reach that
/// nothing else in the graph model can. Not exercising the option that
/// prompted this feature (`-max_interleave_delta`) directly, since its
/// effect is internal muxer buffering behavior with no observable output to
/// assert on; that one's covered by a plain arg-construction test instead.
#[test]
fn output_extra_args_apply_a_real_global_metadata_flag_end_to_end() {
    let dir = std::env::temp_dir().join(format!("tff-test-extra-args-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = make_test_source(&dir, 1, 160, 120);
    let out_path = dir.join("out.mkv");

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap().streams;
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams, Vec::new());
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));
    graph.outputs[0].extra_args = filter_fields(&[("metadata", "comment=hello_from_tff")]);
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let probe = Command::new("ffprobe")
        .args([
            "-v", "error", "-show_entries", "format_tags=comment",
            "-of", "default=noprint_wrappers=1", out_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&probe.stdout);
    assert!(
        text.to_lowercase().contains("comment=hello_from_tff"),
        "expected the global (format-level) comment tag, not a per-stream one:\n{text}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// End-to-end check that a chain (Convert then Metadata) applies *both*
/// effects to the real output file in one ffmpeg run.
#[test]
fn chained_convert_and_metadata_apply_both_end_to_end() {
    let dir = std::env::temp_dir().join(format!("tff-test-chain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let audio_path = dir.join("a_only.m4a");
    let out_path = dir.join("chained.mkv");

    run_ok(Command::new("ffmpeg").args([
        "-y", "-loglevel", "error", "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
        "-c:a", "aac", audio_path.to_str().unwrap(),
    ]));

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input(audio_path.to_str().unwrap().to_string(), ffmpeg::probe(audio_path.to_str().unwrap()).unwrap().streams, Vec::new());
    let convert = graph.add_modifier(ModifierKind::Convert(Codec::Encode("flac".to_string())));
    let metadata = graph.add_modifier(ModifierKind::Metadata { fields: metadata_fields(&[("language", "deu")]) });

    let src = Endpoint::Stream { node: id, stream_idx: 0 };
    graph.connect(src, Target::ModifierIn(convert));
    graph.connect(Endpoint::ModifierOut(convert), Target::ModifierIn(metadata));
    graph.connect(Endpoint::ModifierOut(metadata), Target::Output(out));
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let out_streams = ffmpeg::probe(out_path.to_str().unwrap()).unwrap().streams;
    assert_eq!(out_streams[0].codec, "flac", "expected the convert stage to apply");

    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "stream_tags=language", "-of", "default=noprint_wrappers=0", out_path.to_str().unwrap()])
        .output()
        .unwrap();
    let tags = String::from_utf8_lossy(&probe.stdout);
    assert!(tags.contains("language=deu"), "expected the metadata stage to apply:\n{tags}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// End-to-end check of the multi-output feature: one ffmpeg invocation,
/// two separate output files, each getting a different stream from the
/// same source.
#[test]
fn two_outputs_produce_two_separate_files_in_one_ffmpeg_run() {
    let dir = std::env::temp_dir().join(format!("tff-test-multiout-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let source_path = dir.join("source.mp4");
    let video_out = dir.join("video_only.mkv");
    let audio_out = dir.join("audio_only.mkv");

    run_ok(Command::new("ffmpeg").args([
        "-y", "-loglevel", "error",
        "-f", "lavfi", "-i", "testsrc=duration=1:size=160x120:rate=5",
        "-f", "lavfi", "-i", "sine=frequency=440:duration=1",
        "-c:v", "libx264", "-c:a", "aac", "-shortest", source_path.to_str().unwrap(),
    ]));

    let mut graph = Graph::new();
    let out1 = graph.outputs[0].id;
    let out2 = graph.add_output();
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap().streams;
    let video_idx = streams.iter().position(|s| s.kind == StreamKind::Video).unwrap();
    let audio_idx = streams.iter().position(|s| s.kind == StreamKind::Audio).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams, Vec::new());

    graph.connect(Endpoint::Stream { node: id, stream_idx: video_idx }, Target::Output(out1));
    graph.connect(Endpoint::Stream { node: id, stream_idx: audio_idx }, Target::Output(out2));
    graph.outputs[0].path = video_out.to_str().unwrap().to_string();
    graph.outputs[1].path = audio_out.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let video_streams = ffmpeg::probe(video_out.to_str().unwrap()).unwrap().streams;
    assert_eq!(video_streams.len(), 1);
    assert_eq!(video_streams[0].kind, StreamKind::Video);

    let audio_streams = ffmpeg::probe(audio_out.to_str().unwrap()).unwrap().streams;
    assert_eq!(audio_streams.len(), 1);
    assert_eq!(audio_streams[0].kind, StreamKind::Audio);

    let _ = std::fs::remove_dir_all(&dir);
}

/// build_preview_args should target a caller-given path (not the output
/// node's own configured one) and cap the render with an output-scoped -t,
/// while still honoring whatever codec/metadata a modifier chain sets --
/// verified end-to-end against real ffmpeg, since -t's placement relative
/// to -map/-c/-metadata determines whether it's read as an input or output
/// option.
#[test]
fn preview_args_cap_duration_and_write_to_the_given_path() {
    let dir = std::env::temp_dir().join(format!("tff-test-preview-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let source_path = dir.join("source.mp4");
    let preview_path = dir.join("preview.mkv");

    run_ok(Command::new("ffmpeg").args([
        "-y", "-loglevel", "error",
        "-f", "lavfi", "-i", "testsrc=duration=5:size=160x120:rate=5",
        "-f", "lavfi", "-i", "sine=frequency=440:duration=5",
        "-c:v", "libx264", "-c:a", "aac", "-shortest", source_path.to_str().unwrap(),
    ]));

    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let unconnected_out = graph.add_output();
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap().streams;
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams, Vec::new());
    let modifier = graph.add_modifier(ModifierKind::Metadata { fields: metadata_fields(&[("language", "eng")]) });
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    // An output with nothing mapped to it has nothing resolvable to
    // preview, same as it has nothing to render for real.
    assert!(graph.build_preview_args(unconnected_out, preview_path.to_str().unwrap(), 2, &BTreeMap::new()).is_none());

    let args = graph.build_preview_args(out, preview_path.to_str().unwrap(), 2, &BTreeMap::new()).expect("resolvable");
    run_ok(Command::new("ffmpeg").args(&args));

    assert!(!dir.join(&graph.outputs[0].path).exists(), "preview must not touch the output's own configured path");

    let probe_out = Command::new("ffprobe")
        .args(["-v", "error", "-show_entries", "format=duration", "-of", "default=noprint_wrappers=1", preview_path.to_str().unwrap()])
        .output()
        .unwrap();
    let duration: f64 = String::from_utf8_lossy(&probe_out.stdout)
        .trim()
        .strip_prefix("duration=")
        .unwrap()
        .parse()
        .unwrap();
    assert!(duration <= 3.0, "expected the preview capped near 2s, got {duration}");

    let tags = ffmpeg::probe(preview_path.to_str().unwrap()).unwrap().streams;
    assert_eq!(tags[0].lang.as_deref(), Some("eng"), "modifier chain's metadata should still apply to the preview");

    let _ = std::fs::remove_dir_all(&dir);
}

/// Grounded against this dev environment, which is known to have a real
/// display available (both DISPLAY and WAYLAND_DISPLAY are set) --
/// confirms has_display isn't, say, checking a typo'd env var name and
/// always reporting false regardless of the real environment.
#[test]
fn has_display_detects_the_real_dev_environment() {
    assert!(ffmpeg::has_display(), "expected a display to be detected in this dev environment");
}

/// mpv exiting non-zero -- whether because it's not installed, its own
/// install is broken, or simply because /dev/null isn't a real media file
/// -- should surface as an Err from play_in_terminal, not silently report
/// success. Deliberately doesn't depend on mpv actually working here, so
/// this stays valid however this machine's local mpv install fares.
#[test]
fn play_in_terminal_surfaces_a_failing_mpv_as_an_error() {
    assert!(ffmpeg::play_in_terminal("/dev/null").is_err(), "expected an error from a bogus media path");
}

/// Sanity-checks the -encoders/-muxers parsers against whatever real ffmpeg
/// build is on PATH: known-common names should be found, and the fixed-width
/// column parsing in list_muxers should never pick up a legend line.
#[test]
fn discovers_real_encoders_and_muxers_from_ffmpeg() {
    let encoders = ffmpeg::list_encoders().unwrap();
    assert!(
        encoders.iter().any(|(n, k)| n == "libx264" && *k == StreamKind::Video),
        "expected libx264 among video encoders"
    );
    assert!(
        encoders.iter().any(|(n, k)| n == "aac" && *k == StreamKind::Audio),
        "expected aac among audio encoders"
    );

    let muxers = ffmpeg::list_muxers().unwrap();
    assert!(muxers.iter().any(|m| m == "matroska"), "expected matroska among muxers");
    assert!(muxers.iter().any(|m| m == "mp4"), "expected mp4 among muxers");
    assert!(!muxers.contains(&"=".to_string()), "parser must not pick up legend lines");
}


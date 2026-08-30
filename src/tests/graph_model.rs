use super::*;


/// A direct input -> output wire, with no modifier in between, should
/// resolve to a plain stream copy -- the sensible default for "just wire
/// it up with no changes".
#[test]
fn direct_wire_resolves_to_stream_copy() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let src = Endpoint::Stream { node: id, stream_idx: 0 };
    graph.connect(src, Target::Output(out));

    let resolved = graph.resolve(src).expect("direct wire should resolve");
    let Resolved::Stream { from_node, from_stream_idx, .. } = &resolved else {
        panic!("expected a plain Stream resolution, not a Concat");
    };
    assert_eq!(*from_node, id);
    assert_eq!(*from_stream_idx, 0);
    assert_eq!(*resolved.codec(), Codec::Copy);
    assert!(resolved.metadata().is_empty());

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let joined = args.join(" ");
    assert!(joined.contains("-c:0 copy"), "expected an explicit per-stream copy default: {joined}");
}

/// Routing a stream through a Convert modifier should make that codec show
/// up as a per-stream override in the built ffmpeg args.
#[test]
fn convert_modifier_sets_codec_override() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let modifier = graph.add_modifier(ModifierKind::Convert(Codec::Encode("libx265".to_string())));

    let src = Endpoint::Stream { node: id, stream_idx: 0 };
    graph.connect(src, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    let resolved = graph.resolve(Endpoint::ModifierOut(modifier)).unwrap();
    assert_eq!(*resolved.codec(), Codec::Encode("libx265".to_string()));

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let joined = args.join(" ");
    assert!(joined.contains("-c:0 libx265"), "expected the convert node's codec as an override: {joined}");
}

/// An input's extra_args (the advanced escape hatch for global input
/// options not otherwise modeled) should be spliced in immediately before
/// that input's own `-i <path>`, each entry as `-<key> <value>`. An
/// empty-string value (a valueless switch flag like `-re`) should emit just
/// the bare flag with no operand token.
#[test]
fn input_extra_args_are_spliced_before_its_own_dash_i() {
    let mut graph = Graph::new();
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    graph.input_mut(id).unwrap().extra_args = filter_fields(&[("itsoffset", "2.5"), ("re", "")]);

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let joined = args.join(" ");
    assert!(joined.contains("-itsoffset 2.5"), "{joined}");
    assert!(joined.contains("-re -i in.mp4"), "a valueless flag should have no operand token: {joined}");
}

/// An output's extra_args should be appended after everything else that
/// output's section builds (map/codec/metadata/disposition/container), just
/// before the output path -- and an empty map (the default/cleared state)
/// should add nothing at all.
#[test]
fn output_extra_args_are_appended_before_the_output_path() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));
    graph.outputs[0].extra_args = filter_fields(&[("max_interleave_delta", "5000000"), ("movflags", "+faststart")]);

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    assert_eq!(
        args.last().unwrap(),
        &graph.outputs[0].path,
        "the output path must still be the very last argument"
    );
    let joined = args.join(" ");
    assert!(joined.contains("-max_interleave_delta 5000000"), "{joined}");
    assert!(joined.contains("-movflags +faststart output.mkv"), "{joined}");

    graph.outputs[0].extra_args = BTreeMap::new();
    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    assert!(!args.join(" ").contains("-max_interleave_delta"), "clearing extra_args should remove the tokens");
}

/// A Metadata modifier's language/title should show up as -metadata:s:N
/// arguments.
#[test]
fn metadata_modifier_sets_language_and_title_override() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let modifier = graph.add_modifier(ModifierKind::Metadata {
        fields: metadata_fields(&[("language", "eng"), ("title", "Director's Commentary")]),
    });

    let src = Endpoint::Stream { node: id, stream_idx: 0 };
    graph.connect(src, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let joined = args.join(" ");
    assert!(joined.contains("-metadata:s:0 language=eng"), "{joined}");
    assert!(joined.contains("-metadata:s:0 title=Director's Commentary"), "{joined}");
}

/// A Filter modifier should route its stream through a `-filter_complex`
/// label instead of a direct `file:stream` map, and -- unlike every other
/// modifier kind -- must NOT get a bare `-c:<i> copy` default, since ffmpeg
/// rejects stream-copying a filtered stream outright with "Filtering and
/// streamcopy cannot be used together" (see the end-to-end filter tests for
/// proof this actually runs).
#[test]
fn filter_modifier_routes_through_filter_complex_and_skips_copy_default() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let modifier =
        graph.add_modifier(ModifierKind::Filter { name: FilterName::Scale, fields: filter_fields(&[("width", "640")]) });
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let joined = args.join(" ");
    assert!(joined.contains("-filter_complex"), "{joined}");
    assert!(joined.contains("[0:0]scale=w=640:h=-1[f0]"), "{joined}");
    assert!(joined.contains("-map [f0]"), "{joined}");
    assert!(!joined.contains("-c:0 copy"), "a filtered stream must not default to copy: {joined}");
}

/// A Filter node with no fields set yet is a no-op, same as an empty
/// Metadata/Disposition node -- the stream should map directly with no
/// -filter_complex at all, not an empty or broken one.
#[test]
fn unconfigured_filter_modifier_is_a_no_op() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let modifier = graph.add_modifier(ModifierKind::Filter { name: FilterName::Scale, fields: BTreeMap::new() });
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let joined = args.join(" ");
    assert!(!joined.contains("-filter_complex"), "{joined}");
    assert!(joined.contains("-map 0:0"), "{joined}");
    assert!(joined.contains("-c:0 copy"), "{joined}");
}

/// Trim: video routes through `trim=...,setpts=PTS-STARTPTS` (the PTS
/// reset is required -- see `FilterName::expression`'s doc comment -- or
/// the kept segment keeps its original, non-zeroed timestamps and the
/// output ends up with an apparent leading gap); audio uses `atrim`/
/// `asetpts` instead. A node with neither field set is a no-op, same as
/// any other unconfigured filter.
#[test]
fn filter_trim_builds_kind_specific_expression_with_pts_reset() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let modifier = graph.add_modifier(ModifierKind::Filter {
        name: FilterName::Trim,
        fields: filter_fields(&[("start", "1"), ("end", "3")]),
    });
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let joined = args.join(" ");
    assert!(joined.contains("trim=start=1:end=3,setpts=PTS-STARTPTS"), "{joined}");
}

#[test]
fn filter_trim_uses_atrim_and_asetpts_for_audio() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input(
        "in.mp4".to_string(),
        vec![StreamInfo { index: 0, kind: StreamKind::Audio, codec: "aac".to_string(), lang: None, duration: None }],
        Vec::new(),
    );
    let modifier = graph
        .add_modifier(ModifierKind::Filter { name: FilterName::Trim, fields: filter_fields(&[("start", "2")]) });
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let joined = args.join(" ");
    assert!(joined.contains("atrim=start=2,asetpts=PTS-STARTPTS"), "{joined}");
}

/// A stream whose *entire* filter chain is a single configured Trim gets
/// routed through a dedicated, seeked `-i` (see `SeekInputs`) instead of
/// the input node's shared whole-file one -- the fix for a real render
/// that OOM'd (see `concat_of_two_trim_segments_from_the_same_input_
/// renders_both_end_to_end` in `filters_e2e` for the full story). The
/// plain `-i` (index 0) stays for anything else that might need it; the
/// trimmed stream's filter_complex entry should reference a *second* `-i`
/// (index 1), seeked to the trim's own window with a margin added past
/// `end`, and `-copyts` so the trim filter's own absolute start/end values
/// keep meaning what they always meant.
#[test]
fn trim_only_filter_chain_opens_a_dedicated_seeked_input() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let modifier = graph.add_modifier(ModifierKind::Filter {
        name: FilterName::Trim,
        fields: filter_fields(&[("start", "10"), ("end", "20")]),
    });
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let joined = args.join(" ");
    assert!(joined.contains("-ss 10 -to 22 -copyts -i in.mp4"), "{joined}");
    assert!(joined.contains("[1:0]trim=start=10:end=20,setpts=PTS-STARTPTS[f0]"), "{joined}");
    assert!(joined.contains("-map [f0]"), "{joined}");
    // The plain whole-file `-i` is still there, unaffected.
    let i_count = args.iter().filter(|a| *a == "-i").count();
    assert_eq!(i_count, 2, "expected the plain -i plus one dedicated seek -i: {joined}");
}

/// Two different streams of the *same* input trimmed to the identical
/// `[start, end]` window -- e.g. a video and audio segment cut to the same
/// real-time range, the common case a Concat-based cut produces -- should
/// share one dedicated seek `-i` rather than each opening their own.
#[test]
fn identical_trim_windows_on_the_same_input_share_one_dedicated_seek_input() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
    let video_trim = graph.add_modifier(ModifierKind::Filter {
        name: FilterName::Trim,
        fields: filter_fields(&[("start", "5"), ("end", "9")]),
    });
    let audio_trim = graph.add_modifier(ModifierKind::Filter {
        name: FilterName::Trim,
        fields: filter_fields(&[("start", "5"), ("end", "9")]),
    });
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(video_trim));
    graph.connect(Endpoint::Stream { node: id, stream_idx: 1 }, Target::ModifierIn(audio_trim));
    graph.connect(Endpoint::ModifierOut(video_trim), Target::Output(out));
    graph.connect(Endpoint::ModifierOut(audio_trim), Target::Output(out));

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let joined = args.join(" ");
    // Only one dedicated seek input should have been opened for the shared
    // (5, 9) window -- so only the plain -i plus exactly one more.
    let i_count = args.iter().filter(|a| *a == "-i").count();
    assert_eq!(i_count, 2, "expected the two identical trim windows to share one dedicated -i: {joined}");
    assert!(joined.contains("[1:0]trim=start=5:end=9,setpts=PTS-STARTPTS"), "{joined}");
    assert!(joined.contains("[1:1]atrim=start=5:end=9,asetpts=PTS-STARTPTS"), "{joined}");
}

/// A stream with no duration-affecting filter in its chain should report
/// its source's own probed duration unchanged -- Convert/Metadata/
/// Disposition don't touch duration.
#[test]
fn expected_output_duration_passes_through_unaffected_by_non_duration_filters() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input(
        "in.mp4".to_string(),
        vec![StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None, duration: Some(120.0) }],
        Vec::new(),
    );
    let convert = graph.add_modifier(ModifierKind::Convert(Codec::Encode("libx265".to_string())));
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(convert));
    graph.connect(Endpoint::ModifierOut(convert), Target::Output(out));

    assert_eq!(graph.expected_output_duration(out), Some(120.0));
}

/// A Trim with both `start`/`end` set should shorten duration to exactly
/// their difference, regardless of the source's own duration.
#[test]
fn expected_output_duration_reflects_a_trim_window() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input(
        "in.mp4".to_string(),
        vec![StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None, duration: Some(120.0) }],
        Vec::new(),
    );
    let trim = graph.add_modifier(ModifierKind::Filter {
        name: FilterName::Trim,
        fields: filter_fields(&[("start", "10"), ("end", "30")]),
    });
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(trim));
    graph.connect(Endpoint::ModifierOut(trim), Target::Output(out));

    assert_eq!(graph.expected_output_duration(out), Some(20.0));
}

/// A Trim with only `start` set should shorten duration to the source's
/// own duration minus that start (ffmpeg's own default: `end` unset means
/// "to the end").
#[test]
fn expected_output_duration_with_only_a_trim_start_subtracts_it_from_the_source_duration() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input(
        "in.mp4".to_string(),
        vec![StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None, duration: Some(120.0) }],
        Vec::new(),
    );
    let trim = graph.add_modifier(ModifierKind::Filter { name: FilterName::Trim, fields: filter_fields(&[("start", "20")]) });
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(trim));
    graph.connect(Endpoint::ModifierOut(trim), Target::Output(out));

    assert_eq!(graph.expected_output_duration(out), Some(100.0));
}

/// A Concat's duration is the sum of its segments' own resolved durations.
#[test]
fn expected_output_duration_sums_concat_segments() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let a = graph.add_input(
        "a.mp4".to_string(),
        vec![StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None, duration: Some(30.0) }],
        Vec::new(),
    );
    let b = graph.add_input(
        "b.mp4".to_string(),
        vec![StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None, duration: Some(45.0) }],
        Vec::new(),
    );
    let concat = graph.add_modifier(ModifierKind::Concat);
    graph.connect(Endpoint::Stream { node: a, stream_idx: 0 }, Target::ModifierIn(concat));
    graph.connect(Endpoint::Stream { node: b, stream_idx: 0 }, Target::ModifierIn(concat));
    graph.connect(Endpoint::ModifierOut(concat), Target::Output(out));

    assert_eq!(graph.expected_output_duration(out), Some(75.0));
}

/// A Shift extends (positive) or shrinks (negative) total duration by its
/// own `seconds` amount -- see `FilterName::expression`'s doc comment on
/// `Shift` for why it actually does that to the real rendered output, not
/// just conceptually.
#[test]
fn expected_output_duration_reflects_a_shift() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input(
        "in.mp4".to_string(),
        vec![StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None, duration: Some(60.0) }],
        Vec::new(),
    );
    let shift =
        graph.add_modifier(ModifierKind::Filter { name: FilterName::Shift, fields: filter_fields(&[("seconds", "5")]) });
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(shift));
    graph.connect(Endpoint::ModifierOut(shift), Target::Output(out));

    assert_eq!(graph.expected_output_duration(out), Some(65.0));
}

/// When more than one stream is mapped into the same output, its overall
/// duration should be the *longest* of them (ffmpeg, without `-shortest`,
/// keeps encoding until every mapped stream is done) -- not the shortest,
/// the sum, or the first one found.
#[test]
fn expected_output_duration_is_the_longest_of_several_mapped_streams() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input(
        "in.mp4".to_string(),
        vec![
            StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None, duration: Some(30.0) },
            StreamInfo { index: 1, kind: StreamKind::Audio, codec: "aac".to_string(), lang: None, duration: Some(45.0) },
        ],
        Vec::new(),
    );
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));
    graph.connect(Endpoint::Stream { node: id, stream_idx: 1 }, Target::Output(out));

    assert_eq!(graph.expected_output_duration(out), Some(45.0));
}

/// If *any* mapped stream's duration can't be determined, the whole
/// output's estimate should be `None` rather than silently maxing over
/// just the known ones -- the unknown one could be the longest.
#[test]
fn expected_output_duration_is_none_if_any_mapped_stream_duration_is_unknown() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input(
        "in.mp4".to_string(),
        vec![
            StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None, duration: Some(30.0) },
            StreamInfo { index: 1, kind: StreamKind::Audio, codec: "aac".to_string(), lang: None, duration: None },
        ],
        Vec::new(),
    );
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));
    graph.connect(Endpoint::Stream { node: id, stream_idx: 1 }, Target::Output(out));

    assert_eq!(graph.expected_output_duration(out), None);
}

/// Nothing mapped at all should report `None`, not `0.0` -- an empty
/// output isn't "instant," it's just not something a progress bar applies
/// to.
#[test]
fn expected_output_duration_is_none_with_nothing_mapped() {
    let graph = Graph::new();
    let out = graph.outputs[0].id;
    assert_eq!(graph.expected_output_duration(out), None);
}

/// A Trim stacked with another filter on the same stream is more than the
/// simple case `SeekInputs` handles -- it should keep using the input
/// node's shared whole-file `-i`, exactly as before that optimization
/// existed, rather than risk seeking past frames a *different* filter in
/// the chain still needed.
#[test]
fn trim_combined_with_another_filter_keeps_using_the_shared_input() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let trim = graph.add_modifier(ModifierKind::Filter {
        name: FilterName::Trim,
        fields: filter_fields(&[("start", "10"), ("end", "20")]),
    });
    let scale = graph
        .add_modifier(ModifierKind::Filter { name: FilterName::Scale, fields: filter_fields(&[("width", "640")]) });
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(trim));
    graph.connect(Endpoint::ModifierOut(trim), Target::ModifierIn(scale));
    graph.connect(Endpoint::ModifierOut(scale), Target::Output(out));

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let joined = args.join(" ");
    assert!(!joined.contains("-copyts"), "a stacked filter chain shouldn't get a dedicated seek input: {joined}");
    assert!(joined.contains("[0:0]trim=start=10:end=20,setpts=PTS-STARTPTS,scale=w=640:h=-1"), "{joined}");
    let i_count = args.iter().filter(|a| *a == "-i").count();
    assert_eq!(i_count, 1, "expected just the plain -i, no dedicated seek input: {joined}");
}

#[test]
fn unconfigured_trim_modifier_is_a_no_op() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let modifier = graph.add_modifier(ModifierKind::Filter { name: FilterName::Trim, fields: BTreeMap::new() });
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let joined = args.join(" ");
    assert!(!joined.contains("-filter_complex"), "{joined}");
}

/// The fast, copy-eligible alternative to Trim's filter-based cut: `ss`/
/// `to` set as an output's own extra args land after every -map/-c (so
/// they're output, not input, options -- output-scoped, unlike an
/// input-level seek, so they can't leak into a *different* output reading
/// the same input file) and don't force a re-encode the way a
/// `-filter_complex` entry would.
#[test]
fn output_level_ss_to_extra_args_stay_copy_eligible_and_skip_filter_complex() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));
    graph.outputs[0].extra_args = filter_fields(&[("ss", "1"), ("to", "3")]);

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let joined = args.join(" ");
    assert!(!joined.contains("-filter_complex"), "{joined}");
    assert!(joined.contains("-c:0 copy"), "{joined}");
    assert!(joined.contains("-ss 1"), "{joined}");
    assert!(joined.contains("-to 3"), "{joined}");

    // Output options land after -i, unlike an input-level seek.
    let ss = args.iter().position(|a| a == "-ss").expect("expected an -ss arg");
    let i = args.iter().position(|a| a == "-i").expect("expected an -i arg");
    assert!(ss > i, "output-scoped -ss should follow -i, not precede it: {args:?}");
}

/// Two outputs reading the same input file can each set their own `ss`/
/// `to` window independently -- unlike an input-level seek, one output's
/// window has no way to leak into the other's, since each is scoped to its
/// own output section of the command.
#[test]
fn output_level_ss_to_do_not_leak_across_outputs_sharing_an_input() {
    let mut graph = Graph::new();
    let out1 = graph.outputs[0].id;
    let out2 = graph.add_output();
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out1));
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out2));
    graph.outputs[0].extra_args = filter_fields(&[("ss", "1"), ("to", "3")]);
    graph.outputs[1].path = "second.mkv".to_string();
    // out2 deliberately left without any ss/to -- it should render untouched.

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let ss_count = args.iter().filter(|a| a.as_str() == "-ss").count();
    assert_eq!(ss_count, 1, "only the output that actually set ss/to should get one: {args:?}");
}

/// Chaining a Convert node into a Metadata node should combine both
/// effects on the same resolved connection.
#[test]
fn chain_of_convert_then_metadata_combines_both_effects() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let convert = graph.add_modifier(ModifierKind::Convert(Codec::Encode("libx265".to_string())));
    let metadata = graph.add_modifier(ModifierKind::Metadata { fields: metadata_fields(&[("language", "jpn")]) });

    let src = Endpoint::Stream { node: id, stream_idx: 0 };
    graph.connect(src, Target::ModifierIn(convert));
    graph.connect(Endpoint::ModifierOut(convert), Target::ModifierIn(metadata));
    graph.connect(Endpoint::ModifierOut(metadata), Target::Output(out));

    let resolved = graph.resolve(Endpoint::ModifierOut(metadata)).unwrap();
    assert_eq!(*resolved.codec(), Codec::Encode("libx265".to_string()));
    assert_eq!(resolved.metadata().get("language").map(String::as_str), Some("jpn"));
}

/// When two modifiers of the same kind sit in a chain, the one closer to
/// the output should win -- matching a real pipeline where the last stage
/// applied is what actually reaches the file.
#[test]
fn closest_to_output_modifier_wins_on_conflicting_fields() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let first = graph.add_modifier(ModifierKind::Convert(Codec::Encode("libx264".to_string())));
    let second = graph.add_modifier(ModifierKind::Convert(Codec::Encode("libx265".to_string())));

    let src = Endpoint::Stream { node: id, stream_idx: 0 };
    graph.connect(src, Target::ModifierIn(first));
    graph.connect(Endpoint::ModifierOut(first), Target::ModifierIn(second));
    graph.connect(Endpoint::ModifierOut(second), Target::Output(out));

    let resolved = graph.resolve(Endpoint::ModifierOut(second)).unwrap();
    assert_eq!(*resolved.codec(), Codec::Encode("libx265".to_string()), "the modifier closer to the output should win");
}

/// A modifier with nothing feeding its input is a broken chain -- resolving
/// from its output should fail gracefully (not panic), and such a
/// connection should be skipped when building ffmpeg args.
#[test]
fn broken_chain_resolves_to_none_and_is_skipped_in_args() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let modifier = graph.add_modifier(ModifierKind::Convert(Codec::Encode("libx265".to_string())));
    // Connect the modifier's output straight to the output, but never wire
    // anything into the modifier's own input.
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    assert!(graph.resolve(Endpoint::ModifierOut(modifier)).is_none());

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    assert!(!args.contains(&"output.mkv".to_string()), "an output with only a broken chain should be skipped");
}

/// A cycle (two modifiers feeding each other) must never hang or panic --
/// resolve should bail out via its hop guard.
#[test]
fn cyclic_chain_does_not_panic_or_hang() {
    let mut graph = Graph::new();
    let a = graph.add_modifier(ModifierKind::Convert(Codec::Copy));
    let b = graph.add_modifier(ModifierKind::Convert(Codec::Copy));
    graph.connect(Endpoint::ModifierOut(a), Target::ModifierIn(b));
    graph.connect(Endpoint::ModifierOut(b), Target::ModifierIn(a));

    assert!(graph.resolve(Endpoint::ModifierOut(a)).is_none());
    assert!(graph.resolve(Endpoint::ModifierOut(b)).is_none());
}

/// A modifier's input accepts only one connection: wiring a new source in
/// should replace whatever was feeding it before, not add a second one.
#[test]
fn connecting_into_a_modifier_input_replaces_any_existing_wire() {
    let mut graph = Graph::new();
    let modifier = graph.add_modifier(ModifierKind::Convert(Codec::Copy));
    let a = graph.add_input("a.mp4".to_string(), video_stream(), Vec::new());
    let b = graph.add_input("b.mp4".to_string(), video_stream(), Vec::new());

    graph.connect(Endpoint::Stream { node: a, stream_idx: 0 }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::Stream { node: b, stream_idx: 0 }, Target::ModifierIn(modifier));

    let incoming = graph.incoming(Target::ModifierIn(modifier));
    assert_eq!(incoming.len(), 1, "only the most recent connection should remain");
    assert_eq!(graph.wires[incoming[0]].from, Endpoint::Stream { node: b, stream_idx: 0 });
}

/// Connecting the exact same (source, target) pair twice should toggle it
/// off, matching the arm-then-connect UI's "press 'c' again to undo" feel.
#[test]
fn connecting_the_same_pair_twice_toggles_it_off() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let src = Endpoint::Stream { node: id, stream_idx: 0 };

    graph.connect(src, Target::Output(out));
    assert_eq!(graph.wires.len(), 1);
    graph.connect(src, Target::Output(out));
    assert!(graph.wires.is_empty());
}

/// Removing a modifier should drop both its incoming wire and any outgoing
/// ones, leaving the rest of the graph untouched.
#[test]
fn removing_a_modifier_prunes_its_wires_only() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
    let modifier = graph.add_modifier(ModifierKind::Convert(Codec::Copy));

    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));
    // An unrelated direct connection that should survive the removal.
    graph.connect(Endpoint::Stream { node: id, stream_idx: 1 }, Target::Output(out));
    assert_eq!(graph.wires.len(), 3);

    graph.remove_modifier(modifier);

    assert_eq!(graph.wires.len(), 1, "only the unrelated wire should remain: {:?}", graph.wires);
    assert_eq!(graph.wires[0].from, Endpoint::Stream { node: id, stream_idx: 1 });
}

/// Stream specifiers like -c:0 (and -metadata:s:0) are scoped to the
/// *current* output section in ffmpeg's multi-output syntax, so with two
/// outputs each getting their own Convert node, both sections should
/// independently start at -c:0 rather than sharing a global counter.
#[test]
fn ffmpeg_args_use_local_stream_indices_per_output_section() {
    let mut graph = Graph::new();
    let out1 = graph.outputs[0].id;
    let out2 = graph.add_output();
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let c1 = graph.add_modifier(ModifierKind::Convert(Codec::Encode("libx264".to_string())));
    let c2 = graph.add_modifier(ModifierKind::Convert(Codec::Encode("libx265".to_string())));

    let src = Endpoint::Stream { node: id, stream_idx: 0 };
    graph.connect(src, Target::ModifierIn(c1));
    graph.connect(Endpoint::ModifierOut(c1), Target::Output(out1));
    graph.connect(src, Target::ModifierIn(c2));
    graph.connect(Endpoint::ModifierOut(c2), Target::Output(out2));

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let joined = args.join(" ");
    assert!(joined.contains("-c:0 libx264"), "{joined}");
    assert!(joined.contains("-c:0 libx265"), "{joined}");
    assert!(!joined.contains("-c:1"), "each section has only one stream: {joined}");
}

/// Outputs with nothing resolvable mapped to them should be omitted
/// entirely -- otherwise ffmpeg would try to auto-select streams for that
/// path, which isn't what an empty output node means here.
#[test]
fn ffmpeg_args_skip_outputs_with_no_resolvable_connection() {
    let mut graph = Graph::new();
    graph.add_output(); // second output, left empty
    let id = graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(graph.outputs[0].id));

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    assert!(args.contains(&"output.mkv".to_string()));
    assert!(!args.contains(&"output2.mkv".to_string()), "empty output should not appear: {args:?}");
}

/// The teletext decoder's own options (`txt_format`/`txt_page`/
/// `txt_duration`) should only be offered as curated input extra-args when
/// the configured ffmpeg actually has that decoder -- offering them
/// otherwise would just be an option ffmpeg rejects outright as
/// unrecognized, not merely a no-op.
#[test]
fn input_extra_arg_keys_only_include_teletext_options_when_supported() {
    let without = crate::graph::input_extra_arg_keys(false);
    assert!(without.iter().any(|&(k, _, _)| k == "itsoffset"), "{without:?}");
    assert!(!without.iter().any(|&(k, _, _)| k == "txt_format"), "{without:?}");
    assert!(!without.iter().any(|&(k, _, _)| k == "txt_page"), "{without:?}");
    assert!(!without.iter().any(|&(k, _, _)| k == "txt_duration"), "{without:?}");

    let with = crate::graph::input_extra_arg_keys(true);
    assert!(with.iter().any(|&(k, _, _)| k == "itsoffset"), "{with:?}");
    assert!(with.iter().any(|&(k, _, _)| k == "txt_format"), "{with:?}");
    assert!(with.iter().any(|&(k, _, _)| k == "txt_page"), "{with:?}");
    assert!(with.iter().any(|&(k, _, _)| k == "txt_duration"), "{with:?}");
}

/// Unlike every other modifier kind, a Concat node's input accepts any
/// number of wires, appended (not replaced) in the order they're
/// connected -- `Graph::connect`'s doc comment explains why.
#[test]
fn connect_appends_to_a_concat_nodes_input_instead_of_replacing() {
    let mut graph = Graph::new();
    let a = graph.add_input("a.mp4".to_string(), video_stream(), Vec::new());
    let b = graph.add_input("b.mp4".to_string(), video_stream(), Vec::new());
    let concat = graph.add_modifier(ModifierKind::Concat);

    graph.connect(Endpoint::Stream { node: a, stream_idx: 0 }, Target::ModifierIn(concat));
    graph.connect(Endpoint::Stream { node: b, stream_idx: 0 }, Target::ModifierIn(concat));

    let incoming = graph.incoming(Target::ModifierIn(concat));
    assert_eq!(incoming.len(), 2, "both wires should still be there, not just the second");
    assert_eq!(graph.wires[incoming[0]].from, Endpoint::Stream { node: a, stream_idx: 0 });
    assert_eq!(graph.wires[incoming[1]].from, Endpoint::Stream { node: b, stream_idx: 0 });
}

/// Two video segments joined by a Concat node should resolve to a
/// `Resolved::Concat`, and the built ffmpeg args should route both
/// through a single `concat=n=2:v=1:a=0` filter_complex entry, `-map`ped
/// by its output label -- with no `-c:0 copy` default, since a concat
/// (like any filtered stream) can't be stream-copied.
#[test]
fn concat_modifier_joins_two_video_segments_end_to_end_args() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let a = graph.add_input("a.mp4".to_string(), video_stream(), Vec::new());
    let b = graph.add_input("b.mp4".to_string(), video_stream(), Vec::new());
    let concat = graph.add_modifier(ModifierKind::Concat);
    graph.connect(Endpoint::Stream { node: a, stream_idx: 0 }, Target::ModifierIn(concat));
    graph.connect(Endpoint::Stream { node: b, stream_idx: 0 }, Target::ModifierIn(concat));
    graph.connect(Endpoint::ModifierOut(concat), Target::Output(out));

    let resolved = graph.resolve(Endpoint::ModifierOut(concat)).expect("concat should resolve");
    let Resolved::Concat { kind, segments, .. } = &resolved else {
        panic!("expected a Concat resolution");
    };
    assert_eq!(*kind, StreamKind::Video);
    assert_eq!(segments.len(), 2);

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let joined = args.join(" ");
    assert!(joined.contains("-filter_complex"), "{joined}");
    assert!(joined.contains("[0:0][1:0]concat=n=2:v=1:a=0[f0]"), "{joined}");
    assert!(joined.contains("-map [f0]"), "{joined}");
    assert!(!joined.contains("-c:0 copy"), "a concat output must not default to copy: {joined}");
}

/// A Concat node with segments of mismatched kinds (video mixed with
/// audio) can't be expressed as a single `concat` filter call in v1 scope
/// -- `resolve` should treat it as a broken chain, same as any other
/// unsatisfiable connection.
#[test]
fn concat_modifier_with_mismatched_segment_kinds_fails_to_resolve() {
    let mut graph = Graph::new();
    let id = graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
    let concat = graph.add_modifier(ModifierKind::Concat);
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(concat)); // video
    graph.connect(Endpoint::Stream { node: id, stream_idx: 1 }, Target::ModifierIn(concat)); // audio

    assert!(graph.resolve(Endpoint::ModifierOut(concat)).is_none());
}

/// A Concat node with nothing wired into it yet is a broken chain, not an
/// empty/no-op pass-through (there's no such thing as a `concat` filter
/// with zero inputs).
#[test]
fn concat_modifier_with_no_segments_fails_to_resolve() {
    let mut graph = Graph::new();
    let concat = graph.add_modifier(ModifierKind::Concat);
    assert!(graph.resolve(Endpoint::ModifierOut(concat)).is_none());
}

/// Reordering a Concat node's segment wires (see `App::move_focused_row`)
/// should change the join order ffmpeg's `concat` filter actually uses.
#[test]
fn reordering_concat_segments_changes_the_filter_complex_join_order() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let a = graph.add_input("a.mp4".to_string(), video_stream(), Vec::new());
    let b = graph.add_input("b.mp4".to_string(), video_stream(), Vec::new());
    let concat = graph.add_modifier(ModifierKind::Concat);
    graph.connect(Endpoint::Stream { node: a, stream_idx: 0 }, Target::ModifierIn(concat));
    graph.connect(Endpoint::Stream { node: b, stream_idx: 0 }, Target::ModifierIn(concat));
    graph.connect(Endpoint::ModifierOut(concat), Target::Output(out));

    let incoming = graph.incoming(Target::ModifierIn(concat));
    graph.swap_wires(incoming[0], incoming[1]);

    let args = graph.build_ffmpeg_args(&BTreeMap::new());
    let joined = args.join(" ");
    assert!(joined.contains("[1:0][0:0]concat=n=2:v=1:a=0[f0]"), "{joined}");
}

/// A modifier fed by a Concat node's output (e.g. tagging the joined
/// result with metadata) should see `Resolved::Concat` accumulate the
/// same downstream settings a plain `Resolved::Stream` chain would.
#[test]
fn modifier_downstream_of_concat_still_applies_its_own_settings() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let a = graph.add_input("a.mp4".to_string(), video_stream(), Vec::new());
    let b = graph.add_input("b.mp4".to_string(), video_stream(), Vec::new());
    let concat = graph.add_modifier(ModifierKind::Concat);
    graph.connect(Endpoint::Stream { node: a, stream_idx: 0 }, Target::ModifierIn(concat));
    graph.connect(Endpoint::Stream { node: b, stream_idx: 0 }, Target::ModifierIn(concat));

    let convert = graph.add_modifier(ModifierKind::Convert(Codec::Encode("libx265".to_string())));
    graph.connect(Endpoint::ModifierOut(concat), Target::ModifierIn(convert));
    graph.connect(Endpoint::ModifierOut(convert), Target::Output(out));

    let resolved = graph.resolve(Endpoint::ModifierOut(convert)).expect("chain should resolve");
    assert_eq!(*resolved.codec(), Codec::Encode("libx265".to_string()));
    let Resolved::Concat { segments, .. } = &resolved else {
        panic!("expected the chain to still trace back to a Concat resolution");
    };
    assert_eq!(segments.len(), 2);
}


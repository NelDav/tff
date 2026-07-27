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
    assert_eq!(resolved.from_node, id);
    assert_eq!(resolved.from_stream_idx, 0);
    assert_eq!(resolved.codec, Codec::Copy);
    assert!(resolved.metadata.is_empty());

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
    assert_eq!(resolved.codec, Codec::Encode("libx265".to_string()));

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
        vec![StreamInfo { index: 0, kind: StreamKind::Audio, codec: "aac".to_string(), lang: None }],
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
    assert_eq!(resolved.codec, Codec::Encode("libx265".to_string()));
    assert_eq!(resolved.metadata.get("language").map(String::as_str), Some("jpn"));
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
    assert_eq!(resolved.codec, Codec::Encode("libx265".to_string()), "the modifier closer to the output should win");
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


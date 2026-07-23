use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::sync::mpsc;

use crate::ffmpeg;
use crate::graph::{Codec, Endpoint, FilterName, Graph, ModifierKind, StreamInfo, StreamKind, Target};

fn video_stream() -> Vec<StreamInfo> {
    vec![StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None }]
}

fn video_audio_streams() -> Vec<StreamInfo> {
    vec![
        StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None },
        StreamInfo { index: 1, kind: StreamKind::Audio, codec: "aac".to_string(), lang: None },
    ]
}

fn metadata_fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn disposition_set(flags: &[&str]) -> BTreeSet<String> {
    flags.iter().map(|f| f.to_string()).collect()
}

fn filter_fields(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

// ---------------------------------------------------------------------
// Graph: connection model, chain resolution, ffmpeg arg construction
// ---------------------------------------------------------------------

/// A direct input -> output wire, with no modifier in between, should
/// resolve to a plain stream copy -- the sensible default for "just wire
/// it up with no changes".
#[test]
fn direct_wire_resolves_to_stream_copy() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream());
    let src = Endpoint::Stream { node: id, stream_idx: 0 };
    graph.connect(src, Target::Output(out));

    let resolved = graph.resolve(src).expect("direct wire should resolve");
    assert_eq!(resolved.from_node, id);
    assert_eq!(resolved.from_stream_idx, 0);
    assert_eq!(resolved.codec, Codec::Copy);
    assert!(resolved.metadata.is_empty());

    let args = graph.build_ffmpeg_args();
    let joined = args.join(" ");
    assert!(joined.contains("-c:0 copy"), "expected an explicit per-stream copy default: {joined}");
}

/// Routing a stream through a Convert modifier should make that codec show
/// up as a per-stream override in the built ffmpeg args.
#[test]
fn convert_modifier_sets_codec_override() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream());
    let modifier = graph.add_modifier(ModifierKind::Convert(Codec::Encode("libx265".to_string())));

    let src = Endpoint::Stream { node: id, stream_idx: 0 };
    graph.connect(src, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    let resolved = graph.resolve(Endpoint::ModifierOut(modifier)).unwrap();
    assert_eq!(resolved.codec, Codec::Encode("libx265".to_string()));

    let args = graph.build_ffmpeg_args();
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
    let id = graph.add_input("in.mp4".to_string(), video_stream());
    graph.input_mut(id).unwrap().extra_args = filter_fields(&[("itsoffset", "2.5"), ("re", "")]);

    let args = graph.build_ffmpeg_args();
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
    let id = graph.add_input("in.mp4".to_string(), video_stream());
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));
    graph.outputs[0].extra_args = filter_fields(&[("max_interleave_delta", "5000000"), ("movflags", "+faststart")]);

    let args = graph.build_ffmpeg_args();
    assert_eq!(
        args.last().unwrap(),
        &graph.outputs[0].path,
        "the output path must still be the very last argument"
    );
    let joined = args.join(" ");
    assert!(joined.contains("-max_interleave_delta 5000000"), "{joined}");
    assert!(joined.contains("-movflags +faststart output.mkv"), "{joined}");

    graph.outputs[0].extra_args = BTreeMap::new();
    let args = graph.build_ffmpeg_args();
    assert!(!args.join(" ").contains("-max_interleave_delta"), "clearing extra_args should remove the tokens");
}

/// A Metadata modifier's language/title should show up as -metadata:s:N
/// arguments.
#[test]
fn metadata_modifier_sets_language_and_title_override() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream());
    let modifier = graph.add_modifier(ModifierKind::Metadata {
        fields: metadata_fields(&[("language", "eng"), ("title", "Director's Commentary")]),
    });

    let src = Endpoint::Stream { node: id, stream_idx: 0 };
    graph.connect(src, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    let args = graph.build_ffmpeg_args();
    let joined = args.join(" ");
    assert!(joined.contains("-metadata:s:0 language=eng"), "{joined}");
    assert!(joined.contains("-metadata:s:0 title=Director's Commentary"), "{joined}");
}

/// A Filter modifier should route its stream through a `-filter_complex`
/// label instead of a direct `file:stream` map, and -- unlike every other
/// modifier kind -- must NOT get a bare `-c:<i> copy` default, since ffmpeg
/// rejects stream-copying a filtered stream outright. This is a pure
/// arg-string regression test for a real bug hit while building this: the
/// old blanket `-c copy` broke every filtered output with "Filtering and
/// streamcopy cannot be used together" (see the end-to-end filter tests for
/// proof the fixed version actually runs).
#[test]
fn filter_modifier_routes_through_filter_complex_and_skips_copy_default() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream());
    let modifier =
        graph.add_modifier(ModifierKind::Filter { name: FilterName::Scale, fields: filter_fields(&[("width", "640")]) });
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    let args = graph.build_ffmpeg_args();
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
    let id = graph.add_input("in.mp4".to_string(), video_stream());
    let modifier = graph.add_modifier(ModifierKind::Filter { name: FilterName::Scale, fields: BTreeMap::new() });
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    let args = graph.build_ffmpeg_args();
    let joined = args.join(" ");
    assert!(!joined.contains("-filter_complex"), "{joined}");
    assert!(joined.contains("-map 0:0"), "{joined}");
    assert!(joined.contains("-c:0 copy"), "{joined}");
}

/// Chaining a Convert node into a Metadata node should combine both
/// effects on the same resolved connection.
#[test]
fn chain_of_convert_then_metadata_combines_both_effects() {
    let mut graph = Graph::new();
    let out = graph.outputs[0].id;
    let id = graph.add_input("in.mp4".to_string(), video_stream());
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
    let id = graph.add_input("in.mp4".to_string(), video_stream());
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

    let args = graph.build_ffmpeg_args();
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
    let a = graph.add_input("a.mp4".to_string(), video_stream());
    let b = graph.add_input("b.mp4".to_string(), video_stream());

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
    let id = graph.add_input("in.mp4".to_string(), video_stream());
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
    let id = graph.add_input("in.mp4".to_string(), video_audio_streams());
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
    let id = graph.add_input("in.mp4".to_string(), video_stream());
    let c1 = graph.add_modifier(ModifierKind::Convert(Codec::Encode("libx264".to_string())));
    let c2 = graph.add_modifier(ModifierKind::Convert(Codec::Encode("libx265".to_string())));

    let src = Endpoint::Stream { node: id, stream_idx: 0 };
    graph.connect(src, Target::ModifierIn(c1));
    graph.connect(Endpoint::ModifierOut(c1), Target::Output(out1));
    graph.connect(src, Target::ModifierIn(c2));
    graph.connect(Endpoint::ModifierOut(c2), Target::Output(out2));

    let args = graph.build_ffmpeg_args();
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
    let id = graph.add_input("in.mp4".to_string(), video_stream());
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(graph.outputs[0].id));

    let args = graph.build_ffmpeg_args();
    assert!(args.contains(&"output.mkv".to_string()));
    assert!(!args.contains(&"output2.mkv".to_string()), "empty output should not appear: {args:?}");
}

// ---------------------------------------------------------------------
// App: arming/connecting, adding/editing/deleting modifier nodes
// ---------------------------------------------------------------------

/// Arming an input stream, then focusing a modifier and pressing 'c'
/// should wire the stream into that modifier's input and clear armed
/// state.
#[test]
fn toggle_connect_wires_armed_stream_into_focused_modifier() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    let modifier = app.graph.add_modifier(ModifierKind::Convert(Codec::Copy));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();

    app.focus = Focus::Input(0);
    app.row_idx = 0;
    app.toggle_connect(); // arm the stream
    assert!(app.armed.is_some());

    app.focus = Focus::Modifier(modifier_idx);
    app.toggle_connect(); // connect

    assert!(app.armed.is_none());
    let incoming = app.graph.incoming(Target::ModifierIn(modifier));
    assert_eq!(incoming.len(), 1);
    assert_eq!(app.graph.wires[incoming[0]].from, Endpoint::Stream { node: id, stream_idx: 0 });
}

/// Once a modifier's input is fed, focusing it and pressing 'c' with
/// nothing else armed should arm *its* output, so the chain can continue
/// to another node.
#[test]
fn toggle_connect_arms_a_connected_modifiers_output() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    let modifier = app.graph.add_modifier(ModifierKind::Convert(Codec::Copy));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));

    app.focus = Focus::Modifier(modifier_idx);
    app.toggle_connect();

    assert_eq!(app.armed, Some(Endpoint::ModifierOut(modifier)));
}

/// Pressing 'c' twice on the same modifier with nothing else armed should
/// arm then disarm it, mirroring how a single input port toggles.
#[test]
fn toggle_connect_twice_on_same_modifier_disarms() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::Convert(Codec::Copy));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();

    app.focus = Focus::Modifier(modifier_idx);
    app.toggle_connect();
    assert!(app.armed.is_some());
    app.toggle_connect();
    assert!(app.armed.is_none());
}

/// 'd' on an input port disconnects it from everything downstream (bulk);
/// 'd' on a modifier or output row disconnects just that one selected
/// connection (precise).
#[test]
fn disconnect_is_bulk_on_input_and_precise_elsewhere() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let out1 = app.graph.outputs[0].id;
    let out2 = app.graph.add_output();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    let src = Endpoint::Stream { node: id, stream_idx: 0 };
    app.graph.connect(src, Target::Output(out1));
    app.graph.connect(src, Target::Output(out2));
    assert_eq!(app.graph.wires.len(), 2);

    // Precise: disconnecting via the out2 row only removes that one wire.
    let out2_idx = app.graph.outputs.iter().position(|o| o.id == out2).unwrap();
    app.focus = Focus::Output(out2_idx);
    app.row_idx = 0;
    app.disconnect_focused();
    assert_eq!(app.graph.wires.len(), 1);
    assert_eq!(app.graph.wires[0].to, Target::Output(out1));

    // Reconnect, then verify the bulk path from the input side.
    app.graph.connect(src, Target::Output(out2));
    assert_eq!(app.graph.wires.len(), 2);
    app.focus = Focus::Input(0);
    app.row_idx = 0;
    app.disconnect_focused();
    assert!(app.graph.wires.is_empty());
}

/// 'a' should open a picker; confirming "convert" should add a Convert
/// modifier node (defaulting to Copy) and focus it.
#[test]
fn add_modifier_picker_confirm_convert_creates_and_focuses_node() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    app.open_add_node_picker();
    let idx = match &app.mode {
        Mode::Picker { options, .. } => {
            options.iter().position(|o| o.value.as_deref() == Some("convert")).unwrap()
        }
        _ => panic!("expected picker mode"),
    };
    if let Mode::Picker { selected, .. } = &mut app.mode {
        *selected = idx;
    }
    app.picker_confirm();

    assert_eq!(app.graph.modifiers.len(), 1);
    assert!(matches!(app.graph.modifiers[0].kind, ModifierKind::Convert(Codec::Copy)));
    assert!(matches!(app.focus, Focus::Modifier(0)));
}

/// Confirming "metadata" should add a Metadata node with both fields unset.
#[test]
fn add_modifier_picker_confirm_metadata_creates_node() {
    use crate::app::{App, Mode};

    let mut app = App::new();
    app.open_add_node_picker();
    let idx = match &app.mode {
        Mode::Picker { options, .. } => {
            options.iter().position(|o| o.value.as_deref() == Some("metadata")).unwrap()
        }
        _ => panic!("expected picker mode"),
    };
    if let Mode::Picker { selected, .. } = &mut app.mode {
        *selected = idx;
    }
    app.picker_confirm();

    assert_eq!(app.graph.modifiers.len(), 1);
    assert!(matches!(
        &app.graph.modifiers[0].kind,
        ModifierKind::Metadata { fields } if fields.is_empty()
    ));
}

/// 'e' on a connected Convert node should open the codec picker, scoped to
/// the kind of stream actually flowing into it, with the current codec
/// preselected.
#[test]
fn activate_modifier_opens_codec_picker_for_connected_convert_node() {
    use crate::app::{App, Focus, Mode, PickerKind};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    let modifier = app.graph.add_modifier(ModifierKind::Convert(Codec::Encode("libx265".to_string())));
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier();

    let Mode::Picker { kind, options, selected, .. } = &app.mode else {
        panic!("expected picker mode");
    };
    assert!(matches!(kind, PickerKind::Codec { .. }));
    assert!(options.iter().any(|o| o.value.as_deref() == Some("libx264")), "should offer video encoders");
    assert!(!options.iter().any(|o| o.value.as_deref() == Some("aac")), "should not offer audio encoders");
    assert_eq!(options[*selected].value.as_deref(), Some("libx265"));
}

/// 'e' on a Convert node with nothing feeding it should refuse -- there's
/// no stream kind to filter codec choices by yet.
#[test]
fn activate_modifier_refuses_codec_picker_when_unconnected() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::Convert(Codec::Copy));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier();

    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.log.last().unwrap().contains("connect this node's input first"));
}

/// 'e' on a Metadata node should open a picker listing the curated fields
/// for the connected stream's kind, each showing its current value (or
/// "(not set)"), with a "custom key..." entry at the end.
#[test]
fn activate_modifier_on_metadata_opens_key_picker_with_current_values() {
    use crate::app::{App, Focus, Mode, PickerKind};

    let mut app = App::new();
    let modifier =
        app.graph.add_modifier(ModifierKind::Metadata { fields: metadata_fields(&[("language", "eng")]) });
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier();

    let Mode::Picker { kind, options, .. } = &app.mode else {
        panic!("expected picker mode");
    };
    assert!(matches!(kind, PickerKind::MetadataKey { .. }));
    let displays: Vec<&String> = options.iter().map(|o| &o.display).collect();
    assert!(options.iter().any(|o| o.display == "language: eng"), "{displays:?}");
    assert!(options.iter().any(|o| o.display == "title: (not set)"));
    assert!(options.iter().any(|o| o.display == "handler_name: (not set)"));
    assert_eq!(options.last().unwrap().display, "custom key…", "custom-key escape hatch should be last");
    assert!(options.last().unwrap().value.is_none());
}

/// 'e' on a Disposition node should open a picker listing every curated
/// flag with a checkbox reflecting whether it's currently set.
#[test]
fn activate_modifier_on_disposition_opens_flag_picker_with_checkboxes() {
    use crate::app::{App, Focus, Mode, PickerKind};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::Disposition { flags: disposition_set(&["forced"]) });
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier();

    let Mode::Picker { kind, options, .. } = &app.mode else {
        panic!("expected picker mode");
    };
    assert!(matches!(kind, PickerKind::DispositionFlags { .. }));
    let displays: Vec<&String> = options.iter().map(|o| &o.display).collect();
    assert!(options.iter().any(|o| o.display == "[x] forced"), "{displays:?}");
    assert!(options.iter().any(|o| o.display == "[ ] default"), "{displays:?}");
}

/// Confirming a disposition flag should toggle it in the graph and keep the
/// picker open with the checkbox flipped -- unlike every other picker kind,
/// this one is a multi-select, so Enter shouldn't close it. Toggling the
/// same flag again should turn it back off.
#[test]
fn disposition_picker_confirm_toggles_flag_and_stays_open() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::Disposition { flags: BTreeSet::new() });
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);
    app.activate_modifier();

    let Mode::Picker { options, .. } = &app.mode else { panic!("expected picker mode") };
    let default_row = options.iter().position(|o| o.display == "[ ] default").unwrap();
    // Move onto "default"'s row (picker opens with row 0 selected).
    app.picker_move(default_row as isize);

    app.picker_confirm();

    assert!(matches!(app.mode, Mode::Picker { .. }), "picker should stay open after toggling");
    let Some(m) = app.graph.modifier(modifier) else { panic!("modifier disappeared") };
    let ModifierKind::Disposition { flags } = &m.kind else { panic!("wrong kind") };
    assert!(flags.contains("default"), "flag should now be set: {flags:?}");
    let Mode::Picker { options, .. } = &app.mode else { unreachable!() };
    assert!(options.iter().any(|o| o.display == "[x] default"), "checkbox should reflect the toggle");

    app.picker_confirm(); // toggle it back off

    let Some(m) = app.graph.modifier(modifier) else { panic!("modifier disappeared") };
    let ModifierKind::Disposition { flags } = &m.kind else { panic!("wrong kind") };
    assert!(!flags.contains("default"), "flag should be cleared again: {flags:?}");
}

/// 'e' on an unconnected Filter node should refuse, mirroring how Convert's
/// codec picker refuses until its input is wired up.
#[test]
fn activate_modifier_refuses_filter_field_picker_when_unconnected() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::Filter { name: FilterName::Scale, fields: BTreeMap::new() });
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier();

    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.log.last().unwrap().contains("connect this node's input first"));
}

/// 'e' on a Filter node connected to a stream kind it doesn't apply to
/// (Scale on an audio stream) should refuse with a clear reason instead of
/// opening a picker for parameters that would silently do nothing.
#[test]
fn activate_modifier_refuses_filter_field_picker_for_wrong_stream_kind() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_audio_streams());
    let modifier = app.graph.add_modifier(ModifierKind::Filter { name: FilterName::Scale, fields: BTreeMap::new() });
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 1 }, Target::ModifierIn(modifier)); // audio
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier();

    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.log.last().unwrap().contains("doesn't apply to audio streams"));
}

/// 'e' on a connected, kind-appropriate Filter node should open a picker
/// listing exactly that filter's curated fields -- and, unlike Metadata's
/// picker, no "custom key..." escape hatch.
#[test]
fn activate_modifier_on_filter_opens_field_picker_for_its_kind() {
    use crate::app::{App, Focus, Mode, PickerKind};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    let modifier = app.graph.add_modifier(ModifierKind::Filter {
        name: FilterName::Crop,
        fields: filter_fields(&[("width", "640")]),
    });
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier();

    let Mode::Picker { kind, title, options, .. } = &app.mode else {
        panic!("expected picker mode");
    };
    assert!(matches!(kind, PickerKind::FilterField { .. }));
    assert_eq!(title, "crop: choose field");
    let displays: Vec<&String> = options.iter().map(|o| &o.display).collect();
    assert!(options.iter().any(|o| o.display == "width: 640"), "{displays:?}");
    assert!(options.iter().any(|o| o.display == "height: (not set)"), "{displays:?}");
    assert!(options.iter().any(|o| o.display == "x: (not set)"), "{displays:?}");
    assert!(options.iter().any(|o| o.display == "y: (not set)"), "{displays:?}");
    assert!(!options.iter().any(|o| o.display.contains("custom key")), "Filter fields have no custom-key escape hatch");
}

/// Picking a field from the Filter picker should open a value text input,
/// and confirming it should store the value on the graph -- the same
/// round trip as Metadata's field editing.
#[test]
fn filter_field_picker_confirm_opens_value_input_and_stores_it() {
    use crate::app::{App, Focus, Mode, TextTarget};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    let modifier = app.graph.add_modifier(ModifierKind::Filter { name: FilterName::Scale, fields: BTreeMap::new() });
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier();
    let idx = match &app.mode {
        Mode::Picker { options, .. } => options.iter().position(|o| o.value.as_deref() == Some("width")).unwrap(),
        _ => panic!("expected picker mode"),
    };
    app.picker_move(idx as isize);
    app.picker_confirm();

    let Mode::TextInput { target, buffer, .. } = &app.mode else {
        panic!("expected text input mode");
    };
    assert!(matches!(target, TextTarget::ModifierFilterValue { key, .. } if key == "width"));
    assert_eq!(buffer, "");

    for c in "1280".chars() {
        app.text_input_char(c);
    }
    app.confirm_text_input();

    let Some(m) = app.graph.modifier(modifier) else { panic!("modifier disappeared") };
    let ModifierKind::Filter { fields, .. } = &m.kind else { panic!("wrong kind") };
    assert_eq!(fields.get("width"), Some(&"1280".to_string()));
}

/// A field with a fixed set of valid values (Rotate's "direction") should
/// offer a selection picker instead of free-text entry -- ffmpeg only
/// accepts a handful of exact strings there, so anything else typed is
/// simply wrong, not just unusual.
#[test]
fn filter_field_with_fixed_values_opens_a_selection_picker_not_free_text() {
    use crate::app::{App, Focus, Mode, PickerKind};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    let modifier = app.graph.add_modifier(ModifierKind::Filter { name: FilterName::Rotate, fields: BTreeMap::new() });
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier(); // opens the field picker (just "direction" for Rotate)
    app.picker_confirm(); // pick "direction"

    let Mode::Picker { kind, title, options, .. } = &app.mode else {
        panic!("expected a second picker for the value, not text input");
    };
    assert!(matches!(kind, PickerKind::FilterFieldValue { .. }));
    assert_eq!(title, "rotate: direction");
    let displays: Vec<&String> = options.iter().map(|o| &o.display).collect();
    assert_eq!(displays, vec!["(not set)", "90cw", "90ccw", "180"]);

    let idx = options.iter().position(|o| o.value.as_deref() == Some("90cw")).unwrap();
    app.picker_move(idx as isize);
    app.picker_confirm();

    assert!(matches!(app.mode, Mode::Normal), "picking a value should close back to normal, not stay open");
    let Some(m) = app.graph.modifier(modifier) else { panic!("modifier disappeared") };
    let ModifierKind::Filter { fields, .. } = &m.kind else { panic!("wrong kind") };
    assert_eq!(fields.get("direction"), Some(&"90cw".to_string()));
}

/// The value-selection picker's "(not set)" entry should clear the field,
/// mirroring how the Codec/Container pickers' reset entry works.
#[test]
fn filter_field_value_picker_reset_entry_clears_the_field() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    let modifier = app.graph.add_modifier(ModifierKind::Filter {
        name: FilterName::Rotate,
        fields: filter_fields(&[("direction", "180")]),
    });
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier();
    app.picker_confirm(); // pick "direction"

    let Mode::Picker { options, selected, .. } = &app.mode else { panic!("expected picker mode") };
    assert_eq!(options[*selected].display, "180", "should preselect the current value");
    let reset_idx = options.iter().position(|o| o.value.is_none()).unwrap();
    app.picker_move(reset_idx as isize - *selected as isize);

    app.picker_confirm();

    let Some(m) = app.graph.modifier(modifier) else { panic!("modifier disappeared") };
    let ModifierKind::Filter { fields, .. } = &m.kind else { panic!("wrong kind") };
    assert!(!fields.contains_key("direction"), "direction should be cleared: {fields:?}");
}

/// Selecting a curated key from the metadata picker should open a value
/// text input, pre-filled with the current value if one is set.
#[test]
fn metadata_key_picker_confirm_opens_prefilled_value_input() {
    use crate::app::{App, Focus, Mode, TextTarget};

    let mut app = App::new();
    let modifier =
        app.graph.add_modifier(ModifierKind::Metadata { fields: metadata_fields(&[("language", "eng")]) });
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier();
    let idx = match &app.mode {
        Mode::Picker { options, .. } => options.iter().position(|o| o.value.as_deref() == Some("language")).unwrap(),
        _ => panic!("expected picker mode"),
    };
    if let Mode::Picker { selected, .. } = &mut app.mode {
        *selected = idx;
    }
    app.picker_confirm();

    let Mode::TextInput { target, buffer, .. } = &app.mode else {
        panic!("expected text input mode");
    };
    assert!(matches!(target, TextTarget::ModifierMetadataValue { modifier: m, key } if *m == modifier && key == "language"));
    assert_eq!(buffer, "eng", "should pre-fill the current value");
}

/// Selecting an unset curated key should open an empty value input.
#[test]
fn metadata_key_picker_confirm_on_unset_field_opens_empty_input() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::Metadata { fields: BTreeMap::new() });
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier();
    let idx = match &app.mode {
        Mode::Picker { options, .. } => options.iter().position(|o| o.value.as_deref() == Some("title")).unwrap(),
        _ => panic!("expected picker mode"),
    };
    if let Mode::Picker { selected, .. } = &mut app.mode {
        *selected = idx;
    }
    app.picker_confirm();

    let Mode::TextInput { buffer, .. } = &app.mode else { panic!("expected text input mode") };
    assert!(buffer.is_empty());
}

/// Choosing "custom key..." should first prompt for the key name, then
/// (after confirming that) open the value prompt for whatever key was
/// typed -- letting the user set a field outside the curated list (e.g.
/// the unreliable-but-sometimes-useful "rotate").
#[test]
fn metadata_custom_key_flow_prompts_for_key_then_value() {
    use crate::app::{App, Focus, Mode, TextTarget};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::Metadata { fields: BTreeMap::new() });
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier();
    let custom_idx = match &app.mode {
        Mode::Picker { options, .. } => options.iter().position(|o| o.value.is_none()).unwrap(),
        _ => panic!("expected picker mode"),
    };
    if let Mode::Picker { selected, .. } = &mut app.mode {
        *selected = custom_idx;
    }
    app.picker_confirm();
    assert!(matches!(&app.mode, Mode::TextInput { target: TextTarget::ModifierCustomKey(m), .. } if *m == modifier));

    for c in "rotate".chars() {
        app.text_input_char(c);
    }
    app.confirm_text_input();

    let Mode::TextInput { target, buffer, .. } = &app.mode else {
        panic!("expected the value prompt to open next");
    };
    assert!(matches!(target, TextTarget::ModifierMetadataValue { modifier: m, key } if *m == modifier && key == "rotate"));
    assert!(buffer.is_empty());

    for c in "90".chars() {
        app.text_input_char(c);
    }
    app.confirm_text_input();

    let ModifierKind::Metadata { fields } = &app.graph.modifier_mut(modifier).unwrap().kind else { unreachable!() };
    assert_eq!(fields.get("rotate").map(String::as_str), Some("90"));
}

/// Confirming a field's value text input should set it (or, for an empty
/// value, clear/remove it from the map).
#[test]
fn confirm_text_input_sets_and_clears_a_chosen_metadata_field() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::Metadata { fields: BTreeMap::new() });
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    let pick = |app: &mut App, key: &str| {
        app.activate_modifier();
        let idx = match &app.mode {
            Mode::Picker { options, .. } => options.iter().position(|o| o.value.as_deref() == Some(key)).unwrap(),
            _ => panic!("expected picker mode"),
        };
        if let Mode::Picker { selected, .. } = &mut app.mode {
            *selected = idx;
        }
        app.picker_confirm();
    };

    pick(&mut app, "language");
    for c in "fra".chars() {
        app.text_input_char(c);
    }
    app.confirm_text_input();
    let ModifierKind::Metadata { fields } = &app.graph.modifier_mut(modifier).unwrap().kind else { unreachable!() };
    assert_eq!(fields.get("language").map(String::as_str), Some("fra"));

    pick(&mut app, "title");
    for c in "Behind the Scenes".chars() {
        app.text_input_char(c);
    }
    app.confirm_text_input();
    let ModifierKind::Metadata { fields } = &app.graph.modifier_mut(modifier).unwrap().kind else { unreachable!() };
    assert_eq!(fields.get("title").map(String::as_str), Some("Behind the Scenes"));
    assert_eq!(fields.len(), 2, "both fields should coexist");

    // Clearing: re-pick "language" (buffer pre-fills with "fra"), empty it,
    // then confirm -- the key should be removed from the map entirely.
    pick(&mut app, "language");
    for _ in 0.."fra".len() {
        app.text_input_backspace();
    }
    app.confirm_text_input();
    let ModifierKind::Metadata { fields } = &app.graph.modifier_mut(modifier).unwrap().kind else { unreachable!() };
    assert!(fields.get("language").is_none(), "empty input should remove the key");
    assert_eq!(fields.len(), 1, "title should be unaffected");
}

/// Deleting a focused modifier should remove it and its wires, without
/// touching sibling modifiers or nodes.
#[test]
fn delete_focused_node_on_modifier_removes_it() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    let modifier = app.graph.add_modifier(ModifierKind::Convert(Codec::Copy));
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    app.graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();

    app.focus = Focus::Modifier(modifier_idx);
    app.delete_focused_node();

    assert!(app.graph.modifiers.is_empty());
    assert!(app.graph.wires.is_empty());
}

/// The app should always keep at least one output -- ffmpeg needs
/// somewhere to write to -- so deleting the last one should refuse.
#[test]
fn delete_focused_node_refuses_to_remove_last_output() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    assert_eq!(app.graph.outputs.len(), 1);
    app.focus = Focus::Output(0);

    app.delete_focused_node();

    assert_eq!(app.graph.outputs.len(), 1);
    assert!(app.log.last().unwrap().contains("can't remove the last output"));
}

/// 'O' should add a new output node and focus it, positioned after every
/// input and modifier in tab order.
#[test]
fn add_output_node_appends_and_focuses_it() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    app.graph.add_modifier(ModifierKind::Convert(Codec::Copy));

    app.add_output_node();

    assert_eq!(app.graph.outputs.len(), 2);
    assert!(matches!(app.focus, Focus::Output(1)));
}

/// Picker escape/search machinery is generic over picker kind -- verify it
/// still works correctly when opened via the new modifier-based codec
/// picker (rather than the old direct per-edge one).
#[test]
fn picker_search_and_escape_work_through_convert_modifier_flow() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    let modifier = app.graph.add_modifier(ModifierKind::Convert(Codec::Copy));
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier();
    app.picker_start_search();
    for c in "265".chars() {
        app.picker_search_char(c);
    }
    app.picker_confirm_search();
    app.picker_confirm();

    let ModifierKind::Convert(codec) = &app.graph.modifier_mut(modifier).unwrap().kind else { unreachable!() };
    assert_eq!(*codec, Codec::Encode("libx265".to_string()));

    // Escape semantics: open again, apply a filter, first Esc clears it.
    app.activate_modifier();
    app.picker_start_search();
    app.picker_search_char('x');
    app.picker_confirm_search();
    app.picker_escape();
    assert!(matches!(app.mode, Mode::Picker { .. }), "first Esc should only clear the filter");
    app.picker_escape();
    assert!(matches!(app.mode, Mode::Normal), "second Esc should close the picker");
}

/// 'f' with an input node focused (nothing to pick a container for) should
/// decline rather than silently doing nothing unexplained.
#[test]
fn container_picker_refuses_without_a_focused_output() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    app.graph.add_input("in.mp4".to_string(), video_stream());
    app.focus = Focus::Input(0);

    app.open_container_picker();

    assert!(matches!(app.mode, Mode::Normal));
    assert!(app.log.last().unwrap().contains("focus an output"));
}

/// Confirming a container choice should set an explicit -f override and,
/// for a recognized common container, rewrite the output path's extension
/// for convenience -- and that override should actually reach ffmpeg's args.
#[test]
fn container_picker_confirm_sets_override_and_rewrites_known_extension() {
    use crate::app::{App, Mode};

    let mut app = App::new(); // focus defaults to Focus::Output(0)
    let out = app.graph.outputs[0].id;
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));

    app.open_container_picker();
    let idx = match &app.mode {
        Mode::Picker { options, .. } => options
            .iter()
            .position(|o| o.value.as_deref() == Some("webm"))
            .expect("webm should be among the offered containers"),
        _ => panic!("expected picker mode"),
    };
    if let Mode::Picker { selected, .. } = &mut app.mode {
        *selected = idx;
    }
    app.picker_confirm();

    assert_eq!(app.graph.outputs[0].container.as_deref(), Some("webm"));
    assert_eq!(app.graph.outputs[0].path, "output.webm");
    assert!(app.graph.build_ffmpeg_args().windows(2).any(|w| w == ["-f", "webm"]));
}

/// The pure filter predicate: case-insensitive substring match, empty query
/// matches everything.
#[test]
fn filtered_indices_matches_case_insensitive_substring() {
    use crate::app::{filtered_indices, PickerEntry};

    let options = vec![
        PickerEntry { display: "copy (no re-encode)".to_string(), value: None },
        PickerEntry { display: "libx264".to_string(), value: Some("libx264".to_string()) },
        PickerEntry { display: "libx265".to_string(), value: Some("libx265".to_string()) },
        PickerEntry { display: "libvpx-vp9".to_string(), value: Some("libvpx-vp9".to_string()) },
    ];

    assert_eq!(filtered_indices(&options, ""), vec![0, 1, 2, 3]);
    assert_eq!(filtered_indices(&options, "X26"), vec![1, 2], "should be case-insensitive");
    assert_eq!(filtered_indices(&options, "vp9"), vec![3]);
    assert!(filtered_indices(&options, "nonexistent").is_empty());
}

/// 'p' with an input or modifier focused (rather than an output) should
/// refuse with a hint instead of silently doing nothing.
#[test]
fn start_preview_requires_an_output_focused() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    let idx = app.graph.inputs.iter().position(|n| n.id == id).unwrap();
    app.focus = Focus::Input(idx);

    app.start_preview();

    assert!(!app.running);
    assert!(app.log.last().unwrap().contains("focus an output node first"));
}

/// 'p' on a focused output with nothing mapped to it should refuse with a
/// hint, the same way 'r' does for a graph with no wires at all.
#[test]
fn start_preview_requires_something_mapped_to_the_output() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    app.focus = Focus::Output(0);

    app.start_preview();

    assert!(!app.running);
    assert!(app.log.last().unwrap().contains("nothing mapped"));
}

/// 'p' while a render or another preview is already in flight should
/// refuse rather than stomp on it with a second concurrent ffmpeg job.
#[test]
fn start_preview_refuses_while_a_job_is_already_running() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));
    app.focus = Focus::Output(0);

    app.start_preview();
    assert!(app.running, "first preview should have started a job");
    let log_len = app.log.len();

    app.start_preview();
    assert_eq!(app.log.len(), log_len + 1);
    assert!(app.log.last().unwrap().contains("already running"));
}

/// A successful preview render should hand the finished path off via
/// `preview_ready` rather than playing it directly -- App has no terminal
/// to play it on (only main.rs does), so `poll_ffmpeg` must not call
/// `ffmpeg::play`/`play_in_terminal` itself, only stage the path for
/// main.rs to pick up.
#[test]
fn poll_ffmpeg_hands_off_a_finished_preview_via_preview_ready() {
    use crate::app::{App, Focus};

    let dir = std::env::temp_dir().join(format!("tff-test-preview-handoff-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = make_test_source(&dir, 1, 160, 120);

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap();
    let id = app.graph.add_input(source_path.to_str().unwrap().to_string(), streams);
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));
    app.focus = Focus::Output(0);

    app.start_preview();
    assert!(app.running);
    assert!(app.preview_ready.is_none());

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.running && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
        app.poll_ffmpeg();
    }

    assert!(!app.running, "preview render did not finish in time");
    assert_eq!(app.status, "preview ready");
    let path = app.preview_ready.take().expect("preview_ready should hold the finished file's path");
    assert!(std::path::Path::new(&path).exists(), "the path handed off should actually exist: {path}");

    let _ = std::fs::remove_file(&path); // lands in the global temp dir, not `dir`
    let _ = std::fs::remove_dir_all(&dir);
}

/// 'e' with a modifier focused should dispatch to the same thing
/// `activate_modifier` does directly -- one key, "edit this node,"
/// regardless of what kind of node is focused.
#[test]
fn activate_focused_on_modifier_dispatches_to_activate_modifier() {
    use crate::app::{App, Focus, Mode, PickerKind};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    let modifier = app.graph.add_modifier(ModifierKind::Convert(Codec::Copy));
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_focused();

    let Mode::Picker { kind, .. } = &app.mode else { panic!("expected picker mode") };
    assert!(matches!(kind, PickerKind::Codec { .. }), "expected the codec picker, same as activate_modifier");
}

/// 'e' on a focused input should open a picker listing every curated input
/// flag, with a checkbox for the valueless one (`re`) and "key: value" for
/// the rest, reflecting whatever's already set.
#[test]
fn activate_focused_on_input_opens_extra_args_field_picker_with_current_values() {
    use crate::app::{App, Focus, Mode, PickerKind};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    app.graph.input_mut(id).unwrap().extra_args = filter_fields(&[("itsoffset", "1.0")]);
    let idx = app.graph.inputs.iter().position(|n| n.id == id).unwrap();
    app.focus = Focus::Input(idx);

    app.activate_focused();

    let Mode::Picker { kind, options, .. } = &app.mode else { panic!("expected picker mode") };
    assert!(matches!(kind, PickerKind::ExtraArgField { target } if matches!(target, crate::app::ExtraArgsTarget::Input(nid) if *nid == id)));
    let displays: Vec<&String> = options.iter().map(|o| &o.display).collect();
    assert!(options.iter().any(|o| o.display == "itsoffset: 1.0"), "{displays:?}");
    assert!(options.iter().any(|o| o.display == "stream_loop: (not set)"), "{displays:?}");
    assert!(options.iter().any(|o| o.display == "[ ] re"), "{displays:?}");
    assert_eq!(options.last().unwrap().display, "custom key…");
}

/// 'e' on a focused output should behave the same way (same underlying
/// flow, different curated list and graph accessor).
#[test]
fn activate_focused_on_output_opens_extra_args_field_picker_with_current_values() {
    use crate::app::{App, Focus, Mode, PickerKind};

    let mut app = App::new();
    app.graph.outputs[0].extra_args = filter_fields(&[("max_interleave_delta", "5000000")]);
    app.focus = Focus::Output(0);
    let id = app.graph.outputs[0].id;

    app.activate_focused();

    let Mode::Picker { kind, options, .. } = &app.mode else { panic!("expected picker mode") };
    assert!(matches!(kind, PickerKind::ExtraArgField { target } if matches!(target, crate::app::ExtraArgsTarget::Output(nid) if *nid == id)));
    assert!(options.iter().any(|o| o.display == "max_interleave_delta: 5000000"));
    assert!(options.iter().any(|o| o.display == "[ ] shortest"));
}

/// Picking a curated valueless flag (e.g. "re") should toggle it in place
/// and keep the picker open -- same idea as disposition flags -- rather
/// than opening a value prompt for a flag that doesn't take one.
#[test]
fn extra_args_picker_toggles_valueless_flag_in_place() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    let idx = app.graph.inputs.iter().position(|n| n.id == id).unwrap();
    app.focus = Focus::Input(idx);
    app.activate_focused();

    let Mode::Picker { options, .. } = &app.mode else { panic!("expected picker mode") };
    let re_row = options.iter().position(|o| o.display == "[ ] re").unwrap();
    app.picker_move(re_row as isize);

    app.picker_confirm();

    assert!(matches!(app.mode, Mode::Picker { .. }), "picker should stay open after toggling");
    assert_eq!(app.graph.input(id).unwrap().extra_args.get("re"), Some(&String::new()));
    let Mode::Picker { options, .. } = &app.mode else { unreachable!() };
    assert!(options.iter().any(|o| o.display == "[x] re"));

    app.picker_confirm(); // toggle back off

    assert!(!app.graph.input(id).unwrap().extra_args.contains_key("re"));
}

/// Picking a curated value-taking key should open a value text input, and
/// confirming a value should store it -- same round trip as Metadata/Filter
/// fields.
#[test]
fn extra_args_picker_value_field_opens_text_input_and_stores() {
    use crate::app::{App, Focus, Mode, TextTarget};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    let idx = app.graph.inputs.iter().position(|n| n.id == id).unwrap();
    app.focus = Focus::Input(idx);
    app.activate_focused();

    let Mode::Picker { options, .. } = &app.mode else { panic!("expected picker mode") };
    let row = options.iter().position(|o| o.value.as_deref() == Some("itsoffset")).unwrap();
    app.picker_move(row as isize);
    app.picker_confirm();

    let Mode::TextInput { target, buffer, .. } = &app.mode else { panic!("expected text input mode") };
    assert!(matches!(target, TextTarget::ExtraArgValue { key, .. } if key == "itsoffset"));
    assert_eq!(buffer, "");

    for c in "1.5".chars() {
        app.text_input_char(c);
    }
    app.confirm_text_input();

    assert_eq!(app.graph.input(id).unwrap().extra_args.get("itsoffset"), Some(&"1.5".to_string()));
}

/// "custom key..." should prompt for the key name first, then chain into
/// the value prompt for it -- same two-step flow as Metadata's custom key.
#[test]
fn extra_args_picker_custom_key_flow_prompts_for_key_then_value() {
    use crate::app::{App, Focus, Mode, TextTarget};

    let mut app = App::new();
    app.focus = Focus::Output(0);
    app.activate_focused();

    let Mode::Picker { options, .. } = &app.mode else { panic!("expected picker mode") };
    let last = options.len() - 1;
    app.picker_move(last as isize);
    app.picker_confirm();

    let Mode::TextInput { target, .. } = &app.mode else { panic!("expected text input mode") };
    assert!(matches!(target, TextTarget::ExtraArgCustomKey(_)));

    for c in "fflags".chars() {
        app.text_input_char(c);
    }
    app.confirm_text_input();

    let Mode::TextInput { target, buffer, .. } = &app.mode else { panic!("expected value prompt to open") };
    assert!(matches!(target, TextTarget::ExtraArgValue { key, .. } if key == "fflags"));
    assert_eq!(buffer, "");

    for c in "+genpts".chars() {
        app.text_input_char(c);
    }
    app.confirm_text_input();

    assert_eq!(app.graph.outputs[0].extra_args.get("fflags"), Some(&"+genpts".to_string()));
}

// ---------------------------------------------------------------------
// UI rendering
// ---------------------------------------------------------------------

/// A set extra_args should show up as a title tag on both input and output
/// nodes -- the only visible indicator that one of these is configured,
/// since (unlike Metadata/Filter nodes) input/output boxes have no
/// dedicated upper section to list it in.
#[test]
fn ui_renders_extra_args_in_upper_section_not_the_title() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = crate::app::App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    app.graph.input_mut(id).unwrap().extra_args = filter_fields(&[("itsoffset", "1.0")]);
    app.graph.outputs[0].extra_args = filter_fields(&[("max_interleave_delta", "5000000")]);
    app.graph.outputs[0].width = 60; // wide enough that the line isn't truncated, same box-width truncation as long paths already get elsewhere

    let backend = TestBackend::new(140, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buf = terminal.backend().buffer();
    let screen: String = (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(screen.contains("-itsoffset 1.0"), "expected the input's extra_args in its upper section:\n{screen}");
    assert!(
        screen.contains("-max_interleave_delta 5000000"),
        "expected the output's extra_args in its upper section:\n{screen}"
    );
    // Titles should be back to their plain, un-tagged form -- the whole
    // point of moving this into the body.
    assert!(screen.contains("[0] in.mp4 "), "expected an untagged input title:\n{screen}");
    assert!(screen.contains("OUTPUT 1: output.mkv "), "expected an untagged output title:\n{screen}");
}

/// Regression test for the row-offset math that places wire endpoints on
/// input/output nodes: an input's extra-args section pushes its stream
/// rows down, and an output's pushes its mapped-connection rows down, so
/// wires must attach below those sections rather than at a row hardcoded
/// to right-after-the-title.
#[test]
fn input_and_output_wires_attach_below_their_extra_args_section() {
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let id = app.graph.add_input("in.mp4".to_string(), video_audio_streams());
    app.graph.input_mut(id).unwrap().extra_args = filter_fields(&[("itsoffset", "1.0"), ("stream_loop", "2")]);
    app.graph.outputs[0].extra_args = filter_fields(&[("max_interleave_delta", "5000000")]);
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));

    let backend = TestBackend::new(160, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buf = terminal.backend().buffer();
    let screen: String = (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    let src_line = screen.lines().find(|l| l.contains("○ v:0 h264")).expect("stream row present");
    assert!(
        src_line.contains("─│"),
        "wire from the input should leave from the stream row itself, not drift onto another row:\n{src_line}"
    );

    let dst_line = screen.lines().find(|l| l.contains("v:0 h264") && l.contains("<-")).expect("mapped row present");
    assert!(
        dst_line.contains("│─"),
        "wire into the output should land on the mapped row itself, not drift onto another row:\n{dst_line}"
    );
}

/// A modifier node should render its incoming source summary and its
/// outgoing connection(s), and a wire leaving a non-Copy Convert node
/// should carry that codec as a badge.
#[test]
fn ui_renders_modifier_node_and_codec_badge_on_its_outgoing_wire() {
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let id = app.graph.add_input("video_a.mp4".to_string(), video_stream());
    let modifier = app.graph.add_modifier(ModifierKind::Convert(Codec::Encode("libx265".to_string())));
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    app.graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    let backend = TestBackend::new(160, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buffer = terminal.backend().buffer();
    let screen: String = (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(screen.contains("convert: x265"), "expected the convert node's title to show its codec:\n{screen}");
    assert!(screen.contains("← v:0 h264"), "expected the modifier's incoming summary:\n{screen}");
    assert!(screen.contains("→ OUTPUT 1"), "expected the modifier's outgoing row naming the output:\n{screen}");
    assert!(screen.contains("x265"), "expected the codec badge on the wire leaving the convert node:\n{screen}");
    assert!(screen.contains("[x265]"), "expected the output's mapped line to show the resolved codec tag:\n{screen}");
}

/// Regression test for a bug where the edge-drawing code used a dummy x=0
/// for the connection's destination instead of the output node's actual x
/// position, so every connector line shot off to the graph panel's left
/// edge instead of terminating at the output box.
#[test]
fn edge_line_reaches_output_node_not_canvas_origin() {
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));

    let backend = TestBackend::new(140, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buffer = terminal.backend().buffer().clone();

    let root = ratatui::layout::Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([
            ratatui::layout::Constraint::Length(1),
            ratatui::layout::Constraint::Min(10),
            ratatui::layout::Constraint::Length(1),
            ratatui::layout::Constraint::Length(10),
        ])
        .split(ratatui::layout::Rect::new(0, 0, 140, 40));
    let inner = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .inner(root[1]);

    let is_wire = |ch: &str| matches!(ch, "─" | "│" | "┌" | "┐" | "└" | "┘");

    let stray_at_origin =
        (inner.y..inner.bottom()).any(|y| is_wire(buffer[(inner.x, y)].symbol()));
    assert!(
        !stray_at_origin,
        "connector line reached the canvas origin instead of stopping at the output node"
    );

    let line_drawn_somewhere = (inner.y..inner.bottom())
        .flat_map(|y| (inner.x + 1..inner.right()).map(move |x| (x, y)))
        .any(|(x, y)| is_wire(buffer[(x, y)].symbol()));
    assert!(line_drawn_somewhere, "expected a connector line between the two nodes");
}

/// Connector wires should be colored by the kind of stream resolved at
/// their ultimate source, so a video, audio, and subtitle connection are
/// visually distinct.
#[test]
fn wires_are_colored_by_resolved_stream_kind() {
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let mk = |kind: StreamKind| vec![StreamInfo { index: 0, kind, codec: "c".to_string(), lang: None }];
    let v = app.graph.add_input("v.mp4".to_string(), mk(StreamKind::Video));
    let a = app.graph.add_input("a.m4a".to_string(), mk(StreamKind::Audio));
    let s = app.graph.add_input("s.srt".to_string(), mk(StreamKind::Subtitle));
    app.graph.connect(Endpoint::Stream { node: v, stream_idx: 0 }, Target::Output(out));
    app.graph.connect(Endpoint::Stream { node: a, stream_idx: 0 }, Target::Output(out));
    app.graph.connect(Endpoint::Stream { node: s, stream_idx: 0 }, Target::Output(out));

    let backend = TestBackend::new(140, 60);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buffer = terminal.backend().buffer().clone();

    let is_wire = |ch: &str| matches!(ch, "─" | "│" | "┌" | "┐" | "└" | "┘");
    let has_wire_colored = |color: Color| {
        (0..buffer.area.height).any(|y| {
            (0..buffer.area.width)
                .any(|x| is_wire(buffer[(x, y)].symbol()) && buffer[(x, y)].fg == color)
        })
    };

    assert!(has_wire_colored(Color::LightBlue), "expected a video-colored wire");
    assert!(has_wire_colored(Color::LightGreen), "expected an audio-colored wire");
    assert!(has_wire_colored(Color::LightMagenta), "expected a subtitle-colored wire");
}

/// The suggestions popup should render under the input line, list the
/// current matches, and hide once the mode leaves TextInput.
#[test]
fn ui_renders_suggestions_popup_and_hides_outside_text_input() {
    use crate::app::{App, Mode};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let dir = make_suggestion_fixture();
    let mut app = App::new();
    app.start_add_input();
    let Mode::TextInput { buffer, suggestions, selected, .. } = &mut app.mode else {
        panic!("expected text input mode");
    };
    *buffer = format!("{}/a", dir.display());
    *suggestions = crate::app::path_suggestions(buffer);
    *selected = 1;

    let backend = TestBackend::new(140, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buf = terminal.backend().buffer();
    let screen: String = (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(screen.contains("suggestions"), "expected the popup title:\n{screen}");
    assert!(screen.contains("alpha.mp4"), "expected the matching entry listed:\n{screen}");
    assert!(screen.contains("alpha2.txt"), "expected the other matching entry listed:\n{screen}");
    assert!(!screen.contains("beta.mkv"), "non-matching entry should be filtered out:\n{screen}");

    app.cancel_text_input();
    let mut terminal2 = Terminal::new(TestBackend::new(140, 40)).unwrap();
    terminal2.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buf2 = terminal2.backend().buffer();
    let screen2: String = (0..buf2.area.height)
        .map(|y| (0..buf2.area.width).map(|x| buf2[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!screen2.contains("suggestions"), "popup should disappear once out of text-input mode:\n{screen2}");

    let _ = std::fs::remove_dir_all(&dir);
}

// ---------------------------------------------------------------------
// End-to-end: real ffmpeg + ffprobe
// ---------------------------------------------------------------------

fn run_ok(cmd: &mut Command) {
    let status = cmd.status().expect("failed to run ffmpeg");
    assert!(status.success(), "ffmpeg setup command failed");
}

fn run_graph_and_wait(graph: &Graph) -> Option<String> {
    let args = graph.build_ffmpeg_args();
    let (tx, rx) = mpsc::channel();
    ffmpeg::run_args(args, tx);
    let mut done_code = None;
    while let Ok(line) = rx.recv() {
        if let Some(code) = line.strip_prefix("__DONE__") {
            done_code = Some(code.to_string());
            break;
        }
    }
    done_code
}

/// A real video+audio file for the filter end-to-end tests below, built
/// fresh per test into its own temp dir.
fn make_test_source(dir: &std::path::Path, duration_secs: u32, width: u32, height: u32) -> std::path::PathBuf {
    let path = dir.join("source.mp4");
    run_ok(Command::new("ffmpeg").args([
        "-y", "-loglevel", "error",
        "-f", "lavfi", "-i", &format!("testsrc=duration={duration_secs}:size={width}x{height}:rate=10"),
        "-f", "lavfi", "-i", &format!("sine=frequency=440:duration={duration_secs}"),
        "-c:v", "libx264", "-c:a", "aac", "-shortest", path.to_str().unwrap(),
    ]));
    path
}

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
    let id_v = graph.add_input(video_path.to_str().unwrap().to_string(), ffmpeg::probe(video_path.to_str().unwrap()).unwrap());
    let id_a = graph.add_input(audio_path.to_str().unwrap().to_string(), ffmpeg::probe(audio_path.to_str().unwrap()).unwrap());
    let id_s = graph.add_input(sub_path.to_str().unwrap().to_string(), ffmpeg::probe(sub_path.to_str().unwrap()).unwrap());

    graph.connect(Endpoint::Stream { node: id_v, stream_idx: 0 }, Target::Output(out));
    graph.connect(Endpoint::Stream { node: id_a, stream_idx: 0 }, Target::Output(out));
    graph.connect(Endpoint::Stream { node: id_s, stream_idx: 0 }, Target::Output(out));
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let out_streams = ffmpeg::probe(out_path.to_str().unwrap()).unwrap();
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
    let source_streams = ffmpeg::probe(audio_path.to_str().unwrap()).unwrap();
    assert_eq!(source_streams[0].codec, "aac", "test fixture should start as aac");
    let id = graph.add_input(audio_path.to_str().unwrap().to_string(), source_streams);
    let modifier = graph.add_modifier(ModifierKind::Convert(Codec::Encode("flac".to_string())));

    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let out_streams = ffmpeg::probe(out_path.to_str().unwrap()).unwrap();
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
    let id = graph.add_input(audio_path.to_str().unwrap().to_string(), ffmpeg::probe(audio_path.to_str().unwrap()).unwrap());
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
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap();
    let video_idx = streams.iter().position(|s| s.kind == StreamKind::Video).unwrap();
    let audio_idx = streams.iter().position(|s| s.kind == StreamKind::Audio).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams);

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

// ---------------------------------------------------------------------
// Filter-based modifiers: real -filter_complex end-to-end checks
// ---------------------------------------------------------------------

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
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap();
    let video_idx = streams.iter().position(|s| s.kind == StreamKind::Video).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams);
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
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap();
    let audio_idx = streams.iter().position(|s| s.kind == StreamKind::Audio).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams);
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
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap();
    let audio_idx = streams.iter().position(|s| s.kind == StreamKind::Audio).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams);
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
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap();
    let video_idx = streams.iter().position(|s| s.kind == StreamKind::Video).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams);
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
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap();
    let video_idx = streams.iter().position(|s| s.kind == StreamKind::Video).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams);
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
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap();
    let video_idx = streams.iter().position(|s| s.kind == StreamKind::Video).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams);
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
        let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap();
        let video_idx = streams.iter().position(|s| s.kind == StreamKind::Video).unwrap();
        let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams);
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
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap();
    let video_idx = streams.iter().position(|s| s.kind == StreamKind::Video).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams);

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
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams);
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
    let id = graph.add_input(audio_path.to_str().unwrap().to_string(), ffmpeg::probe(audio_path.to_str().unwrap()).unwrap());
    let convert = graph.add_modifier(ModifierKind::Convert(Codec::Encode("flac".to_string())));
    let metadata = graph.add_modifier(ModifierKind::Metadata { fields: metadata_fields(&[("language", "deu")]) });

    let src = Endpoint::Stream { node: id, stream_idx: 0 };
    graph.connect(src, Target::ModifierIn(convert));
    graph.connect(Endpoint::ModifierOut(convert), Target::ModifierIn(metadata));
    graph.connect(Endpoint::ModifierOut(metadata), Target::Output(out));
    graph.outputs[0].path = out_path.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let out_streams = ffmpeg::probe(out_path.to_str().unwrap()).unwrap();
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
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap();
    let video_idx = streams.iter().position(|s| s.kind == StreamKind::Video).unwrap();
    let audio_idx = streams.iter().position(|s| s.kind == StreamKind::Audio).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams);

    graph.connect(Endpoint::Stream { node: id, stream_idx: video_idx }, Target::Output(out1));
    graph.connect(Endpoint::Stream { node: id, stream_idx: audio_idx }, Target::Output(out2));
    graph.outputs[0].path = video_out.to_str().unwrap().to_string();
    graph.outputs[1].path = audio_out.to_str().unwrap().to_string();

    assert_eq!(run_graph_and_wait(&graph).as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let video_streams = ffmpeg::probe(video_out.to_str().unwrap()).unwrap();
    assert_eq!(video_streams.len(), 1);
    assert_eq!(video_streams[0].kind, StreamKind::Video);

    let audio_streams = ffmpeg::probe(audio_out.to_str().unwrap()).unwrap();
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
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap();
    let id = graph.add_input(source_path.to_str().unwrap().to_string(), streams);
    let modifier = graph.add_modifier(ModifierKind::Metadata { fields: metadata_fields(&[("language", "eng")]) });
    graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    // An output with nothing mapped to it has nothing resolvable to
    // preview, same as it has nothing to render for real.
    assert!(graph.build_preview_args(unconnected_out, preview_path.to_str().unwrap(), 2).is_none());

    let args = graph.build_preview_args(out, preview_path.to_str().unwrap(), 2).expect("resolvable");
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

    let tags = ffmpeg::probe(preview_path.to_str().unwrap()).unwrap();
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

// ---------------------------------------------------------------------
// Path autocomplete (unrelated to the node-chain rework)
// ---------------------------------------------------------------------

/// Sets up an isolated temp directory with known contents (two matching
/// files, one non-matching, a subdirectory, and a hidden file) so
/// path_suggestions' listing/filtering logic can be checked deterministically,
/// independent of whatever the test runner's actual cwd happens to contain.
fn make_suggestion_fixture() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tff-test-suggest-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("subdir")).unwrap();
    std::fs::write(dir.join("alpha.mp4"), b"").unwrap();
    std::fs::write(dir.join("alpha2.txt"), b"").unwrap();
    std::fs::write(dir.join("beta.mkv"), b"").unwrap();
    std::fs::write(dir.join(".hidden"), b"").unwrap();
    dir
}

#[test]
fn path_suggestions_lists_directory_sorted_with_dirs_marked() {
    use crate::app::path_suggestions;

    let dir = make_suggestion_fixture();
    let prefix = format!("{}/", dir.display());
    let results = path_suggestions(&prefix);

    let expected = vec![
        format!("{prefix}alpha.mp4"),
        format!("{prefix}alpha2.txt"),
        format!("{prefix}beta.mkv"),
        format!("{prefix}subdir/"),
    ];
    assert_eq!(results, expected);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn path_suggestions_filters_by_prefix_after_last_slash() {
    use crate::app::path_suggestions;

    let dir = make_suggestion_fixture();
    let query = format!("{}/al", dir.display());
    let results = path_suggestions(&query);

    assert_eq!(results, vec![format!("{}/alpha.mp4", dir.display()), format!("{}/alpha2.txt", dir.display())]);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn path_suggestions_hides_dotfiles_unless_prefix_starts_with_dot() {
    use crate::app::path_suggestions;

    let dir = make_suggestion_fixture();

    let plain = path_suggestions(&format!("{}/", dir.display()));
    assert!(!plain.iter().any(|s| s.ends_with(".hidden")), "dotfile should be hidden by default: {plain:?}");

    let dotted = path_suggestions(&format!("{}/.", dir.display()));
    assert!(dotted.iter().any(|s| s.ends_with(".hidden")), "dotfile should show once typing a dot: {dotted:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn path_suggestions_returns_empty_for_nonexistent_directory() {
    use crate::app::path_suggestions;

    let results = path_suggestions("/this/path/does/not/exist/on/this/machine/");
    assert!(results.is_empty());
}

#[test]
fn text_input_accept_suggestion_drills_into_directories() {
    use crate::app::{App, Mode};

    let dir = make_suggestion_fixture();
    let mut app = App::new();
    app.start_add_input();
    let Mode::TextInput { buffer, suggestions, selected, .. } = &mut app.mode else {
        panic!("expected text input mode");
    };
    *buffer = format!("{}/", dir.display());
    *suggestions = crate::app::path_suggestions(buffer);
    *selected = suggestions.iter().position(|s| s.ends_with("subdir/")).expect("subdir should be offered");

    app.text_input_accept_suggestion();

    let Mode::TextInput { buffer, suggestions, selected, .. } = &app.mode else {
        panic!("expected text input mode");
    };
    assert_eq!(*buffer, format!("{}/subdir/", dir.display()));
    assert_eq!(*selected, 0, "selection should reset after accepting");
    assert!(suggestions.is_empty(), "the fixture's subdir is empty");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn text_input_move_suggestion_wraps() {
    use crate::app::{App, Mode};

    let dir = make_suggestion_fixture();
    let mut app = App::new();
    app.start_add_input();
    let Mode::TextInput { buffer, suggestions, .. } = &mut app.mode else {
        panic!("expected text input mode");
    };
    *buffer = format!("{}/", dir.display());
    *suggestions = crate::app::path_suggestions(buffer);
    let count = suggestions.len();
    assert!(count >= 2, "fixture should offer several entries");

    app.text_input_move_suggestion(-1);
    let Mode::TextInput { selected, .. } = &app.mode else { unreachable!() };
    assert_eq!(*selected, count - 1, "moving back from 0 should wrap to the last entry");

    app.text_input_move_suggestion(1);
    let Mode::TextInput { selected, .. } = &app.mode else { unreachable!() };
    assert_eq!(*selected, 0, "moving forward should wrap back to the first entry");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn text_input_char_and_backspace_refresh_suggestions() {
    use crate::app::{App, Mode};

    let dir = make_suggestion_fixture();
    let mut app = App::new();
    app.start_add_input();
    let Mode::TextInput { buffer, .. } = &mut app.mode else { panic!("expected text input mode") };
    *buffer = format!("{}/", dir.display());

    for c in "al".chars() {
        app.text_input_char(c);
    }
    let Mode::TextInput { buffer, suggestions, selected, .. } = &app.mode else {
        panic!("expected text input mode");
    };
    assert_eq!(buffer, &format!("{}/al", dir.display()));
    assert_eq!(suggestions.len(), 2, "should narrow to the two 'al*' entries: {suggestions:?}");
    assert_eq!(*selected, 0);

    // One backspace narrows "al" to "a" -- still just the two "al*" entries.
    app.text_input_backspace();
    let Mode::TextInput { suggestions, .. } = &app.mode else { panic!("expected text input mode") };
    assert_eq!(suggestions.len(), 2, "'a' should still match only the two 'al*' entries: {suggestions:?}");

    // A second backspace clears the prefix entirely, widening to everything.
    app.text_input_backspace();
    let Mode::TextInput { suggestions, .. } = &app.mode else { panic!("expected text input mode") };
    assert_eq!(suggestions.len(), 4, "an empty prefix should widen the match to all four entries: {suggestions:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// The metadata key picker should actually render its curated fields (with
/// current values), and the modifier node itself should show a compact
/// summary of what's set.
#[test]
fn ui_renders_metadata_picker_and_node_summary() {
    use crate::app::{App, Focus};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new();
    let modifier =
        app.graph.add_modifier(ModifierKind::Metadata { fields: metadata_fields(&[("language", "eng")]) });
    let idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(idx);
    app.activate_modifier();

    let backend = TestBackend::new(140, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buf = terminal.backend().buffer();
    let screen: String = (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(screen.contains("[metadata]"), "expected the node's title tag:\n{screen}");
    assert!(screen.contains("metadata: choose field"), "expected the picker title:\n{screen}");
    assert!(screen.contains("language: eng"), "expected the field listed in the node's upper section:\n{screen}");
    assert!(screen.contains("title: (not set)"), "expected an unset curated field:\n{screen}");
    assert!(screen.contains("handler_name: (not set)"), "expected the third curated field:\n{screen}");
    assert!(screen.contains("custom key"), "expected the custom-key escape hatch:\n{screen}");
}

/// The disposition flag picker should render its checkboxes, and the
/// modifier node itself should list its active flags in the upper section.
#[test]
fn ui_renders_disposition_picker_and_node_flag_list() {
    use crate::app::{App, Focus};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new();
    let modifier =
        app.graph.add_modifier(ModifierKind::Disposition { flags: disposition_set(&["forced", "default"]) });
    let idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(idx);
    app.activate_modifier();

    let backend = TestBackend::new(140, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buf = terminal.backend().buffer();
    let screen: String = (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(screen.contains("[disposition]"), "expected the node's title tag:\n{screen}");
    assert!(screen.contains("disposition: toggle flags"), "expected the picker title:\n{screen}");
    assert!(screen.contains("[x] forced"), "expected the picker's checked box for forced:\n{screen}");
    assert!(screen.contains("[ ] hearing_impaired"), "expected an unchecked curated flag:\n{screen}");
    assert!(screen.contains("default"), "expected 'default' listed in the node's upper section:\n{screen}");
    assert!(screen.contains("forced"), "expected 'forced' listed in the node's upper section:\n{screen}");
}

/// A Filter node's field picker should render its curated fields, and the
/// node itself should list its set parameters in the same two-part upper-
/// section layout as Metadata/Disposition.
#[test]
fn ui_renders_filter_field_picker_and_node_parameter_list() {
    use crate::app::{App, Focus};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream());
    let modifier = app.graph.add_modifier(ModifierKind::Filter {
        name: FilterName::Scale,
        fields: filter_fields(&[("width", "1280")]),
    });
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    let idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(idx);
    app.activate_modifier();

    let backend = TestBackend::new(140, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buf = terminal.backend().buffer();
    let screen: String = (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(screen.contains("[scale]"), "expected the node's title tag:\n{screen}");
    assert!(screen.contains("scale: choose field"), "expected the picker title:\n{screen}");
    assert!(screen.contains("width: 1280"), "expected the current value in the picker:\n{screen}");
    assert!(screen.contains("height: (not set)"), "expected an unset curated field in the picker:\n{screen}");
    assert!(screen.contains("width: 1280"), "expected the field listed in the node's upper section:\n{screen}");
}

/// Regression test for the row-offset math that places wire endpoints on a
/// Metadata node: its field section grows with however many fields are set,
/// which pushes the incoming/outgoing connection rows (and thus where wires
/// must attach) down by that many rows. With a 3-field node this used to be
/// enough to misalign the old hardcoded offsets.
#[test]
fn metadata_node_wires_attach_below_its_field_section_not_at_a_fixed_row() {
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let id = app.graph.add_input("video_a.mp4".to_string(), video_stream());
    let modifier = app.graph.add_modifier(ModifierKind::Metadata {
        fields: metadata_fields(&[("language", "eng"), ("title", "Commentary"), ("handler_name", "H")]),
    });
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    app.graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));

    let backend = TestBackend::new(160, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buffer = terminal.backend().buffer();
    let screen: String = (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    let incoming_line =
        screen.lines().find(|l| l.contains("← v:0 h264")).expect("incoming connection row present");
    assert!(
        incoming_line.contains("───│← v:0 h264"),
        "wire from the input should terminate on the incoming row itself, not drift onto a field row:\n{incoming_line}"
    );

    let outgoing_line =
        screen.lines().find(|l| l.contains("→ OUTPUT 1")).expect("outgoing connection row present");
    assert!(
        outgoing_line.contains("──│"),
        "wire to the output should leave from the outgoing row itself, not drift onto another row:\n{outgoing_line}"
    );
}

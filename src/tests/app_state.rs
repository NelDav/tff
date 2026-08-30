use super::*;


/// Arming an input stream, then focusing a modifier and pressing 'c'
/// should wire the stream into that modifier's input and clear armed
/// state.
#[test]
fn toggle_connect_wires_armed_stream_into_focused_modifier() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let modifier = app.graph.add_modifier(ModifierKind::Convert(Codec::Copy));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();

    app.focus = Focus::Input(0);
    app.row_idx = 0;
    app.toggle_connect(); // arm the stream
    assert!(!app.armed.is_empty());

    app.focus = Focus::Modifier(modifier_idx);
    app.toggle_connect(); // connect

    assert!(app.armed.is_empty());
    let incoming = app.graph.incoming(Target::ModifierIn(modifier));
    assert_eq!(incoming.len(), 1);
    assert_eq!(app.graph.wires[incoming[0]].from, Endpoint::Stream { node: id, stream_idx: 0 });
}

/// Wiring a stream of the wrong kind into a kind-restricted modifier (a
/// `Filter` that only applies to one kind, or `ChapterEdit`, which only
/// meaningfully imports from a chapter-kind source) should be rejected up
/// front rather than silently accepted and only complained about later,
/// when the node is edited.
#[test]
fn toggle_connect_rejects_a_stream_kind_a_filter_modifier_does_not_accept() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
    let modifier = app.graph.add_modifier(ModifierKind::Filter { name: FilterName::Volume, fields: BTreeMap::new() });
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();

    // Volume only applies to audio -- arm the video stream instead.
    app.armed = BTreeSet::from([Endpoint::Stream { node: id, stream_idx: 0 }]);
    app.focus = Focus::Modifier(modifier_idx);
    app.toggle_connect();

    assert!(app.graph.incoming(Target::ModifierIn(modifier)).is_empty(), "the mismatched wire should not be created");
    assert_eq!(app.armed.len(), 1, "the armed port should be left alone so a different target can be tried");
    assert!(app.log.last().unwrap().contains("doesn't accept a video stream"), "{:?}", app.log.last());
}

/// The counterpart to the rejection above: a stream of a kind the filter
/// *does* accept should still connect normally.
#[test]
fn toggle_connect_allows_a_stream_kind_a_filter_modifier_does_accept() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
    let modifier = app.graph.add_modifier(ModifierKind::Filter { name: FilterName::Volume, fields: BTreeMap::new() });
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();

    app.armed = BTreeSet::from([Endpoint::Stream { node: id, stream_idx: 1 }]); // audio
    app.focus = Focus::Modifier(modifier_idx);
    app.toggle_connect();

    assert_eq!(app.graph.incoming(Target::ModifierIn(modifier)).len(), 1);
    assert!(app.armed.is_empty());
}

/// A `ChapterEdit` modifier's input only meaningfully accepts a
/// chapter-kind source (see `ModifierKind::accepts_stream_kind`) -- wiring
/// a regular video/audio stream into it should be rejected the same way.
#[test]
fn toggle_connect_rejects_a_non_chapter_stream_into_a_chapter_edit_modifier() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let modifier = app.graph.add_modifier(ModifierKind::ChapterEdit { chapters: Vec::new() });
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();

    app.armed = BTreeSet::from([Endpoint::Stream { node: id, stream_idx: 0 }]);
    app.focus = Focus::Modifier(modifier_idx);
    app.toggle_connect();

    assert!(app.graph.incoming(Target::ModifierIn(modifier)).is_empty());
    assert_eq!(app.armed.len(), 1);
    assert!(app.log.last().unwrap().contains("doesn't accept a video stream"), "{:?}", app.log.last());
}

/// Connecting a chapter-kind source into a `ChapterEdit` modifier's input
/// should still work normally.
#[test]
fn toggle_connect_allows_a_chapter_stream_into_a_chapter_edit_modifier() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let chapters_id = app.graph.add_input(
        "chapters.ffmeta".to_string(),
        Vec::new(),
        vec![Chapter::new(0.0, 1.0, "A".to_string())],
    );
    let chapter_idx = app.graph.input(chapters_id).unwrap().streams.len() - 1;
    let modifier = app.graph.add_modifier(ModifierKind::ChapterEdit { chapters: Vec::new() });
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();

    app.armed = BTreeSet::from([Endpoint::Stream { node: chapters_id, stream_idx: chapter_idx }]);
    app.focus = Focus::Modifier(modifier_idx);
    app.toggle_connect();

    assert_eq!(app.graph.incoming(Target::ModifierIn(modifier)).len(), 1);
    assert!(app.armed.is_empty());
}

/// Connecting an upstream modifier whose own input isn't wired to
/// anything yet has no determinable stream kind -- that should still be
/// allowed optimistically (same as before this rejection existed),
/// instead of assuming the worst and blocking chain-building order.
#[test]
fn toggle_connect_allows_an_unresolved_upstream_modifier_into_a_kind_restricted_one() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let upstream = app.graph.add_modifier(ModifierKind::Convert(Codec::Copy)); // nothing feeds it yet
    let modifier = app.graph.add_modifier(ModifierKind::Filter { name: FilterName::Scale, fields: BTreeMap::new() });
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();

    app.armed = BTreeSet::from([Endpoint::ModifierOut(upstream)]);
    app.focus = Focus::Modifier(modifier_idx);
    app.toggle_connect();

    assert_eq!(app.graph.incoming(Target::ModifierIn(modifier)).len(), 1, "an unresolved source shouldn't be blocked");
}

/// Once a modifier's input is fed, focusing it and pressing 'c' with
/// nothing else armed should arm *its* output, so the chain can continue
/// to another node.
#[test]
fn toggle_connect_arms_a_connected_modifiers_output() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let modifier = app.graph.add_modifier(ModifierKind::Convert(Codec::Copy));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));

    app.focus = Focus::Modifier(modifier_idx);
    app.toggle_connect();

    assert_eq!(app.armed, BTreeSet::from([Endpoint::ModifierOut(modifier)]));
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
    assert!(!app.armed.is_empty());
    app.toggle_connect();
    assert!(app.armed.is_empty());
}

/// Space toggles the hovered stream's membership in the pending
/// selection, independent of `armed` -- selecting doesn't arm anything by
/// itself.
#[test]
fn space_toggles_port_selection() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
    app.focus = Focus::Input(0);
    app.row_idx = 0;

    app.toggle_port_selection();
    assert_eq!(app.selected, BTreeSet::from([Endpoint::Stream { node: id, stream_idx: 0 }]));
    assert!(app.armed.is_empty(), "selecting shouldn't arm anything");

    app.row_idx = 1;
    app.toggle_port_selection();
    assert_eq!(
        app.selected,
        BTreeSet::from([
            Endpoint::Stream { node: id, stream_idx: 0 },
            Endpoint::Stream { node: id, stream_idx: 1 },
        ])
    );

    // Toggling the same row again removes it.
    app.toggle_port_selection();
    assert_eq!(app.selected, BTreeSet::from([Endpoint::Stream { node: id, stream_idx: 0 }]));
}

/// Ctrl+A selects every port on the focused input node in one action, and
/// is additive with a selection already staged on a different node rather
/// than replacing it.
#[test]
fn ctrl_a_selects_every_port_on_the_focused_node() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let other = app.graph.add_input("other.mp4".to_string(), video_stream(), Vec::new());
    let id = app.graph.add_input("in.mp4".to_string(), three_streams(), Vec::new());

    // Pre-existing selection on a different input node.
    app.focus = Focus::Input(0);
    app.row_idx = 0;
    app.toggle_port_selection();

    app.focus = Focus::Input(1);
    app.row_idx = 1;
    app.select_all_ports();

    assert_eq!(
        app.selected,
        BTreeSet::from([
            Endpoint::Stream { node: other, stream_idx: 0 },
            Endpoint::Stream { node: id, stream_idx: 0 },
            Endpoint::Stream { node: id, stream_idx: 1 },
            Endpoint::Stream { node: id, stream_idx: 2 },
        ])
    );
    assert!(app.armed.is_empty(), "selecting shouldn't arm anything");
}

/// Ctrl+A is a no-op outside `Focus::Input` -- there's no "all ports" of
/// a modifier's single output or an output node.
#[test]
fn ctrl_a_is_a_no_op_off_an_input_node() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    app.graph.add_input("in.mp4".to_string(), three_streams(), Vec::new());
    app.focus = Focus::Output(0);

    app.select_all_ports();

    assert!(app.selected.is_empty());
}

/// Ctrl+Down moves the hovered mapped-stream row past its neighbor --
/// output stream order tracks wire order (see `Graph::incoming`), so this
/// reorders which stream lands where in the muxed container without
/// touching either wire's own endpoints.
#[test]
fn ctrl_down_swaps_the_hovered_output_row_with_the_next() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), three_streams(), Vec::new());
    let out = app.graph.outputs[0].id;
    let video = Endpoint::Stream { node: id, stream_idx: 0 };
    let audio = Endpoint::Stream { node: id, stream_idx: 1 };
    let subtitle = Endpoint::Stream { node: id, stream_idx: 2 };
    app.graph.connect(video, Target::Output(out));
    app.graph.connect(audio, Target::Output(out));
    app.graph.connect(subtitle, Target::Output(out));

    app.focus = Focus::Output(0);
    app.row_idx = 0;
    app.move_focused_row(true);

    let order: Vec<Endpoint> = app
        .graph
        .incoming(Target::Output(out))
        .into_iter()
        .map(|wi| app.graph.wires[wi].from)
        .collect();
    assert_eq!(order, vec![audio, video, subtitle], "video should have moved past audio");
    assert_eq!(app.row_idx, 1, "the hovered row should follow the moved wire");

    // Move it back up again -- should restore the original order.
    app.move_focused_row(false);
    let order: Vec<Endpoint> = app
        .graph
        .incoming(Target::Output(out))
        .into_iter()
        .map(|wi| app.graph.wires[wi].from)
        .collect();
    assert_eq!(order, vec![video, audio, subtitle]);
    assert_eq!(app.row_idx, 0);
}

/// Reordering is a no-op at either edge of the mapped-stream list and on
/// the chapters row (there's only ever one, nothing to swap it with).
#[test]
fn move_output_row_is_a_no_op_at_edges_and_on_the_chapters_row() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), three_streams(), Vec::new());
    let chapters_id = app.graph.add_input(
        "chaps.mp4".to_string(),
        video_stream(),
        vec![Chapter::new(0.0, 1.0, "one".to_string())],
    );
    let out = app.graph.outputs[0].id;
    let video = Endpoint::Stream { node: id, stream_idx: 0 };
    let audio = Endpoint::Stream { node: id, stream_idx: 1 };
    app.graph.connect(video, Target::Output(out));
    app.graph.connect(audio, Target::Output(out));
    app.graph.connect(
        Endpoint::Stream { node: chapters_id, stream_idx: 1 },
        Target::OutputChapters(out),
    );

    app.focus = Focus::Output(0);

    // Already at the top -- moving up further is a no-op.
    app.row_idx = 0;
    app.move_focused_row(false);
    assert_eq!(app.row_idx, 0);
    let order: Vec<Endpoint> = app
        .graph
        .incoming(Target::Output(out))
        .into_iter()
        .map(|wi| app.graph.wires[wi].from)
        .collect();
    assert_eq!(order, vec![video, audio]);

    // Already at the bottom of the mapped-stream rows -- moving down
    // further is a no-op too.
    app.row_idx = 1;
    app.move_focused_row(true);
    assert_eq!(app.row_idx, 1);
    let order: Vec<Endpoint> = app
        .graph
        .incoming(Target::Output(out))
        .into_iter()
        .map(|wi| app.graph.wires[wi].from)
        .collect();
    assert_eq!(order, vec![video, audio]);

    // The chapters row (index 2, one past the two mapped-stream rows) has
    // nothing to reorder against.
    app.row_idx = 2;
    app.move_focused_row(true);
    app.move_focused_row(false);
    assert!(!app.graph.incoming(Target::OutputChapters(out)).is_empty(), "chapters wire untouched");
}

/// Ctrl+Up/Down is a no-op off an output node.
#[test]
fn move_output_row_is_a_no_op_off_an_output_node() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
    let out = app.graph.outputs[0].id;
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 1 }, Target::Output(out));

    app.focus = Focus::Input(0);
    app.row_idx = 0;
    app.move_focused_row(true);

    let order: Vec<Endpoint> = app
        .graph
        .incoming(Target::Output(out))
        .into_iter()
        .map(|wi| app.graph.wires[wi].from)
        .collect();
    assert_eq!(order[0], Endpoint::Stream { node: id, stream_idx: 0 }, "unrelated focus shouldn't reorder anything");
}

/// Shift+Down/Up extends a contiguous range from wherever it started,
/// recomputed fresh each press so shrinking the range back correctly
/// drops rows that fall outside it again.
#[test]
fn shift_extends_a_contiguous_port_range() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), three_streams(), Vec::new());
    app.focus = Focus::Input(0);
    app.row_idx = 0;

    app.extend_port_selection(true); // anchor at 0, extend to 1
    assert_eq!(
        app.selected,
        BTreeSet::from([
            Endpoint::Stream { node: id, stream_idx: 0 },
            Endpoint::Stream { node: id, stream_idx: 1 },
        ])
    );

    app.extend_port_selection(true); // extend to 2
    assert_eq!(app.selected.len(), 3, "{:?}", app.selected);

    app.extend_port_selection(false); // shrink back to 0..=1
    assert_eq!(
        app.selected,
        BTreeSet::from([
            Endpoint::Stream { node: id, stream_idx: 0 },
            Endpoint::Stream { node: id, stream_idx: 1 },
        ]),
        "row 2 should drop back out once the range shrinks past it"
    );
}

/// Plain (non-Shift) navigation should end an in-progress Shift+range, so
/// a later Shift+extend starts a fresh anchor from the new row instead of
/// resuming the old one.
#[test]
fn plain_navigation_resets_the_range_anchor() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), three_streams(), Vec::new());
    app.focus = Focus::Input(0);
    app.row_idx = 0;

    app.extend_port_selection(true); // anchor 0, row_idx -> 1
    assert_eq!(app.row_idx, 1);

    app.cycle_row(true); // plain Down: row_idx -> 2, should reset the anchor
    assert_eq!(app.row_idx, 2);

    app.extend_port_selection(true); // fresh anchor at 2, extends to... clamped at len-1 (2)
    // Only row 2 should be selected from this second range (anchor == new
    // row, since row 2 is already the last stream) -- row 0/1 from the
    // earlier range must be gone since the anchor restarted.
    assert_eq!(app.selected, BTreeSet::from([Endpoint::Stream { node: id, stream_idx: 2 }]));
}

/// Pressing 'c' on an input with a non-empty pending selection should arm
/// every selected port in one action and clear the selection -- even when
/// the ports span more than one input node.
#[test]
fn arming_a_multi_port_selection_across_nodes() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let a = app.graph.add_input("a.mp4".to_string(), video_stream(), Vec::new());
    let b = app.graph.add_input("b.mp4".to_string(), video_stream(), Vec::new());

    app.focus = Focus::Input(0);
    app.row_idx = 0;
    app.toggle_port_selection(); // select a's only stream

    app.focus = Focus::Input(1);
    app.row_idx = 0;
    app.toggle_port_selection(); // select b's only stream
    assert_eq!(app.selected.len(), 2);

    app.toggle_connect(); // 'c' with a pending selection -> arm all of it

    assert!(app.selected.is_empty(), "arming should clear the pending selection");
    assert_eq!(
        app.armed,
        BTreeSet::from([
            Endpoint::Stream { node: a, stream_idx: 0 },
            Endpoint::Stream { node: b, stream_idx: 0 },
        ])
    );
}

/// Arming a selection replaces whatever was already armed rather than
/// adding to it -- pressing 'c' on one selection, then selecting
/// something else entirely and pressing 'c' again, should leave only the
/// second selection armed, not both combined.
#[test]
fn arming_a_new_selection_replaces_rather_than_accumulates() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let a = app.graph.add_input("a.mp4".to_string(), video_stream(), Vec::new());
    let b = app.graph.add_input("b.mp4".to_string(), video_stream(), Vec::new());

    app.focus = Focus::Input(0);
    app.row_idx = 0;
    app.toggle_port_selection(); // select a's only stream
    app.toggle_connect(); // arm it
    assert_eq!(app.armed, BTreeSet::from([Endpoint::Stream { node: a, stream_idx: 0 }]));

    app.focus = Focus::Input(1);
    app.row_idx = 0;
    app.toggle_port_selection(); // select b's only stream instead
    app.toggle_connect(); // arm it -- should replace a's armed port, not join it

    assert_eq!(
        app.armed,
        BTreeSet::from([Endpoint::Stream { node: b, stream_idx: 0 }]),
        "arming a new selection should replace the previously armed set"
    );
}

/// With nothing explicitly selected, 'c' on an input falls back to the
/// original single-hover toggle-arm behavior.
#[test]
fn arming_with_nothing_selected_falls_back_to_single_toggle() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    app.focus = Focus::Input(0);
    app.row_idx = 0;

    app.toggle_connect();
    assert_eq!(app.armed, BTreeSet::from([Endpoint::Stream { node: id, stream_idx: 0 }]));

    app.toggle_connect(); // pressing 'c' again on the same hovered port disarms it
    assert!(app.armed.is_empty());
}

/// The same replace-not-accumulate rule applies when arming one port at a
/// time by hovering, with nothing ever explicitly Space-selected: hovering
/// a different row and pressing 'c' again should leave only that row
/// armed, not both.
#[test]
fn arming_by_hovering_different_rows_replaces_rather_than_accumulates() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
    app.focus = Focus::Input(0);

    app.row_idx = 0;
    app.toggle_connect(); // arm the video stream by hovering it
    assert_eq!(app.armed, BTreeSet::from([Endpoint::Stream { node: id, stream_idx: 0 }]));

    app.row_idx = 1;
    app.toggle_connect(); // hover the audio stream and arm it instead

    assert_eq!(
        app.armed,
        BTreeSet::from([Endpoint::Stream { node: id, stream_idx: 1 }]),
        "arming a different hovered port should replace the previously armed one"
    );
}

/// Connecting a multi-armed set to an output should create one wire per
/// armed source, all in a single 'c' press.
#[test]
fn connecting_multiple_armed_ports_to_an_output_creates_one_wire_each() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let id = app.graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
    app.focus = Focus::Input(0);
    app.row_idx = 0;
    app.toggle_port_selection();
    app.row_idx = 1;
    app.toggle_port_selection();
    app.toggle_connect(); // arm both

    app.focus = Focus::Output(0);
    app.toggle_connect(); // connect all armed to the output

    assert!(app.armed.is_empty());
    let incoming = app.graph.incoming(Target::Output(out));
    assert_eq!(incoming.len(), 2, "{incoming:?}");
    let sources: BTreeSet<Endpoint> = incoming.iter().map(|&wi| app.graph.wires[wi].from).collect();
    assert_eq!(
        sources,
        BTreeSet::from([
            Endpoint::Stream { node: id, stream_idx: 0 },
            Endpoint::Stream { node: id, stream_idx: 1 },
        ])
    );
}

/// A modifier's input only ever holds one wire -- connecting a multi-armed
/// set to one should be rejected with a clear message, leaving the armed
/// set untouched (and no wire created) so the user can fix it up.
#[test]
fn connecting_multiple_armed_ports_to_a_modifier_is_rejected() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    app.graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
    let modifier = app.graph.add_modifier(ModifierKind::Convert(Codec::Copy));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();

    app.focus = Focus::Input(0);
    app.row_idx = 0;
    app.toggle_port_selection();
    app.row_idx = 1;
    app.toggle_port_selection();
    app.toggle_connect(); // arm both
    assert_eq!(app.armed.len(), 2);

    let armed_before = app.armed.clone();
    app.focus = Focus::Modifier(modifier_idx);
    app.toggle_connect(); // should reject, not connect

    assert_eq!(app.armed, armed_before, "armed set should be untouched after a rejected connect");
    assert!(app.graph.incoming(Target::ModifierIn(modifier)).is_empty());
    assert!(app.log.last().unwrap().contains("only accepts one"), "{:?}", app.log.last());
}

/// Deleting an input node should clean up any of its ports from both
/// `selected` and `armed`, not just `armed`.
#[test]
fn deleting_an_input_cleans_up_selected_and_armed() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let a = app.graph.add_input("a.mp4".to_string(), video_stream(), Vec::new());
    let b = app.graph.add_input("b.mp4".to_string(), video_stream(), Vec::new());
    app.selected.insert(Endpoint::Stream { node: a, stream_idx: 0 });
    app.armed.insert(Endpoint::Stream { node: b, stream_idx: 0 });

    app.focus = Focus::Input(0); // "a"
    app.delete_focused_node();

    assert!(app.selected.is_empty(), "{:?}", app.selected);
    assert!(!app.armed.is_empty(), "b's armed port shouldn't be touched by deleting a");
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
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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

/// 'd' on an input with a pending multi-port selection should disconnect
/// every selected port from everything downstream in one action, the
/// mirror image of how 'c' arms them all at once -- and clear the
/// selection afterward.
#[test]
fn disconnecting_a_multi_port_selection_removes_wires_from_every_selected_port() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let id = app.graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 1 }, Target::Output(out));
    assert_eq!(app.graph.wires.len(), 2);

    app.focus = Focus::Input(0);
    app.row_idx = 0;
    app.toggle_port_selection();
    app.row_idx = 1;
    app.toggle_port_selection();
    assert_eq!(app.selected.len(), 2);

    app.disconnect_focused();

    assert!(app.graph.wires.is_empty(), "{:?}", app.graph.wires);
    assert!(app.selected.is_empty(), "the selection should be consumed by the bulk disconnect");
}

/// A bulk disconnect via a pending selection should work regardless of
/// which input node happens to be focused when 'd' is pressed, spanning
/// ports from several different inputs at once -- same as bulk arming.
#[test]
fn disconnecting_a_multi_port_selection_spans_different_input_nodes() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let a = app.graph.add_input("a.mp4".to_string(), video_stream(), Vec::new());
    let b = app.graph.add_input("b.mp4".to_string(), video_stream(), Vec::new());
    app.graph.connect(Endpoint::Stream { node: a, stream_idx: 0 }, Target::Output(out));
    app.graph.connect(Endpoint::Stream { node: b, stream_idx: 0 }, Target::Output(out));

    app.focus = Focus::Input(0);
    app.row_idx = 0;
    app.toggle_port_selection(); // select a's stream

    app.focus = Focus::Input(1);
    app.row_idx = 0;
    app.toggle_port_selection(); // select b's stream

    // Focused on b when 'd' is pressed, but both should still be removed.
    app.disconnect_focused();

    assert!(app.graph.wires.is_empty(), "{:?}", app.graph.wires);
}

/// With nothing explicitly selected, 'd' on an input falls back to the
/// original single-hover disconnect behavior.
#[test]
fn disconnecting_with_nothing_selected_falls_back_to_single_hover() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let id = app.graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 1 }, Target::Output(out));

    app.focus = Focus::Input(0);
    app.row_idx = 0;
    app.disconnect_focused();

    assert_eq!(app.graph.wires.len(), 1, "only the hovered row's wire should be removed");
    assert_eq!(app.graph.wires[0].from, Endpoint::Stream { node: id, stream_idx: 1 });
}

/// A bulk disconnect that clears a `ChapterEdit` node's only connected
/// chapter source should trigger the same auto-import cleanup a
/// single-port disconnect already does.
#[test]
fn bulk_disconnect_triggers_chapter_edit_import_cleanup() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let modifier = app
        .graph
        .add_modifier(ModifierKind::ChapterEdit { chapters: vec![Chapter::new(0.0, 1.0, "Manual".to_string())] });
    let chapters_id = app.graph.add_input(
        "chapters.ffmeta".to_string(),
        Vec::new(),
        vec![Chapter::new(0.0, 2.0, "Imported".to_string())],
    );
    let chapter_idx = app.graph.input(chapters_id).unwrap().streams.len() - 1;
    app.armed = BTreeSet::from([Endpoint::Stream { node: chapters_id, stream_idx: chapter_idx }]);
    app.focus = Focus::Modifier(app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap());
    app.toggle_connect(); // connect + auto-import
    assert_eq!(chapter_edit_chapters(&app.graph, modifier).unwrap().len(), 2);

    let chapters_idx = app.graph.inputs.iter().position(|n| n.id == chapters_id).unwrap();
    app.focus = Focus::Input(chapters_idx);
    app.row_idx = chapter_idx;
    app.toggle_port_selection();
    app.disconnect_focused();

    let chapters = chapter_edit_chapters(&app.graph, modifier).unwrap();
    assert_eq!(chapters.len(), 1, "{chapters:?}");
    assert_eq!(chapters[0].title, "Manual");
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
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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
    let id = app.graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
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
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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

    let Mode::TextInput { target, input, .. } = &app.mode else {
        panic!("expected text input mode");
    };
    let buffer = input.value();
    assert!(matches!(target, TextTarget::ModifierFilterValue { key, .. } if key == "width"));
    assert_eq!(buffer, "");

    for c in "1280".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    app.confirm_text_input();

    let Some(m) = app.graph.modifier(modifier) else { panic!("modifier disappeared") };
    let ModifierKind::Filter { fields, .. } = &m.kind else { panic!("wrong kind") };
    assert_eq!(fields.get("width"), Some(&"1280".to_string()));
}

/// Trim's start/end fields are the one Filter field pair that accepts a
/// time -- HH:MM:SS/MM:SS with an optional fractional-seconds tail, or
/// plain seconds -- and should be stored as plain seconds regardless of
/// which form was typed, since that's the only form that survives being
/// embedded in the `trim=start=...:end=...` expression (a colon inside
/// the stored value would collide with the filtergraph's own key=value
/// separator).
#[test]
fn trim_start_field_accepts_hh_mm_ss_with_fractional_seconds() {
    use crate::app::{App, Focus, Mode, TextTarget};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let modifier = app.graph.add_modifier(ModifierKind::Filter { name: FilterName::Trim, fields: BTreeMap::new() });
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier();
    let idx = match &app.mode {
        Mode::Picker { options, .. } => options.iter().position(|o| o.value.as_deref() == Some("start")).unwrap(),
        _ => panic!("expected picker mode"),
    };
    app.picker_move(idx as isize);
    app.picker_confirm();

    let Mode::TextInput { target, .. } = &app.mode else {
        panic!("expected text input mode");
    };
    assert!(matches!(target, TextTarget::ModifierFilterValue { key, .. } if key == "start"));

    for c in "1:30.5".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    app.confirm_text_input();

    let Some(m) = app.graph.modifier(modifier) else { panic!("modifier disappeared") };
    let ModifierKind::Filter { fields, .. } = &m.kind else { panic!("wrong kind") };
    assert_eq!(fields.get("start"), Some(&"90.5".to_string()), "1:30.5 should parse to 90.5 plain seconds");
    assert!(app.log.last().unwrap().contains("00:01:30.500"), "{:?}", app.log.last());
}

/// Re-opening the value editor for an already-set Trim start/end field
/// should show it back as HH:MM:SS, not the plain-seconds form it's
/// stored as -- same round trip a chapter's start/end field gets.
#[test]
fn trim_time_field_reprefills_as_hh_mm_ss_not_plain_seconds() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let modifier = app.graph.add_modifier(ModifierKind::Filter {
        name: FilterName::Trim,
        fields: filter_fields(&[("start", "90.5")]),
    });
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier();
    let idx = match &app.mode {
        Mode::Picker { options, .. } => options.iter().position(|o| o.value.as_deref() == Some("start")).unwrap(),
        _ => panic!("expected picker mode"),
    };
    app.picker_move(idx as isize);
    app.picker_confirm();

    let Mode::TextInput { input, .. } = &app.mode else {
        panic!("expected text input mode");
    };
    assert_eq!(input.value(), "00:01:30.500");
}

/// A time that doesn't parse at all should be rejected with a log message
/// rather than silently stored -- an unparseable value embedded in the
/// filter expression would just break the whole render.
#[test]
fn trim_time_field_rejects_unparseable_input() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let modifier = app.graph.add_modifier(ModifierKind::Filter { name: FilterName::Trim, fields: BTreeMap::new() });
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.activate_modifier();
    let idx = match &app.mode {
        Mode::Picker { options, .. } => options.iter().position(|o| o.value.as_deref() == Some("start")).unwrap(),
        _ => panic!("expected picker mode"),
    };
    app.picker_move(idx as isize);
    app.picker_confirm();

    for c in "not-a-time".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    app.confirm_text_input();

    let Some(m) = app.graph.modifier(modifier) else { panic!("modifier disappeared") };
    let ModifierKind::Filter { fields, .. } = &m.kind else { panic!("wrong kind") };
    assert!(fields.get("start").is_none(), "an unparseable time must not be stored");
    assert!(app.log.last().unwrap().contains("couldn't parse"), "{:?}", app.log.last());
}

/// 's' with anything other than a Trim modifier focused should refuse to
/// start a scrub session, before ever touching a process/socket -- these
/// all exercise the validation path only, so no real mpv is involved.
#[test]
fn start_scrub_requires_a_trim_modifier_focused() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    app.focus = Focus::Output(0);
    app.start_scrub();
    assert!(app.log.last().unwrap().contains("focus a trim node first"), "{:?}", app.log.last());

    let modifier = app.graph.add_modifier(ModifierKind::Metadata { fields: BTreeMap::new() });
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);
    app.start_scrub();
    assert!(app.log.last().unwrap().contains("only a trim node can be scrubbed"), "{:?}", app.log.last());
}

/// A Trim modifier with nothing wired into its input has no source to
/// scrub -- should be rejected the same way every other "connect this
/// first" action is.
#[test]
fn start_scrub_requires_a_connected_source() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::Filter { name: FilterName::Trim, fields: BTreeMap::new() });
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.start_scrub();

    assert!(app.log.last().unwrap().contains("connect a stream to this node first"), "{:?}", app.log.last());
    assert!(app.scrub.is_none(), "no session should have started");
}

/// A Trim node whose upstream chain is broken (a modifier with nothing
/// feeding *it*) has no resolvable source either.
#[test]
fn start_scrub_rejects_a_broken_chain() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let upstream = app.graph.add_modifier(ModifierKind::Convert(Codec::Copy)); // nothing wired into this
    let modifier = app.graph.add_modifier(ModifierKind::Filter { name: FilterName::Trim, fields: BTreeMap::new() });
    app.graph.connect(Endpoint::ModifierOut(upstream), Target::ModifierIn(modifier));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.start_scrub();

    assert!(app.log.last().unwrap().contains("broken chain"), "{:?}", app.log.last());
    assert!(app.scrub.is_none(), "no session should have started");
}

/// A Trim node fed by a Concat node's output has no single file to open
/// in mpv (the joined timeline only exists as a virtual filtergraph, not
/// a file on disk) -- should be refused with a clear, actionable message
/// rather than failing obscurely trying to resolve a path.
#[test]
fn start_scrub_rejects_a_concat_upstream_source() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let a = app.graph.add_input("a.mp4".to_string(), video_stream(), Vec::new());
    let b = app.graph.add_input("b.mp4".to_string(), video_stream(), Vec::new());
    let concat = app.graph.add_modifier(ModifierKind::Concat);
    app.graph.connect(Endpoint::Stream { node: a, stream_idx: 0 }, Target::ModifierIn(concat));
    app.graph.connect(Endpoint::Stream { node: b, stream_idx: 0 }, Target::ModifierIn(concat));
    let modifier = app.graph.add_modifier(ModifierKind::Filter { name: FilterName::Trim, fields: BTreeMap::new() });
    app.graph.connect(Endpoint::ModifierOut(concat), Target::ModifierIn(modifier));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = Focus::Modifier(modifier_idx);

    app.start_scrub();

    assert!(app.log.last().unwrap().contains("can't scrub a concatenated source"), "{:?}", app.log.last());
    assert!(app.scrub.is_none(), "no session should have started");
}

/// A field with a fixed set of valid values (Rotate's "direction") should
/// offer a selection picker instead of free-text entry -- ffmpeg only
/// accepts a handful of exact strings there, so anything else typed is
/// simply wrong, not just unusual.
#[test]
fn filter_field_with_fixed_values_opens_a_selection_picker_not_free_text() {
    use crate::app::{App, Focus, Mode, PickerKind};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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

    let Mode::TextInput { target, input, .. } = &app.mode else {
        panic!("expected text input mode");
    };
    let buffer = input.value();
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

    let Mode::TextInput { input, .. } = &app.mode else {
        panic!("expected text input mode");
    };
    assert!(input.value().is_empty());
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
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    app.confirm_text_input();

    let Mode::TextInput { target, input, .. } = &app.mode else {
        panic!("expected the value prompt to open next");
    };
    let buffer = input.value();
    assert!(matches!(target, TextTarget::ModifierMetadataValue { modifier: m, key } if *m == modifier && key == "rotate"));
    assert!(buffer.is_empty());

    for c in "90".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
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
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    app.confirm_text_input();
    let ModifierKind::Metadata { fields } = &app.graph.modifier_mut(modifier).unwrap().kind else { unreachable!() };
    assert_eq!(fields.get("language").map(String::as_str), Some("fra"));

    pick(&mut app, "title");
    for c in "Behind the Scenes".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    app.confirm_text_input();
    let ModifierKind::Metadata { fields } = &app.graph.modifier_mut(modifier).unwrap().kind else { unreachable!() };
    assert_eq!(fields.get("title").map(String::as_str), Some("Behind the Scenes"));
    assert_eq!(fields.len(), 2, "both fields should coexist");

    // Clearing: re-pick "language" (buffer pre-fills with "fra"), empty it,
    // then confirm -- the key should be removed from the map entirely.
    pick(&mut app, "language");
    for _ in 0.."fra".len() {
        app.text_input_handle_key(key(KeyCode::Backspace));
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
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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

/// 'y' on a focused input should append a second input node with the same
/// path/streams/chapters/extra_args (a fresh re-derivation, not literally
/// the same `Vec`s), leave the original untouched, and focus the new one --
/// an input has nothing "incoming" to copy, so this is the whole check.
#[test]
fn duplicate_focused_node_on_input_copies_config_not_wires() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input(
        "in.mp4".to_string(),
        video_audio_streams(),
        vec![Chapter::new(0.0, 10.0, "one".to_string())],
    );
    app.graph.input_mut(id).unwrap().extra_args = filter_fields(&[("itsoffset", "1.5")]);
    let out = app.graph.outputs[0].id;
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));

    app.focus = Focus::Input(0);
    app.duplicate_focused_node();

    assert_eq!(app.graph.inputs.len(), 2);
    assert!(matches!(app.focus, Focus::Input(1)));
    let (original, duplicate) = (&app.graph.inputs[0], &app.graph.inputs[1]);
    assert_eq!(duplicate.path, original.path);
    assert_eq!(duplicate.streams.len(), original.streams.len(), "including the derived chapter-stream row");
    assert_eq!(duplicate.chapters.len(), 1);
    assert_eq!(duplicate.extra_args.get("itsoffset"), Some(&"1.5".to_string()));
    assert_eq!(app.graph.outgoing(Endpoint::Stream { node: duplicate.id, stream_idx: 0 }).len(), 0, "no wires copied");
    assert_eq!(
        app.graph.outgoing(Endpoint::Stream { node: original.id, stream_idx: 0 }).len(),
        1,
        "original's own wire must be untouched"
    );
}

/// 'y' on a focused modifier should append a second one of the same kind
/// with the same fields, wired to the same source(s) the original has (so
/// e.g. duplicating a Concat node copies every one of its segments), but
/// not wired to anywhere the original's *output* already feeds.
#[test]
fn duplicate_focused_node_on_modifier_copies_kind_and_incoming_wires() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let modifier = app.graph.add_modifier(ModifierKind::Metadata { fields: metadata_fields(&[("language", "eng")]) });
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(modifier));
    let out = app.graph.outputs[0].id;
    app.graph.connect(Endpoint::ModifierOut(modifier), Target::Output(out));
    let modifier_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();

    app.focus = Focus::Modifier(modifier_idx);
    app.duplicate_focused_node();

    assert_eq!(app.graph.modifiers.len(), 2);
    assert!(matches!(app.focus, Focus::Modifier(1)));
    let duplicate_id = app.graph.modifiers[1].id;
    let ModifierKind::Metadata { fields } = &app.graph.modifiers[1].kind else { panic!("wrong kind") };
    assert_eq!(fields.get("language"), Some(&"eng".to_string()));
    assert_eq!(
        app.graph.incoming(Target::ModifierIn(duplicate_id)).len(),
        1,
        "the incoming wire should be copied"
    );
    assert!(app.graph.outgoing(Endpoint::ModifierOut(duplicate_id)).is_empty(), "no outgoing wire copied");
    assert_eq!(
        app.graph.outgoing(Endpoint::ModifierOut(modifier)).len(),
        1,
        "original's own outgoing wire must be untouched"
    );
}

/// 'y' on a focused output should append a second one with the same
/// container/extra_args and every mapped stream (plus chapters, if any)
/// the original has, but a distinct default path so the two don't
/// silently collide.
#[test]
fn duplicate_focused_node_on_output_copies_settings_and_incoming_wires() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input(
        "in.mp4".to_string(),
        video_stream(),
        vec![Chapter::new(0.0, 10.0, "one".to_string())],
    );
    let out = app.graph.outputs[0].id;
    app.graph.outputs[0].path = "output.mkv".to_string();
    app.graph.outputs[0].container = Some("matroska".to_string());
    app.graph.outputs[0].extra_args = filter_fields(&[("movflags", "+faststart")]);
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 1 }, Target::OutputChapters(out));

    app.focus = Focus::Output(0);
    app.duplicate_focused_node();

    assert_eq!(app.graph.outputs.len(), 2);
    assert!(matches!(app.focus, Focus::Output(1)));
    let duplicate = &app.graph.outputs[1];
    assert_eq!(duplicate.path, "output-copy.mkv", "should get a distinct default path, not collide with the original");
    assert_eq!(duplicate.container, Some("matroska".to_string()));
    assert_eq!(duplicate.extra_args.get("movflags"), Some(&"+faststart".to_string()));
    assert_eq!(app.graph.incoming(Target::Output(duplicate.id)).len(), 1);
    assert_eq!(app.graph.incoming(Target::OutputChapters(duplicate.id)).len(), 1);
    assert_eq!(app.graph.incoming(Target::Output(out)).len(), 1, "original's own mapping must be untouched");
}

/// Duplicating an output whose default "-copy" path is already taken by a
/// third output should keep incrementing until it finds a free one.
#[test]
fn duplicate_focused_node_on_output_increments_past_an_existing_copy_path() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    app.graph.outputs[0].path = "output.mkv".to_string();
    app.graph.add_output();
    app.graph.outputs[1].path = "output-copy.mkv".to_string();

    app.focus = Focus::Output(0);
    app.duplicate_focused_node();

    assert_eq!(app.graph.outputs.len(), 3);
    assert_eq!(app.graph.outputs[2].path, "output-copy2.mkv");
}

/// Picker escape/search machinery is generic over picker kind -- verify it
/// works correctly when opened via the modifier-based codec picker.
#[test]
fn picker_search_and_escape_work_through_convert_modifier_flow() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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
    app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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
    assert!(app.graph.build_ffmpeg_args(&BTreeMap::new()).windows(2).any(|w| w == ["-f", "webm"]));
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
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap().streams;
    let id = app.graph.add_input(source_path.to_str().unwrap().to_string(), streams, Vec::new());
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

/// A run that fails outright (here, ffmpeg refusing a nonexistent input
/// file, a real, deterministic, near-instant way to get `[error]`/`[fatal]`
/// -tagged lines out of a real ffmpeg process -- see `ffmpeg::
/// classify_log_line`) should have `poll_ffmpeg` count every one of them
/// into `log_errors`, color them in the log (checked indirectly here via
/// `classify_log_line`, since this test has no terminal to render into),
/// and fold the count into the final status line alongside the exit code,
/// exactly the summary `App::log_issue_summary` builds.
#[test]
fn poll_ffmpeg_counts_tagged_error_lines_and_summarizes_them_in_the_status() {
    use crate::app::App;

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let id = app.graph.add_input("/no/such/file-tff-test-only.mkv".to_string(), video_stream(), Vec::new());
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));

    app.start_render();
    assert!(app.running);
    assert_eq!(app.log_errors, 0, "nothing counted yet -- the process has barely started");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while app.running && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(20));
        app.poll_ffmpeg();
    }

    assert!(!app.running, "the failing render did not finish in time");
    assert!(app.log_errors > 0, "opening a nonexistent input should produce at least one [error]/[fatal] line");
    assert_eq!(app.log_warnings, 0);
    assert!(
        app.log.iter().any(|l| crate::ffmpeg::classify_log_line(l) == Some(crate::ffmpeg::LogSeverity::Error)),
        "at least one logged line should itself classify as an error: {:?}",
        app.log
    );
    let expected_suffix = format!("({} error{})", app.log_errors, if app.log_errors == 1 { "" } else { "s" });
    assert!(
        app.status.contains(&expected_suffix),
        "status should summarize the error count: {} (expected to contain {expected_suffix:?})",
        app.status
    );
    assert!(app.status.starts_with("ffmpeg exited with code"), "{}", app.status);
}

/// ffmpeg's periodic progress reports (`frame=... fps=... time=...`) are
/// separated by a bare `\r`, not `\n` (see `ffmpeg::stream_raw_lines`'s doc
/// comment) -- `poll_ffmpeg` has to fold every one of those into a single,
/// continuously-overwritten log entry rather than letting them either (a)
/// glue into one ever-growing line (the original bug report this whole
/// feature started from) or (b) each become their own permanent line
/// (correct-looking but floods the log over a long render). Forces several
/// updates to actually happen within a real, short render via a tiny
/// `-stats_period` (a real ffmpeg option, set directly through the output
/// node's `extra_args` escape hatch -- see `output_extra_arg_keys`'s doc
/// comment for why it isn't in the curated picker list, though the
/// underlying data model accepts any key) combined with a slow-enough
/// preset that the render can't finish before several of them land.
#[test]
fn poll_ffmpeg_folds_repeated_progress_updates_into_one_overwritten_line() {
    use crate::app::App;

    let dir = std::env::temp_dir().join(format!("tff-test-progress-fold-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = make_test_source(&dir, 1, 1280, 720);
    let out_path = dir.join("out.mkv");

    let mut app = App::new();
    let streams = ffmpeg::probe(source_path.to_str().unwrap()).unwrap().streams;
    let id = app.graph.add_input(source_path.to_str().unwrap().to_string(), streams, Vec::new());
    let convert = app.graph.add_modifier(ModifierKind::Convert(Codec::Encode("libx264".to_string())));
    let out = app.graph.outputs[0].id;
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(convert));
    app.graph.connect(Endpoint::ModifierOut(convert), Target::Output(out));
    app.graph.outputs[0].path = out_path.to_str().unwrap().to_string();
    app.graph.outputs[0].extra_args = filter_fields(&[("preset", "veryslow"), ("stats_period", "0.05")]);

    app.start_render();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while app.running && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
        app.poll_ffmpeg();
    }
    assert!(!app.running, "render did not finish in time");
    assert!(app.status.starts_with("ffmpeg exited with code 0"), "{}", app.status);

    // The original bug: several `[info]`-tagged progress reports glued
    // into one giant line with no separator at all.
    assert!(
        app.log.iter().all(|l| l.matches("frame=").count() <= 1),
        "no single log line should contain more than one progress report: {:?}",
        app.log
    );
    // The log-spam alternative to that bug: every progress update kept as
    // its own permanent line instead of overwriting the last one.
    let progress_lines = app.log.iter().filter(|l| l.contains("frame=")).count();
    assert!(
        progress_lines <= 3,
        "expected repeated progress updates to overwrite one line, not accumulate ({progress_lines} found): {:?}",
        app.log
    );
    // The overwrite marker is purely poll_ffmpeg's own bookkeeping (see
    // `ffmpeg::PROGRESS_LINE_PREFIX`) -- `ui::draw_log` strips it for
    // display, but nothing should scrub it out of the stored log itself.
    assert!(
        app.log.iter().any(|l| l.starts_with(crate::ffmpeg::PROGRESS_LINE_PREFIX)),
        "expected at least one stored line to still carry the internal marker: {:?}",
        app.log
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 'e' with a modifier focused should dispatch to the same thing
/// `activate_modifier` does directly -- one key, "edit this node,"
/// regardless of what kind of node is focused.
#[test]
fn activate_focused_on_modifier_dispatches_to_activate_modifier() {
    use crate::app::{App, Focus, Mode, PickerKind};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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

/// The input extra-args picker should only list the teletext decoder's
/// options (`txt_format`/`txt_page`/`txt_duration`) when
/// `App::has_teletext_decoder` says the configured ffmpeg actually has
/// that decoder -- checked directly against the field here rather than
/// spawning a real ffmpeg, since `App::new()`'s own query result depends
/// on whatever's installed on the machine running this test.
#[test]
fn extra_args_picker_only_lists_teletext_options_when_the_decoder_is_available() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let idx = app.graph.inputs.iter().position(|n| n.id == id).unwrap();
    app.focus = Focus::Input(idx);

    app.has_teletext_decoder = false;
    app.activate_focused();
    let Mode::Picker { options, .. } = &app.mode else { panic!("expected picker mode") };
    let displays: Vec<&String> = options.iter().map(|o| &o.display).collect();
    assert!(!displays.iter().any(|d| d.contains("txt_format")), "{displays:?}");
    assert!(!displays.iter().any(|d| d.contains("txt_page")), "{displays:?}");
    assert!(!displays.iter().any(|d| d.contains("txt_duration")), "{displays:?}");

    app.has_teletext_decoder = true;
    app.activate_focused();
    let Mode::Picker { options, .. } = &app.mode else { panic!("expected picker mode") };
    let displays: Vec<&String> = options.iter().map(|o| &o.display).collect();
    assert!(displays.iter().any(|d| d.contains("txt_format")), "{displays:?}");
    assert!(displays.iter().any(|d| d.contains("txt_page")), "{displays:?}");
    assert!(displays.iter().any(|d| d.contains("txt_duration")), "{displays:?}");
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

/// `ss`/`to` are shown under friendlier labels than their bare, cryptic
/// ffmpeg flag names -- but picking one still edits the value keyed by the
/// real flag name, and the text-input prompt that opens for it repeats the
/// same friendly label, not the raw key.
#[test]
fn extra_args_picker_shows_friendly_labels_for_ss_and_to() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    app.graph.outputs[0].extra_args = filter_fields(&[("ss", "1")]);
    app.focus = Focus::Output(0);

    app.activate_focused();
    let Mode::Picker { options, .. } = &app.mode else { panic!("expected picker mode") };
    let displays: Vec<&String> = options.iter().map(|o| &o.display).collect();
    assert!(options.iter().any(|o| o.display == "trim start (ss): 1"), "{displays:?}");
    assert!(options.iter().any(|o| o.display == "trim end (to): (not set)"), "{displays:?}");

    let row = options.iter().position(|o| o.value.as_deref() == Some("ss")).unwrap();
    app.picker_move(row as isize);
    app.picker_confirm();

    let Mode::TextInput { target, .. } = &app.mode else { panic!("expected text-input mode") };
    let crate::app::TextTarget::ExtraArgValue { target, key } = target else {
        panic!("expected ExtraArgValue target")
    };
    assert_eq!(crate::app::extra_arg_label(*target, key, app.has_teletext_decoder), "trim start (ss)");
}

/// Picking a curated valueless flag (e.g. "re") should toggle it in place
/// and keep the picker open -- same idea as disposition flags -- rather
/// than opening a value prompt for a flag that doesn't take one.
#[test]
fn extra_args_picker_toggles_valueless_flag_in_place() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let idx = app.graph.inputs.iter().position(|n| n.id == id).unwrap();
    app.focus = Focus::Input(idx);
    app.activate_focused();

    let Mode::Picker { options, .. } = &app.mode else { panic!("expected picker mode") };
    let row = options.iter().position(|o| o.value.as_deref() == Some("itsoffset")).unwrap();
    app.picker_move(row as isize);
    app.picker_confirm();

    let Mode::TextInput { target, input, .. } = &app.mode else {
        panic!("expected text input mode");
    };
    assert!(matches!(target, TextTarget::ExtraArgValue { key, .. } if key == "itsoffset"));
    assert_eq!(input.value(), "");

    for c in "1.5".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
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
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    app.confirm_text_input();

    let Mode::TextInput { target, input, .. } = &app.mode else { panic!("expected value prompt to open") };
    let buffer = input.value();
    assert!(matches!(target, TextTarget::ExtraArgValue { key, .. } if key == "fflags"));
    assert_eq!(buffer, "");

    for c in "+genpts".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    app.confirm_text_input();

    assert_eq!(app.graph.outputs[0].extra_args.get("fflags"), Some(&"+genpts".to_string()));
}

/// PageUp/PageDown should page through the log by a full screen at a time,
/// floor at the very start rather than underflowing, and -- reaching the
/// live bottom again -- snap back to `None` (pinned/live) rather than
/// getting stuck on a stale `Some(live_start)`.
#[test]
fn scroll_log_pages_up_then_back_down_to_live() {
    use crate::app::App;

    let mut app = App::new();
    let inner_height = (crate::ui::LOG_PANE_HEIGHT as usize) - 2;
    app.log.clear();
    for i in 0..(inner_height * 3) {
        app.log.push(format!("line {i}"));
    }
    assert_eq!(app.log_scroll, None);

    app.scroll_log(false); // PageUp: live_start (16) -> 8
    assert_eq!(app.log_scroll, Some(inner_height));
    app.scroll_log(false); // PageUp: 8 -> 0
    assert_eq!(app.log_scroll, Some(0));
    app.scroll_log(false); // PageUp at the very top: floors, stays 0
    assert_eq!(app.log_scroll, Some(0));

    app.scroll_log(true); // PageDown: 0 -> 8
    assert_eq!(app.log_scroll, Some(inner_height));
    app.scroll_log(true); // PageDown: 8 -> live_start (16), snaps to pinned/live
    assert_eq!(app.log_scroll, None);
    app.scroll_log(true); // PageDown while already pinned/live: stays pinned
    assert_eq!(app.log_scroll, None);
}

/// PageUp is a no-op when the whole log already fits on screen -- there's
/// nothing to scroll to, so it should stay pinned/live rather than
/// producing a `Some` that shows exactly the same lines anyway.
#[test]
fn scroll_log_page_up_is_a_no_op_when_everything_already_fits() {
    use crate::app::App;

    let mut app = App::new();
    app.log.clear();
    app.log.push("only one line".to_string());
    app.scroll_log(false);
    assert_eq!(app.log_scroll, None);
}

/// Ctrl+Right/Left should adjust `log_hscroll` by a fixed step, cap at the
/// longest currently visible line's own length (here, its only line) once
/// scrolling further wouldn't reveal anything new, and floor at zero
/// rather than underflowing -- same shape as `scroll_node_text`'s own
/// step/floor/cap test.
#[test]
fn scroll_log_horizontal_steps_floors_and_caps() {
    use crate::app::App;

    let mut app = App::new();
    app.log.clear();
    app.log.push("x".repeat(50));

    assert_eq!(app.log_hscroll, 0);
    app.scroll_log_horizontal(true);
    assert_eq!(app.log_hscroll, 8);
    app.scroll_log_horizontal(true);
    assert_eq!(app.log_hscroll, 16);

    for _ in 0..10 {
        app.scroll_log_horizontal(true);
    }
    assert_eq!(app.log_hscroll, 50, "should cap at the line's own length rather than growing unbounded");

    app.scroll_log_horizontal(false);
    assert_eq!(app.log_hscroll, 42);

    for _ in 0..10 {
        app.scroll_log_horizontal(false);
    }
    assert_eq!(app.log_hscroll, 0, "should floor at zero rather than underflowing");
}

/// The horizontal-scroll cap only considers the lines actually visible in
/// the current (vertically scrolled) window -- a much longer line sitting
/// outside that window, further up the log, shouldn't let horizontal
/// scroll run past what the visible lines themselves would ever need.
#[test]
fn scroll_log_horizontal_cap_only_considers_currently_visible_lines() {
    use crate::app::App;

    let mut app = App::new();
    let inner_height = (crate::ui::LOG_PANE_HEIGHT as usize) - 2;
    app.log.clear();
    app.log.push("x".repeat(100)); // scrolls out of the live view below
    for i in 0..inner_height {
        app.log.push(format!("short-{i}"));
    }

    for _ in 0..20 {
        app.scroll_log_horizontal(true);
    }
    let (start, end) = app.visible_log_range();
    let max_visible = app.log[start..end].iter().map(|l| l.chars().count()).max().unwrap() as u16;
    assert!(max_visible < 100, "sanity check: the long line shouldn't be part of the live view");
    assert_eq!(app.log_hscroll, max_visible);
}

/// 'c' on a Concat modifier with several armed sources should append all
/// of them as segments in one action (like an output's bulk-connect), not
/// reject the batch the way a single-input modifier would.
#[test]
fn toggle_connect_appends_every_armed_source_to_a_concat_modifier() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let a = app.graph.add_input("a.mp4".to_string(), video_stream(), Vec::new());
    let b = app.graph.add_input("b.mp4".to_string(), video_stream(), Vec::new());
    let concat = app.graph.add_modifier(ModifierKind::Concat);
    let concat_idx = app.graph.modifiers.iter().position(|m| m.id == concat).unwrap();

    app.armed = BTreeSet::from([
        Endpoint::Stream { node: a, stream_idx: 0 },
        Endpoint::Stream { node: b, stream_idx: 0 },
    ]);
    app.focus = Focus::Modifier(concat_idx);
    app.toggle_connect();

    assert!(app.armed.is_empty());
    assert_eq!(app.graph.incoming(Target::ModifierIn(concat)).len(), 2);
}

/// A source whose kind doesn't match a Concat node's existing segments
/// should be rejected without disturbing the segments already connected.
#[test]
fn toggle_connect_rejects_a_concat_segment_that_mismatches_the_existing_kind() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
    let concat = app.graph.add_modifier(ModifierKind::Concat);
    let concat_idx = app.graph.modifiers.iter().position(|m| m.id == concat).unwrap();

    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::ModifierIn(concat)); // video

    app.armed = BTreeSet::from([Endpoint::Stream { node: id, stream_idx: 1 }]); // audio
    app.focus = Focus::Modifier(concat_idx);
    app.toggle_connect();

    assert_eq!(app.graph.incoming(Target::ModifierIn(concat)).len(), 1, "the mismatched segment must not be added");
    assert!(
        app.log.iter().any(|l| l.contains("can't mix in a audio one")),
        "{:?}",
        app.log
    );
}

/// 'd' on a Concat modifier's segment row should remove just that segment,
/// not every incoming wire -- mirroring an output's per-row disconnect.
#[test]
fn disconnect_focused_removes_just_the_hovered_concat_segment() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let a = app.graph.add_input("a.mp4".to_string(), video_stream(), Vec::new());
    let b = app.graph.add_input("b.mp4".to_string(), video_stream(), Vec::new());
    let concat = app.graph.add_modifier(ModifierKind::Concat);
    let concat_idx = app.graph.modifiers.iter().position(|m| m.id == concat).unwrap();
    app.graph.connect(Endpoint::Stream { node: a, stream_idx: 0 }, Target::ModifierIn(concat));
    app.graph.connect(Endpoint::Stream { node: b, stream_idx: 0 }, Target::ModifierIn(concat));

    app.focus = Focus::Modifier(concat_idx);
    app.row_idx = 0;
    app.disconnect_focused();

    let incoming = app.graph.incoming(Target::ModifierIn(concat));
    assert_eq!(incoming.len(), 1);
    assert_eq!(app.graph.wires[incoming[0]].from, Endpoint::Stream { node: b, stream_idx: 0 });
}

/// 'd' on a Concat modifier's *outgoing* row (past the segment rows)
/// should disconnect that downstream wire, not a segment -- row_idx spans
/// segments then outgoing wires, same combined indexing `cycle_row` uses.
#[test]
fn disconnect_focused_on_a_concat_outgoing_row_leaves_segments_untouched() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let a = app.graph.add_input("a.mp4".to_string(), video_stream(), Vec::new());
    let b = app.graph.add_input("b.mp4".to_string(), video_stream(), Vec::new());
    let concat = app.graph.add_modifier(ModifierKind::Concat);
    let concat_idx = app.graph.modifiers.iter().position(|m| m.id == concat).unwrap();
    app.graph.connect(Endpoint::Stream { node: a, stream_idx: 0 }, Target::ModifierIn(concat));
    app.graph.connect(Endpoint::Stream { node: b, stream_idx: 0 }, Target::ModifierIn(concat));
    let out = app.graph.outputs[0].id;
    app.graph.connect(Endpoint::ModifierOut(concat), Target::Output(out));

    app.focus = Focus::Modifier(concat_idx);
    app.row_idx = 2; // past the two segment rows -- the one outgoing row
    app.disconnect_focused();

    assert_eq!(app.graph.incoming(Target::ModifierIn(concat)).len(), 2, "segments should be untouched");
    assert!(app.graph.outgoing(Endpoint::ModifierOut(concat)).is_empty());
}

/// Ctrl+Up/Down while focused on a Concat segment row should reorder that
/// segment, changing the join order (see `Graph::swap_wires`), same as an
/// output's mapped-stream reordering.
#[test]
fn move_focused_row_reorders_a_concat_segment() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let a = app.graph.add_input("a.mp4".to_string(), video_stream(), Vec::new());
    let b = app.graph.add_input("b.mp4".to_string(), video_stream(), Vec::new());
    let concat = app.graph.add_modifier(ModifierKind::Concat);
    let concat_idx = app.graph.modifiers.iter().position(|m| m.id == concat).unwrap();
    app.graph.connect(Endpoint::Stream { node: a, stream_idx: 0 }, Target::ModifierIn(concat));
    app.graph.connect(Endpoint::Stream { node: b, stream_idx: 0 }, Target::ModifierIn(concat));

    app.focus = Focus::Modifier(concat_idx);
    app.row_idx = 0;
    app.move_focused_row(true);

    let order: Vec<Endpoint> = app
        .graph
        .incoming(Target::ModifierIn(concat))
        .into_iter()
        .map(|wi| app.graph.wires[wi].from)
        .collect();
    assert_eq!(order, vec![Endpoint::Stream { node: b, stream_idx: 0 }, Endpoint::Stream { node: a, stream_idx: 0 }]);
    assert_eq!(app.row_idx, 1);
}

/// `cycle_row` on a Concat modifier should cycle through its segment rows
/// and then its outgoing rows as one combined list, same combined scheme
/// as an output's mapped-streams-then-chapters list.
#[test]
fn cycle_row_combines_concat_segments_and_outgoing_rows() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let a = app.graph.add_input("a.mp4".to_string(), video_stream(), Vec::new());
    let b = app.graph.add_input("b.mp4".to_string(), video_stream(), Vec::new());
    let concat = app.graph.add_modifier(ModifierKind::Concat);
    let concat_idx = app.graph.modifiers.iter().position(|m| m.id == concat).unwrap();
    app.graph.connect(Endpoint::Stream { node: a, stream_idx: 0 }, Target::ModifierIn(concat));
    app.graph.connect(Endpoint::Stream { node: b, stream_idx: 0 }, Target::ModifierIn(concat));
    let out = app.graph.outputs[0].id;
    app.graph.connect(Endpoint::ModifierOut(concat), Target::Output(out));

    app.focus = Focus::Modifier(concat_idx);
    app.row_idx = 0;
    app.cycle_row(true);
    assert_eq!(app.row_idx, 1); // second segment
    app.cycle_row(true);
    assert_eq!(app.row_idx, 2); // the one outgoing row
    app.cycle_row(true);
    assert_eq!(app.row_idx, 0, "should wrap back around to the first segment");
}



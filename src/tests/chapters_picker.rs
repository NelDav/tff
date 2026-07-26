use super::*;

/// Focuses the given `ChapterEdit` modifier by id -- most of these tests
/// drive the flow starting from 'e' on that node, same as a real user.
fn focus_modifier(app: &mut crate::app::App, modifier: NodeId) {
    let idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = crate::app::Focus::Modifier(idx);
}

/// Drives the chapter table to a specific chapter's column: focuses the
/// node, 'e' (opens the table directly), then navigates row/col.
fn open_chapter_table_at(app: &mut crate::app::App, modifier: NodeId, index: usize, col: crate::app::ChapterColumn) {
    use crate::app::ChapterColumn;

    focus_modifier(app, modifier);
    app.activate_focused();
    for _ in 0..index {
        app.chapter_table_move_row(true);
    }
    let steps = match col {
        ChapterColumn::Start => 0,
        ChapterColumn::End => 1,
        ChapterColumn::Title => 2,
    };
    for _ in 0..steps {
        app.chapter_table_move_col(true);
    }
}

/// Time/title fields prefill their buffer with the current value (like
/// Metadata's), so replacing it means clearing what's there first --
/// otherwise the new text is appended onto the old, not substituted.
fn retype(app: &mut crate::app::App, text: &str) {
    let crate::app::Mode::TextInput { input, .. } = &app.mode else { panic!("expected text input mode") };
    let buffer = input.value();
    let len = buffer.chars().count();
    for _ in 0..len {
        app.text_input_handle_key(key(KeyCode::Backspace));
    }
    for c in text.chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
}

/// 'e' on a focused `ChapterEdit` modifier should open its chapter table
/// directly (no intermediate picker) -- same one-step pattern as 'e' on a
/// Metadata or Disposition node -- landing on the first chapter's start
/// column.
#[test]
fn activate_focused_on_chapter_edit_modifier_opens_chapter_table() {
    use crate::app::{App, ChapterColumn, Mode};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::ChapterEdit {
        chapters: vec![Chapter::new(0.0, 65.0, "Intro".to_string())],
    });
    focus_modifier(&mut app, modifier);

    app.activate_focused();

    let Mode::ChapterTable { modifier: m, row, col } = &app.mode else { panic!("expected chapter table mode") };
    assert_eq!(*m, modifier);
    assert_eq!(*row, 0);
    assert_eq!(*col, ChapterColumn::Start);
}

/// Adding a new chapter should prefill its start time with the previous
/// chapter's end time, so a chain of Enter-on-the-add-row -> set end ->
/// Enter-on-the-add-row again carries the end forward as the next start
/// without retyping it. Also verifies that Enter on the "add chapter" row
/// adds the chapter immediately, with no further menu in between.
#[test]
fn adding_a_chapter_prefills_start_from_previous_chapters_end() {
    use crate::app::{App, ChapterTimeField, Mode, TextTarget};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::ChapterEdit { chapters: Vec::new() });
    focus_modifier(&mut app, modifier);

    // The list starts empty, so 'e' lands straight on the "add chapter"
    // row (row 0, since there are no real chapters yet).
    app.activate_focused();
    let Mode::ChapterTable { row, .. } = &app.mode else { panic!() };
    assert_eq!(*row, 0);

    // Enter adds the chapter immediately -- no field editor in between.
    app.chapter_table_confirm();
    assert_eq!(chapter_edit_chapters(&app.graph, modifier).unwrap().len(), 1);
    assert_eq!(chapter_edit_chapters(&app.graph, modifier).unwrap()[0].start_secs, 0.0);
    let Mode::ChapterTable { row, .. } = &app.mode else { panic!("should stay in the table") };
    assert_eq!(*row, 0, "cursor should land on the newly added chapter");

    // Set its end time to 90 seconds via the "end" column.
    app.chapter_table_move_col(true); // Start -> End
    app.chapter_table_confirm();
    let Mode::TextInput { target, .. } = &app.mode else {
        panic!("expected time text input");
    };
    assert!(matches!(target, TextTarget::ChapterTime { field: ChapterTimeField::End, .. }));
    retype(&mut app, "1:30");
    app.confirm_text_input();
    assert_eq!(chapter_edit_chapters(&app.graph, modifier).unwrap()[0].end_secs, 90.0);

    // Back in the table (same cell) -- move down to the trailing "add
    // chapter" row and add a second one.
    let Mode::ChapterTable { row, .. } = &app.mode else { panic!("should return to the table") };
    assert_eq!(*row, 0);
    app.chapter_table_move_row(true);
    app.chapter_table_confirm();

    let chapters = chapter_edit_chapters(&app.graph, modifier).unwrap();
    assert_eq!(chapters.len(), 2);
    assert_eq!(chapters[1].start_secs, 90.0, "new chapter's start should be prefilled from the previous chapter's end");
}

/// Both HH:MM:SS and plain-seconds should be accepted when editing a
/// chapter's start/end, and the stored value should reflect whichever
/// format was typed.
#[test]
fn chapter_time_field_accepts_both_time_formats() {
    use crate::app::{App, ChapterColumn, ChapterTimeField, Mode, TextTarget};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::ChapterEdit {
        chapters: vec![Chapter::new(0.0, 0.0, String::new())],
    });

    open_chapter_table_at(&mut app, modifier, 0, ChapterColumn::Start);
    app.chapter_table_confirm();
    let Mode::TextInput { target, .. } = &app.mode else { panic!() };
    assert!(matches!(target, TextTarget::ChapterTime { field: ChapterTimeField::Start, .. }));
    retype(&mut app, "5.5");
    app.confirm_text_input();
    assert_eq!(chapter_edit_chapters(&app.graph, modifier).unwrap()[0].start_secs, 5.5);

    open_chapter_table_at(&mut app, modifier, 0, ChapterColumn::End);
    app.chapter_table_confirm();
    retype(&mut app, "00:02:00");
    app.confirm_text_input();
    assert_eq!(chapter_edit_chapters(&app.graph, modifier).unwrap()[0].end_secs, 120.0);
}

/// An unparsable time should be rejected without modifying the chapter,
/// and should reopen the table at the same cell so the user doesn't lose
/// their place.
#[test]
fn chapter_time_field_rejects_unparsable_input_without_modifying() {
    use crate::app::{App, ChapterColumn, Mode};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::ChapterEdit {
        chapters: vec![Chapter::new(3.0, 9.0, String::new())],
    });

    open_chapter_table_at(&mut app, modifier, 0, ChapterColumn::Start);
    app.chapter_table_confirm();
    retype(&mut app, "garbage");
    app.confirm_text_input();

    assert_eq!(
        chapter_edit_chapters(&app.graph, modifier).unwrap()[0].start_secs,
        3.0,
        "unparsable input shouldn't modify the chapter"
    );
    let Mode::ChapterTable { row, col, .. } = &app.mode else { panic!("should reopen the table") };
    assert_eq!(*row, 0);
    assert_eq!(*col, ChapterColumn::Start);
}

/// Esc while editing a chapter's start/end/title should cancel just that
/// text edit and land back in the chapter table at the same cell, not
/// throw away the whole chapter-editing session by dropping to Normal --
/// unlike every other text input in the app, this one was reached by
/// drilling into a table, not straight from Normal.
#[test]
fn escaping_a_chapter_cell_edit_returns_to_the_table_not_normal() {
    use crate::app::{App, ChapterColumn, Mode};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::ChapterEdit {
        chapters: vec![Chapter::new(3.0, 9.0, "Original".to_string())],
    });

    open_chapter_table_at(&mut app, modifier, 0, ChapterColumn::End);
    app.chapter_table_confirm();
    retype(&mut app, "99");
    app.cancel_text_input();

    assert_eq!(
        chapter_edit_chapters(&app.graph, modifier).unwrap()[0].end_secs,
        9.0,
        "cancelling the edit shouldn't change the chapter"
    );
    let Mode::ChapterTable { row, col, .. } = &app.mode else {
        panic!("Esc should return to the chapter table, not Normal: {:?}", std::mem::discriminant(&app.mode))
    };
    assert_eq!(*row, 0);
    assert_eq!(*col, ChapterColumn::End);

    // Same for the title field.
    open_chapter_table_at(&mut app, modifier, 0, ChapterColumn::Title);
    app.chapter_table_confirm();
    retype(&mut app, "Something Else");
    app.cancel_text_input();
    assert_eq!(chapter_edit_chapters(&app.graph, modifier).unwrap()[0].title, "Original");
    assert!(matches!(&app.mode, Mode::ChapterTable { col: ChapterColumn::Title, .. }));
}

/// Editing a chapter's title should round-trip through the text input.
#[test]
fn chapter_title_field_round_trips() {
    use crate::app::{App, ChapterColumn};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::ChapterEdit {
        chapters: vec![Chapter::new(0.0, 1.0, String::new())],
    });

    open_chapter_table_at(&mut app, modifier, 0, ChapterColumn::Title);
    app.chapter_table_confirm();
    for c in "The Beginning".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    app.confirm_text_input();

    assert_eq!(chapter_edit_chapters(&app.graph, modifier).unwrap()[0].title, "The Beginning");
}

/// 'd' on a chapter row should remove it and stay in the table; it's a
/// no-op on the trailing "add chapter" row.
#[test]
fn chapter_table_delete_removes_chapter_and_stays_in_table() {
    use crate::app::{App, ChapterColumn, Mode};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::ChapterEdit {
        chapters: vec![
            Chapter::new(0.0, 1.0, "Keep".to_string()),
            Chapter::new(1.0, 2.0, "Delete Me".to_string()),
        ],
    });

    open_chapter_table_at(&mut app, modifier, 1, ChapterColumn::Start);
    app.chapter_table_delete();

    let chapters = chapter_edit_chapters(&app.graph, modifier).unwrap();
    assert_eq!(chapters.len(), 1);
    assert_eq!(chapters[0].title, "Keep");
    assert!(matches!(&app.mode, Mode::ChapterTable { .. }));

    // Deleting on the trailing "add chapter" row is a no-op.
    open_chapter_table_at(&mut app, modifier, 1, ChapterColumn::Start); // row 1 == the add-row now
    app.chapter_table_delete();
    assert_eq!(chapter_edit_chapters(&app.graph, modifier).unwrap().len(), 1);
}

/// Tab walks every cell in reading order, wrapping from a row's last
/// column to the next row's first -- including onto the trailing "add
/// chapter" row -- rather than staying within one row like Left/Right.
/// Shift+Tab (backward) retraces the same path.
#[test]
fn tab_walks_the_table_in_reading_order_wrapping_rows() {
    use crate::app::{App, ChapterColumn};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::ChapterEdit {
        chapters: vec![Chapter::new(0.0, 1.0, "A".to_string()), Chapter::new(1.0, 2.0, "B".to_string())],
    });

    let cell = |app: &App| {
        let crate::app::Mode::ChapterTable { row, col, .. } = &app.mode else { panic!("expected chapter table") };
        (*row, *col)
    };

    open_chapter_table_at(&mut app, modifier, 0, ChapterColumn::Start);
    assert_eq!(cell(&app), (0, ChapterColumn::Start));

    app.chapter_table_tab(true);
    assert_eq!(cell(&app), (0, ChapterColumn::End));
    app.chapter_table_tab(true);
    assert_eq!(cell(&app), (0, ChapterColumn::Title));

    // Past the last column of row 0, Tab wraps to the first column of row 1.
    app.chapter_table_tab(true);
    assert_eq!(cell(&app), (1, ChapterColumn::Start));

    app.chapter_table_tab(true);
    app.chapter_table_tab(true);
    assert_eq!(cell(&app), (1, ChapterColumn::Title));

    // Past the last real chapter's last column, Tab lands on the add row.
    app.chapter_table_tab(true);
    assert_eq!(cell(&app), (2, ChapterColumn::Start));

    // The add row is the end of the line -- Tab forward is a no-op there.
    app.chapter_table_tab(true);
    assert_eq!(cell(&app), (2, ChapterColumn::Start));

    // Shift+Tab retraces the exact same path backward.
    app.chapter_table_tab(false);
    assert_eq!(cell(&app), (1, ChapterColumn::Title));
    app.chapter_table_tab(false);
    app.chapter_table_tab(false);
    assert_eq!(cell(&app), (1, ChapterColumn::Start));
    app.chapter_table_tab(false);
    assert_eq!(cell(&app), (0, ChapterColumn::Title), "Shift+Tab should wrap back to the previous row's last column");

    // Shift+Tab from the very first cell is a no-op.
    app.chapter_table_tab(false);
    app.chapter_table_tab(false);
    assert_eq!(cell(&app), (0, ChapterColumn::Start));
    app.chapter_table_tab(false);
    assert_eq!(cell(&app), (0, ChapterColumn::Start));
}

/// Connecting a chapter-kind source into a `ChapterEdit` node's input
/// should automatically import its chapters -- no separate action needed
/// -- and merge them alongside whatever the user already added manually,
/// leaving the manual entry untouched. Connecting a non-chapter (wrong
/// kind) source shouldn't import anything.
#[test]
fn connecting_a_chapter_source_auto_imports_and_merges_with_manual_chapters() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let modifier = app
        .graph
        .add_modifier(ModifierKind::ChapterEdit { chapters: vec![Chapter::new(0.0, 1.0, "Manual".to_string())] });

    // Wrong kind: connecting a video stream shouldn't import anything.
    let video_id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    app.armed = BTreeSet::from([Endpoint::Stream { node: video_id, stream_idx: 0 }]);
    focus_modifier(&mut app, modifier);
    app.toggle_connect();
    let chapters = chapter_edit_chapters(&app.graph, modifier).unwrap();
    assert_eq!(chapters.len(), 1, "{chapters:?}");
    assert!(!chapters[0].imported);

    // Right kind: connecting a chapter stream should auto-import, merging
    // with the untouched manual entry.
    let chapters_id = app.graph.add_input(
        "chapters.ffmeta".to_string(),
        Vec::new(),
        vec![Chapter::new(0.0, 2.0, "Imported".to_string())],
    );
    let chapter_idx = app.graph.input(chapters_id).unwrap().streams.len() - 1;
    app.armed = BTreeSet::from([Endpoint::Stream { node: chapters_id, stream_idx: chapter_idx }]);
    app.focus = Focus::Modifier(app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap());
    app.toggle_connect();

    let chapters = chapter_edit_chapters(&app.graph, modifier).unwrap();
    assert_eq!(chapters.len(), 2, "{chapters:?}");
    assert_eq!(chapters[0].title, "Manual");
    assert!(!chapters[0].imported);
    assert_eq!(chapters[1].title, "Imported");
    assert_eq!(chapters[1].end_secs, 2.0);
    assert!(chapters[1].imported);
}

/// Disconnecting a `ChapterEdit` node's input (via 'd' on the source
/// input's stream) should automatically remove exactly the chapters that
/// were auto-imported from that connection, leaving manually-added
/// entries -- even ones added after the import -- untouched.
#[test]
fn disconnecting_the_source_removes_only_auto_imported_chapters() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::ChapterEdit { chapters: Vec::new() });
    let chapters_id = app.graph.add_input(
        "chapters.ffmeta".to_string(),
        Vec::new(),
        vec![Chapter::new(0.0, 2.0, "Imported".to_string())],
    );
    let chapter_idx = app.graph.input(chapters_id).unwrap().streams.len() - 1;
    app.armed = BTreeSet::from([Endpoint::Stream { node: chapters_id, stream_idx: chapter_idx }]);
    app.focus = Focus::Modifier(app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap());
    app.toggle_connect();
    assert_eq!(chapter_edit_chapters(&app.graph, modifier).unwrap().len(), 1);

    // Add a manual chapter after the import, then disconnect the source.
    if let Some(chapters) = app.graph.modifier_mut(modifier).map(|m| match &mut m.kind {
        ModifierKind::ChapterEdit { chapters } => chapters,
        _ => unreachable!(),
    }) {
        chapters.push(Chapter::new(5.0, 6.0, "Manual".to_string()));
    }

    app.focus = Focus::Input(0);
    app.row_idx = chapter_idx;
    app.disconnect_focused();

    let chapters = chapter_edit_chapters(&app.graph, modifier).unwrap();
    assert_eq!(chapters.len(), 1, "{chapters:?}");
    assert_eq!(chapters[0].title, "Manual");
}

/// Reconnecting a `ChapterEdit` node's input to a *different* chapter
/// source (a modifier's input only ever holds one wire, so wiring a new
/// source in replaces the old one) should swap out the old auto-imported
/// set for the new one, without disturbing manually-added entries.
#[test]
fn reconnecting_to_a_different_source_replaces_the_auto_imported_set() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let modifier = app
        .graph
        .add_modifier(ModifierKind::ChapterEdit { chapters: vec![Chapter::new(0.0, 1.0, "Manual".to_string())] });
    let a_id =
        app.graph.add_input("a.ffmeta".to_string(), Vec::new(), vec![Chapter::new(0.0, 1.0, "FromA".to_string())]);
    let a_idx = app.graph.input(a_id).unwrap().streams.len() - 1;
    let modifier_focus_idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();

    app.armed = BTreeSet::from([Endpoint::Stream { node: a_id, stream_idx: a_idx }]);
    app.focus = Focus::Modifier(modifier_focus_idx);
    app.toggle_connect();
    let chapters = chapter_edit_chapters(&app.graph, modifier).unwrap();
    assert!(chapters.iter().any(|c| c.title == "FromA"), "{chapters:?}");

    let b_id =
        app.graph.add_input("b.ffmeta".to_string(), Vec::new(), vec![Chapter::new(0.0, 1.0, "FromB".to_string())]);
    let b_idx = app.graph.input(b_id).unwrap().streams.len() - 1;
    app.armed = BTreeSet::from([Endpoint::Stream { node: b_id, stream_idx: b_idx }]);
    app.focus = Focus::Modifier(modifier_focus_idx);
    app.toggle_connect();

    let chapters = chapter_edit_chapters(&app.graph, modifier).unwrap();
    assert!(!chapters.iter().any(|c| c.title == "FromA"), "old import should be gone: {chapters:?}");
    assert!(chapters.iter().any(|c| c.title == "FromB"), "{chapters:?}");
    assert!(chapters.iter().any(|c| c.title == "Manual"), "{chapters:?}");
}

/// Deleting the source input node entirely (not just disconnecting its
/// stream) should also trigger the same auto-import cleanup.
#[test]
fn deleting_the_source_input_node_also_removes_its_imported_chapters() {
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
    app.toggle_connect();
    assert_eq!(chapter_edit_chapters(&app.graph, modifier).unwrap().len(), 2);

    app.focus = Focus::Input(0);
    app.delete_focused_node();

    let chapters = chapter_edit_chapters(&app.graph, modifier).unwrap();
    assert_eq!(chapters.len(), 1, "{chapters:?}");
    assert_eq!(chapters[0].title, "Manual");
}

/// 'c' to connect an armed chapter-kind endpoint into a focused output
/// should land it on the chapters slot (`Target::OutputChapters`), not the
/// regular mapped-stream slot -- a plain video/audio endpoint should still
/// land on the regular slot as before. Regression guard for the
/// kind-based dispatch in `toggle_connect`'s `Focus::Output` arm.
#[test]
fn toggle_connect_routes_by_endpoint_kind() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let out = app.graph.outputs[0].id;

    let video_id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    app.armed = BTreeSet::from([Endpoint::Stream { node: video_id, stream_idx: 0 }]);
    app.focus = Focus::Output(0);
    app.toggle_connect();
    assert_eq!(app.graph.incoming(Target::Output(out)).len(), 1);
    assert_eq!(app.graph.incoming(Target::OutputChapters(out)).len(), 0);

    let chapters_id = app.graph.add_input(
        "chapters.ffmeta".to_string(),
        Vec::new(),
        vec![Chapter::new(0.0, 1.0, "A".to_string())],
    );
    let chapter_idx = app.graph.input(chapters_id).unwrap().streams.len() - 1;
    app.armed = BTreeSet::from([Endpoint::Stream { node: chapters_id, stream_idx: chapter_idx }]);
    app.toggle_connect();
    assert_eq!(app.graph.incoming(Target::OutputChapters(out)).len(), 1);
    assert_eq!(app.graph.incoming(Target::Output(out)).len(), 1, "the earlier video connection should be untouched");
}

/// An output's row list always has one more row than its mapped-stream
/// count, for its chapters slot. Disconnecting on that appended row should
/// only remove the chapters wire, leaving mapped streams untouched.
#[test]
fn disconnect_focused_on_the_appended_chapters_row_only_disconnects_chapters() {
    use crate::app::{App, Focus};

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let video_id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    app.graph.connect(Endpoint::Stream { node: video_id, stream_idx: 0 }, Target::Output(out));
    let chapters_id = app.graph.add_input(
        "chapters.ffmeta".to_string(),
        Vec::new(),
        vec![Chapter::new(0.0, 1.0, "A".to_string())],
    );
    let chapter_idx = app.graph.input(chapters_id).unwrap().streams.len() - 1;
    app.graph.connect(Endpoint::Stream { node: chapters_id, stream_idx: chapter_idx }, Target::OutputChapters(out));

    app.focus = Focus::Output(0);
    app.row_idx = 1; // the appended chapters row, right after the one mapped stream
    app.disconnect_focused();

    assert_eq!(app.graph.incoming(Target::OutputChapters(out)).len(), 0);
    assert_eq!(app.graph.incoming(Target::Output(out)).len(), 1, "the mapped stream should be untouched");
}

/// The "chapters" entry in the add-node picker should create a
/// `ChapterEdit` modifier node.
#[test]
fn add_node_picker_chapters_entry_creates_chapter_edit_modifier() {
    use crate::app::{App, Mode};

    let mut app = App::new();
    app.open_add_node_picker();
    let Mode::Picker { options, .. } = &app.mode else { panic!() };
    let row = options.iter().position(|o| o.value.as_deref() == Some("chapters")).unwrap();
    app.picker_move(row as isize);
    app.picker_confirm();

    assert_eq!(app.graph.modifiers.len(), 1);
    assert!(matches!(app.graph.modifiers[0].kind, ModifierKind::ChapterEdit { .. }));
}



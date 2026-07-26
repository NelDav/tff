use super::*;


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
    let Mode::TextInput { input, suggestions, selected, .. } = &mut app.mode else {
        panic!("expected text input mode");
    };
    *input = tui_input::Input::new(format!("{}/", dir.display()));
    *suggestions = crate::app::path_suggestions(input.value());
    *selected = suggestions.iter().position(|s| s.ends_with("subdir/")).expect("subdir should be offered");

    app.text_input_accept_suggestion();

    let Mode::TextInput { input, suggestions, selected, .. } = &app.mode else {
        panic!("expected text input mode");
    };
    assert_eq!(input.value(), format!("{}/subdir/", dir.display()));
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
    let Mode::TextInput { input, suggestions, .. } = &mut app.mode else {
        panic!("expected text input mode");
    };
    *input = tui_input::Input::new(format!("{}/", dir.display()));
    *suggestions = crate::app::path_suggestions(input.value());
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
    let Mode::TextInput { input, .. } = &mut app.mode else { panic!("expected text input mode") };
    *input = tui_input::Input::new(format!("{}/", dir.display())); // Input::new already puts the cursor at the end

    for c in "al".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    let Mode::TextInput { input, suggestions, selected, .. } = &app.mode else {
        panic!("expected text input mode");
    };
    assert_eq!(input.value(), format!("{}/al", dir.display()));
    assert_eq!(suggestions.len(), 2, "should narrow to the two 'al*' entries: {suggestions:?}");
    assert_eq!(*selected, 0);

    // One backspace narrows "al" to "a" -- still just the two "al*" entries.
    app.text_input_handle_key(key(KeyCode::Backspace));
    let Mode::TextInput { suggestions, .. } = &app.mode else { panic!("expected text input mode") };
    assert_eq!(suggestions.len(), 2, "'a' should still match only the two 'al*' entries: {suggestions:?}");

    // A second backspace clears the prefix entirely, widening to everything.
    app.text_input_handle_key(key(KeyCode::Backspace));
    let Mode::TextInput { suggestions, .. } = &app.mode else { panic!("expected text input mode") };
    assert_eq!(suggestions.len(), 4, "an empty prefix should widen the match to all four entries: {suggestions:?}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// A freshly-opened text field's cursor should start at the end of
/// whatever it's prefilled with -- typing should append, same as before
/// cursor movement existed, unless the user explicitly moves it.
#[test]
fn text_input_mode_starts_with_cursor_at_the_end_of_the_buffer() {
    use crate::app::{App, Mode};

    let mut app = App::new();
    app.graph.outputs[0].path = "out.mkv".to_string();
    app.start_edit_output();
    let Mode::TextInput { input, .. } = &app.mode else { panic!("expected text input mode") };
    let buffer = input.value();
    let cursor = input.cursor();
    assert_eq!(buffer, "out.mkv");
    assert_eq!(cursor, 7);
}

/// Left/Right should move the cursor within the buffer, clamped to its
/// bounds -- moving past either end should just stop there, not wrap or
/// panic.
#[test]
fn text_input_move_cursor_clamps_to_buffer_bounds() {
    use crate::app::{App, Mode};

    let mut app = App::new();
    app.start_add_input();
    for c in "abc".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    let cursor = input.cursor();
    assert_eq!(cursor, 3);

    app.text_input_handle_key(key(KeyCode::Left));
    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    let cursor = input.cursor();
    assert_eq!(cursor, 2);

    for _ in 0..10 {
        app.text_input_handle_key(key(KeyCode::Left));
    }
    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    let cursor = input.cursor();
    assert_eq!(cursor, 0, "moving past the start should clamp, not go negative");

    for _ in 0..10 {
        app.text_input_handle_key(key(KeyCode::Right));
    }
    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    let cursor = input.cursor();
    assert_eq!(cursor, 3, "moving past the end should clamp to the buffer's length");
}

/// Home/End should jump the cursor straight to the start/end of the
/// buffer, same as a normal text field -- backed by `tui_input`'s
/// `GoToStart`/`GoToEnd` requests.
#[test]
fn text_input_home_and_end_jump_to_the_buffer_bounds() {
    use crate::app::{App, Mode};

    let mut app = App::new();
    app.start_add_input();
    for c in "hello".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    assert_eq!(input.cursor(), 5, "cursor should already be at the end after typing");

    app.text_input_handle_key(key(KeyCode::Home));
    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    assert_eq!(input.cursor(), 0);

    app.text_input_handle_key(key(KeyCode::End));
    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    assert_eq!(input.cursor(), 5);

    // Home then typing should insert at the very start, not append.
    app.text_input_handle_key(key(KeyCode::Home));
    app.text_input_handle_key(key(KeyCode::Char('!')));
    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    assert_eq!(input.value(), "!hello");
    assert_eq!(input.cursor(), 1);
}

/// Typing with the cursor positioned mid-buffer should insert right there
/// -- editing a string shouldn't require erasing everything after the
/// insertion point first.
#[test]
fn text_input_char_inserts_at_the_cursor_not_always_at_the_end() {
    use crate::app::{App, Mode};

    let mut app = App::new();
    app.start_add_input();
    for c in "ac".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    app.text_input_handle_key(key(KeyCode::Left)); // between 'a' and 'c'
    app.text_input_handle_key(key(KeyCode::Char('b')));

    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    let buffer = input.value();
    let cursor = input.cursor();
    assert_eq!(buffer, "abc");
    assert_eq!(cursor, 2, "cursor should land right after the inserted character");
}

/// Backspace with the cursor mid-buffer should remove the character just
/// before it, not the buffer's last character.
#[test]
fn text_input_backspace_removes_the_character_before_the_cursor() {
    use crate::app::{App, Mode};

    let mut app = App::new();
    app.start_add_input();
    for c in "abc".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    app.text_input_handle_key(key(KeyCode::Left)); // between 'b' and 'c'
    app.text_input_handle_key(key(KeyCode::Backspace));

    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    let buffer = input.value();
    let cursor = input.cursor();
    assert_eq!(buffer, "ac");
    assert_eq!(cursor, 1);
}

/// Backspace at the very start of the buffer (cursor at 0) should be a
/// no-op rather than panicking or removing from the end.
#[test]
fn text_input_backspace_at_start_of_buffer_is_a_no_op() {
    use crate::app::{App, Mode};

    let mut app = App::new();
    app.start_add_input();
    for c in "ab".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    for _ in 0..10 { // clamp to 0
        app.text_input_handle_key(key(KeyCode::Left));
    }
    app.text_input_handle_key(key(KeyCode::Backspace));

    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    let buffer = input.value();
    let cursor = input.cursor();
    assert_eq!(buffer, "ab", "nothing before the cursor to remove");
    assert_eq!(cursor, 0);
}

/// The Delete key removes the character right at the cursor (the mirror
/// image of Backspace), leaving the cursor itself in place.
#[test]
fn text_input_delete_removes_the_character_at_the_cursor() {
    use crate::app::{App, Mode};

    let mut app = App::new();
    app.start_add_input();
    for c in "abc".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    for _ in 0..2 { // between 'a' and 'b'
        app.text_input_handle_key(key(KeyCode::Left));
    }
    app.text_input_handle_key(key(KeyCode::Delete)); // removes 'b'

    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    let buffer = input.value();
    let cursor = input.cursor();
    assert_eq!(buffer, "ac");
    assert_eq!(cursor, 1, "the cursor shouldn't move -- only what's ahead of it is removed");
}

/// Delete at the very end of the buffer (nothing to its right) should be
/// a no-op rather than panicking or removing from the start.
#[test]
fn text_input_delete_at_end_of_buffer_is_a_no_op() {
    use crate::app::{App, Mode};

    let mut app = App::new();
    app.start_add_input();
    for c in "ab".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    // Cursor is already at the end after typing.
    app.text_input_handle_key(key(KeyCode::Delete));

    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    let buffer = input.value();
    let cursor = input.cursor();
    assert_eq!(buffer, "ab", "nothing after the cursor to remove");
    assert_eq!(cursor, 2);
}

/// Delete should handle a multi-byte UTF-8 character right at the cursor
/// without panicking or corrupting the buffer, same concern as Backspace.
#[test]
fn text_input_delete_handles_multi_byte_utf8() {
    use crate::app::{App, Mode};

    let mut app = App::new();
    app.start_add_input();
    for c in "café".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    app.text_input_handle_key(key(KeyCode::Left)); // right before 'é'
    app.text_input_handle_key(key(KeyCode::Delete));

    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    let buffer = input.value();
    assert_eq!(buffer, "caf");
}

/// Multi-byte UTF-8 characters (e.g. in a chapter title) shouldn't panic
/// or corrupt the buffer when inserting/deleting at a mid-string cursor
/// position -- `cursor` is a char index, but `String::insert`/`remove`
/// need a byte offset, so this exercises that conversion directly.
#[test]
fn text_input_char_and_backspace_handle_multi_byte_utf8() {
    use crate::app::{App, Mode};

    let mut app = App::new();
    app.start_add_input();
    for c in "café".chars() {
        app.text_input_handle_key(key(KeyCode::Char(c)));
    }
    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    let buffer = input.value();
    let cursor = input.cursor();
    assert_eq!(buffer, "café");
    assert_eq!(cursor, 4, "cursor counts chars, not bytes, even though é is multi-byte");

    app.text_input_handle_key(key(KeyCode::Left)); // between 'f' and 'é'
    app.text_input_handle_key(key(KeyCode::Char('!')));
    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    let buffer = input.value();
    assert_eq!(buffer, "caf!é");

    app.text_input_handle_key(key(KeyCode::Backspace)); // removes the '!' just inserted
    app.text_input_handle_key(key(KeyCode::Backspace)); // removes 'f'
    let Mode::TextInput { input, .. } = &app.mode else { panic!() };
    let buffer = input.value();
    assert_eq!(buffer, "caé");
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

/// The chapter table should render as an actual table -- a header row
/// plus one row per chapter showing its start/end/title -- with a
/// trailing "add chapter" row underneath.
#[test]
fn ui_renders_chapter_table_with_header_and_add_row() {
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new();
    let modifier = app.graph.add_modifier(ModifierKind::ChapterEdit {
        chapters: vec![Chapter::new(0.0, 65.0, "Intro".to_string())],
    });
    let idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = crate::app::Focus::Modifier(idx);
    app.activate_focused();

    let backend = TestBackend::new(140, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buf = terminal.backend().buffer();
    let screen: String = (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(screen.contains(" chapters "), "expected the popup title:\n{screen}");
    assert!(screen.contains("start") && screen.contains("end") && screen.contains("title"), "expected a header row:\n{screen}");
    assert!(screen.contains("00:00:00"), "expected the chapter's start time:\n{screen}");
    assert!(screen.contains("00:01:05"), "expected the chapter's end time:\n{screen}");
    assert!(screen.contains("Intro"), "expected the chapter's title:\n{screen}");
    assert!(screen.contains("add chapter"), "expected the trailing add-chapter row:\n{screen}");
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
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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

/// Verifies the row-offset math that places wire endpoints on a Metadata
/// node: its field section grows with however many fields are set, which
/// pushes the incoming/outgoing connection rows (and thus where wires
/// must attach) down by that many rows.
#[test]
fn metadata_node_wires_attach_below_its_field_section_not_at_a_fixed_row() {
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let id = app.graph.add_input("video_a.mp4".to_string(), video_stream(), Vec::new());
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
    let content_start = incoming_line.find("← v:0 h264").unwrap();
    let border_pos =
        incoming_line[..content_start].rfind('│').expect("the modifier's left border precedes the incoming row");
    let before_border = incoming_line[..border_pos].chars().next_back();
    assert!(
        before_border.is_some_and(is_wire_glyph),
        "wire from the input should terminate on the incoming row itself, not drift onto a field row:\n{incoming_line}"
    );

    let outgoing_line =
        screen.lines().find(|l| l.contains("→ OUTPUT 1")).expect("outgoing connection row present");
    let content_start = outgoing_line.find("→ OUTPUT 1").unwrap();
    let border_pos = content_start
        + outgoing_line[content_start..].find('│').expect("the modifier's right border follows the outgoing row");
    let after_border = outgoing_line[border_pos + '│'.len_utf8()..].chars().next();
    assert!(
        after_border.is_some_and(is_wire_glyph),
        "wire to the output should leave from the outgoing row itself, not drift onto another row:\n{outgoing_line}"
    );
}

use super::*;


/// A set extra_args should show up as a title tag on both input and output
/// nodes -- the only visible indicator that one of these is configured,
/// since (unlike Metadata/Filter nodes) input/output boxes have no
/// dedicated upper section to list it in.
#[test]
fn ui_renders_extra_args_in_upper_section_not_the_title() {
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = crate::app::App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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
    // The title itself stays plain and un-tagged -- extra_args only ever
    // shows in the upper section.
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
    let id = app.graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
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
    let content_end = src_line.find("h264").unwrap() + "h264".len();
    let border_pos = content_end + src_line[content_end..].find('│').expect("the node's right border follows the stream row");
    let after_border = src_line[border_pos + '│'.len_utf8()..].chars().next();
    assert!(
        after_border.is_some_and(is_wire_glyph),
        "wire from the input should leave from the stream row itself, not drift onto another row:\n{src_line}"
    );

    let dst_line = screen.lines().find(|l| l.contains("v:0 h264") && l.contains("<-")).expect("mapped row present");
    let content_start = dst_line.find("v:0 h264").unwrap();
    let border_pos = dst_line[..content_start].rfind('│').expect("the node's left border precedes the mapped row");
    let before_border = dst_line[..border_pos].chars().next_back();
    assert!(
        before_border.is_some_and(is_wire_glyph),
        "wire into the output should land on the mapped row itself, not drift onto another row:\n{dst_line}"
    );
}

/// In a dense layout, one wire's straight run can pass directly through
/// the cell where a different wire turns a corner. Positions two inputs
/// and an output precisely enough that this actually happens, and checks
/// the shared cell renders as a real junction glyph (both lines visibly
/// meet there) rather than one wire's glyph silently overwriting the
/// other's -- see `draw_wire`'s box-drawing merge logic in `src/ui.rs`.
#[test]
fn crossing_wires_render_as_a_real_junction_not_a_silent_overwrite() {
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let a = app.graph.add_input("a.mp4".to_string(), video_stream(), Vec::new());
    let b = app.graph.add_input("b.mp4".to_string(), video_stream(), Vec::new());

    // A's wire runs straight across row 1 (its stream row); B sits further
    // right but one row lower, so B's wire bends -- and its corner lands
    // in the middle of A's straight run, at the same buffer cell.
    app.graph.input_mut(a).unwrap().pos = (0.0, 0.0);
    app.graph.input_mut(a).unwrap().width = 16;
    app.graph.input_mut(b).unwrap().pos = (24.0, 0.0);
    app.graph.input_mut(b).unwrap().width = 16;
    app.graph.outputs[0].pos = (70.0, 0.0);
    app.graph.outputs[0].width = 20;

    app.graph.connect(Endpoint::Stream { node: a, stream_idx: 0 }, Target::Output(out)); // row 0 -- straight, row 1
    app.graph.connect(Endpoint::Stream { node: b, stream_idx: 0 }, Target::Output(out)); // row 1 -- bends through row 1

    let backend = TestBackend::new(160, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buf = terminal.backend().buffer();
    let screen: String = (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    let lines: Vec<&str> = screen.lines().collect();
    let a_title = lines.iter().position(|l| l.contains("[0] a.mp4")).expect("input a's title row present");
    let a_row = lines[a_title + 1];
    assert!(
        a_row.contains(['┬', '┴', '├', '┤', '┼']),
        "the cell where b's wire crosses a's straight run should show a real junction, not overwrite it:\n{a_row}"
    );
}

/// An output should always show a chapters row -- "chapters (not
/// connected)" when nothing's wired in, or a description of whatever is --
/// as the last row after its mapped streams, and wire attachment math
/// should still land correctly for both rows when extra_args, a mapped
/// stream, and a connected chapter source are all present together.
#[test]
fn ui_renders_chapters_row_and_wires_still_attach_correctly() {
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let id = app.graph.add_input("in.mp4".to_string(), video_audio_streams(), Vec::new());
    let chapters_id = app.graph.add_input(
        "chapters.ffmeta".to_string(),
        Vec::new(),
        vec![Chapter::new(0.0, 5.0, "Intro".to_string())],
    );
    let chapter_idx = app.graph.input(chapters_id).unwrap().streams.len() - 1;
    app.graph.outputs[0].extra_args = filter_fields(&[("max_interleave_delta", "5000000")]);
    app.graph.outputs[0].width = 60; // wide enough that the extra_args line isn't truncated
    app.graph.connect(Endpoint::Stream { node: id, stream_idx: 0 }, Target::Output(out));
    app.graph.connect(Endpoint::Stream { node: chapters_id, stream_idx: chapter_idx }, Target::OutputChapters(out));

    let backend = TestBackend::new(160, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buf = terminal.backend().buffer();
    let screen: String = (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(screen.contains("-max_interleave_delta 5000000"), "expected the output's extra_args:\n{screen}");
    assert!(screen.contains("chapters <-"), "expected a chapters row describing the connected source:\n{screen}");

    let dst_line = screen.lines().find(|l| l.contains("v:0 h264") && l.contains("<-")).expect("mapped row present");
    assert!(
        dst_line.contains("─│v:0"),
        "wire into the output should land on the mapped row itself, not drift because of the chapters row:\n{dst_line}"
    );

    let chapters_line = screen.lines().find(|l| l.contains("chapters <-")).expect("chapters row present");
    assert!(
        chapters_line.contains("─│chapters"),
        "wire into the output's chapters slot should land on the chapters row itself:\n{chapters_line}"
    );
}

/// Regression test: when an output has *only* a chapter stream connected
/// (no mapped video/audio at all), the mapped section still occupies one
/// visual row -- the "(nothing mapped)" placeholder -- so the chapters
/// row sits right below *that*, not at the very top of the box. The wire
/// into the chapters slot has to land on the same row, not one row above
/// it.
#[test]
fn ui_wire_lands_on_chapters_row_when_output_has_no_mapped_streams() {
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new();
    let out = app.graph.outputs[0].id;
    let chapters_id = app.graph.add_input(
        "chapters.ffmeta".to_string(),
        Vec::new(),
        vec![Chapter::new(0.0, 5.0, "Intro".to_string())],
    );
    let chapter_idx = app.graph.input(chapters_id).unwrap().streams.len() - 1;
    app.graph.connect(Endpoint::Stream { node: chapters_id, stream_idx: chapter_idx }, Target::OutputChapters(out));

    let backend = TestBackend::new(160, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buf = terminal.backend().buffer();
    let screen: String = (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    let chapters_line = screen.lines().find(|l| l.contains("chapters <-")).expect("chapters row present");
    assert!(
        chapters_line.contains("─│chapters"),
        "the wire should land on the chapters row itself, not the placeholder row above it:\n{chapters_line}"
    );
    // The placeholder row is on the same terminal line as the *source*
    // input box's own outgoing-wire mark (an unrelated departure, not an
    // arrival) -- so check specifically for the wire arriving right at the
    // output box's own left border, immediately before its text, rather
    // than searching the whole line.
    let placeholder_line = screen.lines().find(|l| l.contains("(nothing mapped")).expect("placeholder row present");
    let before_placeholder = &placeholder_line[..placeholder_line.find("(nothing mapped").unwrap()];
    assert!(
        !before_placeholder.ends_with("─│"),
        "the wire shouldn't land on the placeholder row:\n{placeholder_line}"
    );
}

/// With nothing wired into it, an output's chapters row should be absent
/// entirely -- same as an output doesn't get a placeholder row for an
/// unmapped video/audio stream; a chapter stream isn't special.
#[test]
fn ui_omits_the_chapters_row_entirely_when_unconnected() {
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let app = App::new();
    let backend = TestBackend::new(160, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buf = terminal.backend().buffer();
    let screen: String = (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(!screen.contains("chapters"), "{screen}");
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
    let id = app.graph.add_input("video_a.mp4".to_string(), video_stream(), Vec::new());
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

/// A stream port's marker should distinguish all three states: idle "○",
/// selected-but-not-armed "●", and armed "◎" -- each stream in this test
/// is put into a different one of the three so they can all be checked in
/// the same render.
#[test]
fn ui_renders_distinct_markers_for_idle_selected_and_armed_ports() {
    use crate::app::{App, Focus};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), three_streams(), Vec::new());
    app.focus = Focus::Input(0);
    app.row_idx = 1;
    app.toggle_port_selection(); // select stream 1, leave 0 idle and 2 untouched
    app.armed.insert(Endpoint::Stream { node: id, stream_idx: 2 }); // arm stream 2 directly

    let backend = TestBackend::new(160, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buffer = terminal.backend().buffer();
    let screen: String = (0..buffer.area.height)
        .map(|y| (0..buffer.area.width).map(|x| buffer[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");

    let idle_line = screen.lines().find(|l| l.contains("v:0 h264")).expect("idle row present");
    assert!(idle_line.contains('○'), "idle port should show the empty marker:\n{idle_line}");

    let selected_line = screen.lines().find(|l| l.contains("a:1 aac")).expect("selected row present");
    assert!(selected_line.contains('●'), "selected-but-unarmed port should show the filled marker:\n{selected_line}");

    let armed_line = screen.lines().find(|l| l.contains("s:2 srt")).expect("armed row present");
    assert!(armed_line.contains('◎'), "armed port should show the armed marker:\n{armed_line}");
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
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
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
    let v = app.graph.add_input("v.mp4".to_string(), mk(StreamKind::Video), Vec::new());
    let a = app.graph.add_input("a.m4a".to_string(), mk(StreamKind::Audio), Vec::new());
    let s = app.graph.add_input("s.srt".to_string(), mk(StreamKind::Subtitle), Vec::new());
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
    let Mode::TextInput { input, suggestions, selected, .. } = &mut app.mode else {
        panic!("expected text input mode");
    };
    *input = tui_input::Input::new(format!("{}/a", dir.display()));
    *suggestions = crate::app::path_suggestions(input.value());
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

/// A suggestion's display label is just its own trailing name -- the
/// shared directory prefix every candidate in one listing has in common is
/// dropped, keeping a directory's own trailing '/' marker.
#[test]
fn suggestion_label_strips_the_shared_directory_prefix() {
    assert_eq!(crate::ui::suggestion_label("/some/long/dir/alpha.mp4"), "alpha.mp4");
    assert_eq!(crate::ui::suggestion_label("/some/long/dir/subdir/"), "subdir/");
    assert_eq!(crate::ui::suggestion_label("alpha.mp4"), "alpha.mp4");
}

/// Regression test for a real bug: the popup used to show each suggestion's
/// *full* path, which is harmless for a short directory but silently pushes
/// the actual file name off the edge of the (deliberately capped-width)
/// popup once the directory itself is long enough -- exactly what happened
/// on a CI runner whose temp directory is much longer than this dev
/// machine's `/tmp`. Uses a directory nested deep enough to reproduce that
/// regardless of platform, rather than relying on whatever temp path this
/// machine happens to have.
#[test]
fn ui_suggestions_popup_shows_the_file_name_even_under_a_long_directory_path() {
    use crate::app::{App, Mode};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let dir = std::env::temp_dir().join(
        "tff-test-long-path-so-so-so-so-so-so-so-so-so-so-so-so-so-so-so-so-so-so-so-so-very-deep",
    );
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("alpha.mp4"), b"").unwrap();

    let mut app = App::new();
    app.start_add_input();
    let Mode::TextInput { input, suggestions, .. } = &mut app.mode else {
        panic!("expected text input mode");
    };
    *input = tui_input::Input::new(format!("{}/a", dir.display()));
    *suggestions = crate::app::path_suggestions(input.value());

    let mut terminal = Terminal::new(TestBackend::new(140, 40)).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buf = terminal.backend().buffer();
    let screen: String = (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(screen.contains("alpha.mp4"), "expected the file name to stay visible:\n{screen}");

    let _ = std::fs::remove_dir_all(&dir);
}


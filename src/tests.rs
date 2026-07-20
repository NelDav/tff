use std::process::Command;
use std::sync::mpsc;

use crate::ffmpeg;
use crate::graph::{Codec, Graph, StreamInfo, StreamKind};

/// build_ffmpeg_args should keep the blanket `-c copy` default and add a
/// per-output-stream override only for the edge that isn't Copy, addressed
/// by its position in the edge list (which is also the -map/output order).
#[test]
fn ffmpeg_args_add_per_stream_codec_override() {
    let mut graph = Graph::new();
    let id = graph.add_input(
        "in.mp4".to_string(),
        vec![
            StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None },
            StreamInfo { index: 1, kind: StreamKind::Audio, codec: "aac".to_string(), lang: None },
        ],
    );
    graph.toggle_edge(id, 0); // video: stays Copy
    graph.toggle_edge(id, 1); // audio: re-encoded
    graph.set_edge_codec(id, 1, Codec::Encode("flac".to_string()));

    let args = graph.build_ffmpeg_args();
    let joined = args.join(" ");

    assert!(joined.contains("-c copy"), "expected a blanket -c copy default: {joined}");
    assert!(joined.contains("-c:1 flac"), "expected -c:1 flac override: {joined}");
    assert!(!joined.contains("-c:0"), "video edge is Copy, should get no override: {joined}");
}

/// Opening the codec picker for a connected port should list "copy" first,
/// then only encoders matching that stream's kind (never e.g. an audio
/// encoder for a video port), with the currently-set codec pre-selected.
#[test]
fn codec_picker_offers_only_matching_kind_with_current_preselected() {
    use crate::app::{App, Focus, Mode, PickerKind};

    let mut app = App::new();
    let id = app.graph.add_input(
        "in.mp4".to_string(),
        vec![StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None }],
    );
    app.graph.toggle_edge(id, 0);
    app.graph.set_edge_codec(id, 0, Codec::Encode("libx265".to_string()));
    app.focus = Focus::Input(0);
    app.port_idx = 0;

    app.open_codec_picker();

    let Mode::Picker { kind, options, selected, .. } = &app.mode else {
        panic!("expected picker mode");
    };
    assert!(matches!(kind, PickerKind::Codec { .. }));
    assert_eq!(options[0].display, "copy (no re-encode)");
    assert!(options.iter().any(|o| o.value.as_deref() == Some("libx264")));
    assert!(
        !options.iter().any(|o| o.value.as_deref() == Some("aac")),
        "video port should not offer an audio encoder"
    );
    assert_eq!(
        options[*selected].value.as_deref(),
        Some("libx265"),
        "current codec should be preselected"
    );
}

/// 'e' should refuse to open a picker for an unconnected port -- codec only
/// matters for streams actually being muxed into the output.
#[test]
fn codec_picker_refuses_on_unconnected_port() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    app.graph.add_input(
        "in.mp4".to_string(),
        vec![StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None }],
    );
    app.focus = Focus::Input(0);
    app.port_idx = 0;

    app.open_codec_picker();

    assert!(matches!(app.mode, Mode::Normal), "should not open a picker for an unconnected port");
    assert!(app.log.last().unwrap().contains("connect this stream first"));
}

/// Confirming a container choice should set an explicit -f override and,
/// for a recognized common container, rewrite the output path's extension
/// for convenience -- and that override should actually reach ffmpeg's args.
#[test]
fn container_picker_confirm_sets_override_and_rewrites_known_extension() {
    use crate::app::{App, Mode};

    let mut app = App::new();
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

    assert_eq!(app.graph.output.container.as_deref(), Some("webm"));
    assert_eq!(app.graph.output.path, "output.webm");
    assert!(app.graph.build_ffmpeg_args().windows(2).any(|w| w == ["-f", "webm"]));
}

/// Esc (with no active filter) should discard the picker without applying
/// any selection.
#[test]
fn picker_escape_with_no_filter_closes_picker_unchanged() {
    use crate::app::{App, Focus, Mode};

    let mut app = App::new();
    let id = app.graph.add_input(
        "in.mp4".to_string(),
        vec![StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None }],
    );
    app.graph.toggle_edge(id, 0);
    app.focus = Focus::Input(0);
    app.port_idx = 0;

    app.open_codec_picker();
    app.picker_move(3);
    app.picker_escape();

    assert!(matches!(app.mode, Mode::Normal));
    assert_eq!(app.graph.edges[0].codec, Codec::Copy, "cancel should not have applied any selection");
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

/// Typing a query with '/' should live-filter the list and reset the
/// selection; Enter should stop typing but keep the filter applied so
/// arrow keys navigate the now-shorter list.
#[test]
fn picker_search_filters_list_and_confirm_keeps_filter_for_navigation() {
    use crate::app::{filtered_indices, App, Focus, Mode};

    let mut app = App::new();
    let id = app.graph.add_input(
        "in.mp4".to_string(),
        vec![StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None }],
    );
    app.graph.toggle_edge(id, 0);
    app.focus = Focus::Input(0);
    app.port_idx = 0;
    app.open_codec_picker();

    app.picker_start_search();
    for c in "265".chars() {
        app.picker_search_char(c);
    }

    let Mode::Picker { query, options, searching, .. } = &app.mode else {
        panic!("expected picker mode");
    };
    assert!(*searching);
    assert_eq!(query, "265");
    let visible = filtered_indices(options, query);
    assert_eq!(visible.len(), 1);
    assert_eq!(options[visible[0]].value.as_deref(), Some("libx265"));

    app.picker_search_backspace();
    let Mode::Picker { query, .. } = &app.mode else { panic!("expected picker mode") };
    assert_eq!(query, "26", "backspace should drop the last character");

    app.picker_search_char('5');
    app.picker_confirm_search();
    let Mode::Picker { searching, query, .. } = &app.mode else {
        panic!("expected picker mode");
    };
    assert!(!searching, "Enter should stop typing");
    assert_eq!(query, "265", "but keep the filter applied");

    app.picker_confirm();
    assert_eq!(
        app.graph.edges[0].codec,
        Codec::Encode("libx265".to_string()),
        "selecting the only filtered match should apply it"
    );
}

/// Esc semantics mirror vim: while typing, cancel the query outright.
/// Once not typing, a first Esc clears an active filter (picker stays
/// open); only a second Esc (with no filter left) closes the picker.
#[test]
fn picker_escape_clears_filter_before_closing() {
    use crate::app::{App, Mode};

    let mut app = App::new();
    app.open_container_picker();

    app.picker_start_search();
    app.picker_search_char('w');
    app.picker_search_char('e');
    app.picker_escape();
    assert!(matches!(app.mode, Mode::Picker { .. }), "Esc while typing cancels the query, not the picker");
    let Mode::Picker { query, searching, .. } = &app.mode else { unreachable!() };
    assert!(query.is_empty());
    assert!(!searching);

    // Re-apply a filter without the "currently typing" flag, then confirm
    // search first so we're purely in list-nav mode with a filter active.
    app.picker_start_search();
    app.picker_search_char('m');
    app.picker_confirm_search();
    app.picker_escape();
    assert!(matches!(app.mode, Mode::Picker { .. }), "first Esc should clear the filter, not close the picker");
    let Mode::Picker { query, .. } = &app.mode else { unreachable!() };
    assert!(query.is_empty(), "filter should be cleared");

    app.picker_escape();
    assert!(matches!(app.mode, Mode::Normal), "second Esc with no filter left should close the picker");
}

/// The UI should render the codec choice both as a small badge sitting
/// directly on the connection wire and as a tag on the output node's
/// mapped-stream line -- the two places a "converter between connections"
/// should be visible.
#[test]
fn ui_shows_codec_badge_on_wire_and_output_line() {
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new();
    let id = app.graph.add_input(
        "video_a.mp4".to_string(),
        vec![StreamInfo { index: 0, kind: StreamKind::Video, codec: "h264".to_string(), lang: None }],
    );
    app.graph.toggle_edge(id, 0);
    app.graph.set_edge_codec(id, 0, Codec::Encode("libx265".to_string()));

    let backend = TestBackend::new(140, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buffer = terminal.backend().buffer();

    let mut rows = Vec::new();
    for y in 0..buffer.area.height {
        let mut row = String::new();
        for x in 0..buffer.area.width {
            row.push_str(buffer[(x, y)].symbol());
        }
        rows.push(row);
    }
    let screen = rows.join("\n");

    assert!(screen.contains("→ x265"), "expected the input port line to show the target codec:\n{screen}");
    assert!(
        screen.contains("─x265─") || screen.contains("x265"),
        "expected a codec badge on the connector wire:\n{screen}"
    );
    assert!(
        screen.contains("[x265]"),
        "expected the output node's mapped line to show the codec tag:\n{screen}"
    );
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
    let id = app.graph.add_input(
        "in.mp4".to_string(),
        vec![StreamInfo {
            index: 0,
            kind: StreamKind::Video,
            codec: "h264".to_string(),
            lang: None,
        }],
    );
    app.graph.toggle_edge(id, 0);

    let backend = TestBackend::new(140, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|frame| crate::ui::draw(frame, &app)).unwrap();
    let buffer = terminal.backend().buffer().clone();

    // Mirrors the vertical layout in ui::draw() so we can scope the check to
    // the graph panel's genuine interior rows -- the panel's own top/bottom
    // border legitimately draws '─' across every column, which would
    // otherwise look identical to a stray wire and false-positive the check.
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

    // Column just inside the graph panel's left border: with the bug, the
    // connector line's destination x was hardcoded to 0, so it always
    // crossed this column on its way to the (wrong) origin.
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

/// Connector wires should be colored by the kind of stream they carry, so a
/// video edge, an audio edge, and a subtitle edge are visually distinct.
#[test]
fn wires_are_colored_by_stream_kind() {
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::style::Color;
    use ratatui::Terminal;

    let mut app = App::new();
    let mk = |kind: StreamKind, codec: &str| {
        vec![StreamInfo {
            index: 0,
            kind,
            codec: codec.to_string(),
            lang: None,
        }]
    };
    let v = app.graph.add_input("v.mp4".to_string(), mk(StreamKind::Video, "h264"));
    let a = app.graph.add_input("a.m4a".to_string(), mk(StreamKind::Audio, "aac"));
    let s = app.graph.add_input("s.srt".to_string(), mk(StreamKind::Subtitle, "subrip"));
    app.graph.toggle_edge(v, 0);
    app.graph.toggle_edge(a, 0);
    app.graph.toggle_edge(s, 0);

    // Tall enough that all three inputs (spaced 12 rows apart) stay within
    // the graph panel instead of being clipped by node_rect's bounds check.
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

/// End-to-end check of the core feature: pull one track each from three
/// different input files and mux them into a single output file.
#[test]
fn combines_video_audio_and_subtitle_from_three_files() {
    let dir = std::env::temp_dir().join(format!("tff-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let video_path = dir.join("v_only.mp4");
    let audio_path = dir.join("a_only.m4a");
    let sub_path = dir.join("sub.srt");
    let out_path = dir.join("combined.mkv");

    std::fs::write(
        &sub_path,
        "1\n00:00:00,000 --> 00:00:01,000\nhello from tff\n",
    )
    .unwrap();

    run_ok(Command::new("ffmpeg").args([
        "-y",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "testsrc=duration=1:size=160x120:rate=5",
        "-c:v",
        "libx264",
        "-an",
        video_path.to_str().unwrap(),
    ]));
    run_ok(Command::new("ffmpeg").args([
        "-y",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=1",
        "-c:a",
        "aac",
        audio_path.to_str().unwrap(),
    ]));

    let mut graph = Graph::new();
    let id_v = graph.add_input(
        video_path.to_str().unwrap().to_string(),
        ffmpeg::probe(video_path.to_str().unwrap()).unwrap(),
    );
    let id_a = graph.add_input(
        audio_path.to_str().unwrap().to_string(),
        ffmpeg::probe(audio_path.to_str().unwrap()).unwrap(),
    );
    let id_s = graph.add_input(
        sub_path.to_str().unwrap().to_string(),
        ffmpeg::probe(sub_path.to_str().unwrap()).unwrap(),
    );

    // Exactly what the TUI does when the user arms a port and connects it.
    graph.toggle_edge(id_v, 0);
    graph.toggle_edge(id_a, 0);
    graph.toggle_edge(id_s, 0);
    graph.output.path = out_path.to_str().unwrap().to_string();

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
    assert_eq!(done_code.as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let out_streams = ffmpeg::probe(out_path.to_str().unwrap()).unwrap();
    assert_eq!(out_streams.len(), 3, "expected exactly 3 muxed streams");
    assert!(out_streams.iter().any(|s| s.kind == StreamKind::Video));
    assert!(out_streams.iter().any(|s| s.kind == StreamKind::Audio));
    assert!(out_streams.iter().any(|s| s.kind == StreamKind::Subtitle));

    let _ = std::fs::remove_dir_all(&dir);
}

/// End-to-end check of the codec-conversion feature: a connection with a
/// non-Copy codec should actually transcode, not just copy through. Encodes
/// an AAC source track to FLAC and verifies the *output* file's codec
/// changed, which only happens if -c:<i> actually reached ffmpeg.
#[test]
fn reencodes_a_connected_stream_to_a_different_codec() {
    let dir = std::env::temp_dir().join(format!("tff-test-reencode-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();

    let audio_path = dir.join("a_only.m4a");
    let out_path = dir.join("recoded.mkv");

    run_ok(Command::new("ffmpeg").args([
        "-y",
        "-loglevel",
        "error",
        "-f",
        "lavfi",
        "-i",
        "sine=frequency=440:duration=1",
        "-c:a",
        "aac",
        audio_path.to_str().unwrap(),
    ]));

    let mut graph = Graph::new();
    let source_streams = ffmpeg::probe(audio_path.to_str().unwrap()).unwrap();
    assert_eq!(source_streams[0].codec, "aac", "test fixture should start as aac");
    let id = graph.add_input(audio_path.to_str().unwrap().to_string(), source_streams);

    graph.toggle_edge(id, 0);
    graph.set_edge_codec(id, 0, Codec::Encode("flac".to_string()));
    graph.output.path = out_path.to_str().unwrap().to_string();

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
    assert_eq!(done_code.as_deref(), Some("0"), "ffmpeg did not exit cleanly");

    let out_streams = ffmpeg::probe(out_path.to_str().unwrap()).unwrap();
    assert_eq!(out_streams.len(), 1);
    assert_eq!(out_streams[0].codec, "flac", "expected the output to be re-encoded to flac");

    let _ = std::fs::remove_dir_all(&dir);
}

fn run_ok(cmd: &mut Command) {
    let status = cmd.status().expect("failed to run ffmpeg");
    assert!(status.success(), "ffmpeg setup command failed");
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

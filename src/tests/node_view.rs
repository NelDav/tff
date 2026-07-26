use super::*;


/// Right/Left should adjust `text_scroll` by a fixed step, floor at zero
/// (no negative scroll), and cap once the node's longest line (here, the
/// title -- a filename picked long enough that it, not any body line, is
/// what's driving the bound) is fully revealed, rather than growing
/// unbounded.
#[test]
fn scroll_node_text_steps_floors_and_caps() {
    use crate::app::App;

    // Title = " [0] this-is-a-very-long-filename-1234567890.mp4 " = 49
    // chars; default input width 34 minus 2 borders = 32 visible, so the
    // title needs 49 - 32 = 17 columns of scroll to fully reveal -- well
    // past any of this stream's own short body text.
    let mut app = App::new();
    app.graph.add_input("this-is-a-very-long-filename-1234567890.mp4".to_string(), video_stream(), Vec::new());
    app.focus = crate::app::Focus::Input(0);

    assert_eq!(app.text_scroll, 0);
    app.scroll_node_text(true);
    assert_eq!(app.text_scroll, 4);
    app.scroll_node_text(true);
    assert_eq!(app.text_scroll, 8);
    app.scroll_node_text(false);
    assert_eq!(app.text_scroll, 4);

    // Floors at zero rather than wrapping.
    app.scroll_node_text(false);
    app.scroll_node_text(false);
    assert_eq!(app.text_scroll, 0);

    // Caps at 17 (the title's own full reveal point) rather than growing
    // unbounded.
    for _ in 0..200 {
        app.scroll_node_text(true);
    }
    assert_eq!(app.text_scroll, 17);
}

/// Scrolling shouldn't be possible past the point where the node's
/// longest line -- title or body, whichever is longer -- is fully
/// revealed; going further would only show blank space.
#[test]
fn scroll_clamps_to_the_longest_line_in_the_node() {
    use crate::app::App;

    let mut app = App::new();
    // A short title (well under the box width) but a body line -- the
    // metadata value -- long enough to need real scrolling: the bound
    // should track that longer line, not the title.
    let modifier = app.graph.add_modifier(ModifierKind::Metadata {
        fields: metadata_fields(&[("title", "a-much-longer-value-than-the-boxs-own-default-width-provides")]),
    });
    let idx = app.graph.modifiers.iter().position(|m| m.id == modifier).unwrap();
    app.focus = crate::app::Focus::Modifier(idx);

    for _ in 0..200 {
        app.scroll_node_text(true);
    }
    let capped = app.text_scroll;
    assert!(capped > 0, "the long metadata value should allow real scrolling");

    // A node with nothing but short content shouldn't be scrollable at all.
    let short = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let short_idx = app.graph.inputs.iter().position(|n| n.id == short).unwrap();
    app.focus = crate::app::Focus::Input(short_idx);
    app.scroll_node_text(true);
    assert_eq!(app.text_scroll, 0, "nothing here is wider than the box, so there's nothing to scroll to");
}

/// Moving focus to a different node should reset the scroll -- it's a
/// property of "whichever node is currently in view", not something that
/// should carry over onto the next node Tab lands on.
#[test]
fn changing_focus_resets_text_scroll() {
    use crate::app::App;

    let mut app = App::new();
    app.graph.add_input("this-is-a-very-long-filename-1234567890.mp4".to_string(), video_stream(), Vec::new());
    app.graph.add_input("in2.mp4".to_string(), video_stream(), Vec::new());
    app.focus = crate::app::Focus::Input(0);
    app.scroll_node_text(true);
    assert_eq!(app.text_scroll, 4);

    app.cycle_focus(true);

    assert_eq!(app.text_scroll, 0);
}

/// Shift+Right/Left should grow/shrink only the focused node's own width,
/// floor at a minimum wide enough to still show a title, and cap at a
/// generous ceiling.
#[test]
fn resize_focused_node_grows_shrinks_and_clamps() {
    use crate::app::App;

    let mut app = App::new();
    let id = app.graph.add_input("in.mp4".to_string(), video_stream(), Vec::new());
    let other = app.graph.add_input("other.mp4".to_string(), video_stream(), Vec::new());
    app.focus = crate::app::Focus::Input(0);
    let starting_width = app.graph.input(id).unwrap().width;

    app.resize_focused_node(true);
    assert_eq!(app.graph.input(id).unwrap().width, starting_width + 2);
    app.resize_focused_node(false);
    app.resize_focused_node(false);
    assert_eq!(app.graph.input(id).unwrap().width, starting_width - 2);

    // Only the focused node's width should have changed.
    assert_eq!(app.graph.input(other).unwrap().width, starting_width);

    // Floors at a minimum rather than shrinking to nothing.
    for _ in 0..50 {
        app.resize_focused_node(false);
    }
    assert_eq!(app.graph.input(id).unwrap().width, 14);

    // Caps at a ceiling rather than growing unbounded.
    for _ in 0..200 {
        app.resize_focused_node(true);
    }
    assert_eq!(app.graph.input(id).unwrap().width, 200);
}

/// Scrolling should shift both the focused node's title and its body text
/// together (both draw from the same `text_scroll`), while leaving an
/// unfocused node's rendering untouched.
#[test]
fn ui_scrolls_the_focused_nodes_title_and_body_together() {
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new();
    app.graph.add_input(
        "a-very-long-input-filename-that-will-not-fit.mp4".to_string(),
        video_stream(),
        Vec::new(),
    );
    app.graph.add_input("short.mp4".to_string(), video_stream(), Vec::new());
    app.focus = crate::app::Focus::Input(0);

    let render = |app: &App| {
        let backend = TestBackend::new(160, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    };

    let before = render(&app);
    assert!(before.contains("a-very-long-input"), "expected the unscrolled title's start:\n{before}");

    for _ in 0..10 {
        app.scroll_node_text(true);
    }
    let after = render(&app);
    assert!(
        !after.contains("a-very-long-input"),
        "scrolling right should have moved the title's start out of view:\n{after}"
    );
    assert!(after.contains("not-fit.mp4"), "expected further-in text to now be visible:\n{after}");

    // The unfocused node's own title is untouched by the focused node's scroll.
    assert!(after.contains("short.mp4"), "unfocused node shouldn't be scrolled:\n{after}");
}

/// A short title shouldn't keep sliding out of view just because a much
/// longer body line (here, a metadata value) still has more to reveal --
/// once the title itself is fully shown, it should stay put while the
/// body keeps scrolling.
#[test]
fn ui_title_stops_scrolling_once_fully_revealed_even_if_body_has_more() {
    use crate::app::App;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    let mut app = App::new();
    app.graph.add_modifier(ModifierKind::Metadata {
        fields: metadata_fields(&[("title", "a-much-longer-value-than-the-boxs-own-default-width-provides")]),
    });
    app.focus = crate::app::Focus::Modifier(0);

    let render = |app: &App| {
        let backend = TestBackend::new(160, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| crate::ui::draw(frame, app)).unwrap();
        let buf = terminal.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    };

    // The title ("[metadata]") is short -- a handful of scroll presses is
    // already more than enough to fully reveal it.
    for _ in 0..5 {
        app.scroll_node_text(true);
    }
    let mostly_scrolled = render(&app);
    assert!(mostly_scrolled.contains("metadata"), "the short title should still be visible:\n{mostly_scrolled}");

    // Keep scrolling well past that, purely to reveal more of the long
    // metadata value -- the title shouldn't vanish just because the body
    // still has room to move.
    for _ in 0..20 {
        app.scroll_node_text(true);
    }
    let fully_scrolled = render(&app);
    assert!(
        fully_scrolled.contains("metadata"),
        "the title should stay visible once fully revealed, even as the body keeps scrolling:\n{fully_scrolled}"
    );
    assert!(
        fully_scrolled.contains("width-provides"),
        "expected the body to have scrolled further into the long value:\n{fully_scrolled}"
    );
}


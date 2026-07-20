use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, Focus, Mode};
use crate::graph::{Codec, Edge, InputNode, StreamKind};

pub fn draw(frame: &mut Frame, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),  // title / hotkeys
            Constraint::Min(10),    // node canvas
            Constraint::Length(1),  // status / text-input line
            Constraint::Length(10), // log pane
        ])
        .split(frame.area());

    draw_header(frame, root[0]);
    draw_graph(frame, app, root[1]);
    draw_status_line(frame, app, root[2]);
    draw_log(frame, app, root[3]);
    draw_picker_popup(frame, app);
}

fn draw_header(frame: &mut Frame, area: Rect) {
    let line = TextLine::from(
        " tff — node-based ffmpeg  │  Tab focus  ↑↓ port  hjkl move  a add-input  o output-path  c connect  d disconnect  e codec  f container  x delete-node  r render  q quit ",
    )
    .style(Style::default().fg(Color::Black).bg(Color::Cyan));
    frame.render_widget(Paragraph::new(line), area);
}

fn draw_status_line(frame: &mut Frame, app: &App, area: Rect) {
    let line = match &app.mode {
        Mode::TextInput { target, buffer } => {
            let prompt = match target {
                crate::app::TextTarget::NewInputPath => "add input file path: ",
                crate::app::TextTarget::OutputPath => "output file path: ",
            };
            TextLine::from(vec![
                Span::styled(prompt, Style::default().fg(Color::Yellow)),
                Span::raw(buffer.clone()),
                Span::styled("_", Style::default().add_modifier(Modifier::SLOW_BLINK)),
            ])
        }
        Mode::Picker { .. } => TextLine::from(Span::styled(
            "↑↓/jk move · / search · Enter select · Esc cancel",
            Style::default().fg(Color::Yellow),
        )),
        Mode::Normal => {
            if let Some((node_id, stream_idx)) = app.armed {
                if let Some(node) = app.graph.input(node_id) {
                    let label = node
                        .streams
                        .get(stream_idx)
                        .map(|s| s.label())
                        .unwrap_or_default();
                    TextLine::from(Span::styled(
                        format!(
                            "armed {label} from {} — focus OUTPUT and press 'c' to connect (Esc to cancel)",
                            node.path
                        ),
                        Style::default().fg(Color::Yellow),
                    ))
                } else {
                    TextLine::from("")
                }
            } else if app.running {
                TextLine::from(Span::styled(
                    app.status.clone(),
                    Style::default().fg(Color::Green),
                ))
            } else if !app.status.is_empty() {
                TextLine::from(app.status.clone())
            } else {
                TextLine::from(Span::styled(
                    "ready",
                    Style::default().fg(Color::DarkGray),
                ))
            }
        }
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn node_rows(n_streams: usize) -> u16 {
    (2 + n_streams.max(1)) as u16
}

/// Renders the node graph: a bordered panel with edge wires drawn straight
/// into the buffer as box-drawing characters (so they read as thin, crisp
/// lines rather than a braille-dot blob), then node boxes layered on top.
fn draw_graph(frame: &mut Frame, app: &App, area: Rect) {
    let panel = Block::default().borders(Borders::ALL).title(" graph ");
    let inner = panel.inner(area);
    frame.render_widget(panel, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    draw_edges(frame, app, inner);

    for (i, input) in app.graph.inputs.iter().enumerate() {
        let focused = matches!(app.focus, Focus::Input(fi) if fi == i);
        draw_input_node(frame, inner, input, &app.graph.edges, focused, app.port_idx, app.armed);
    }
    draw_output_node(frame, inner, app);
}

/// Draws each edge as an orthogonal wire (one or two `─` runs joined by a
/// `│` and box-drawing corners when source and destination rows differ),
/// colored by the stream kind it carries. Attaches one cell outside each
/// node's border so it never overlaps the box the node widgets draw later.
fn draw_edges(frame: &mut Frame, app: &App, area: Rect) {
    let output_rows = node_rows(app.graph.edges.len());
    let Some(dst_rect) = node_rect(area, app.graph.output.pos, app.graph.output.width, output_rows)
    else {
        return;
    };

    let buf = frame.buffer_mut();
    for (i, edge) in app.graph.edges.iter().enumerate() {
        let Some(input) = app.graph.input(edge.from_node) else {
            continue;
        };
        let Some(stream) = input.streams.get(edge.from_stream_idx) else {
            continue;
        };
        let src_rows = node_rows(input.streams.len());
        let Some(src_rect) = node_rect(area, input.pos, input.width, src_rows) else {
            continue;
        };

        let src = Position::new(src_rect.right(), src_rect.y + 1 + edge.from_stream_idx as u16);
        let dst = Position::new(dst_rect.x.saturating_sub(1), dst_rect.y + 1 + i as u16);
        let badge = match edge.codec {
            Codec::Copy => None,
            Codec::Encode(_) => Some(edge.codec.label()),
        };

        draw_wire(buf, area, src, dst, i as u16, kind_color(stream.kind), badge);
    }
}

/// Draws one wire, then -- if `badge` is set (the connection re-encodes
/// rather than copies) -- overlays a small colored label on the segment
/// leading into the destination, acting as a "converter" sitting on the
/// wire itself. Skipped if the segment isn't long enough to hold it.
fn draw_wire(
    buf: &mut Buffer,
    bounds: Rect,
    src: Position,
    dst: Position,
    lane: u16,
    color: Color,
    badge: Option<&str>,
) {
    let (sx, sy) = (src.x, src.y);
    let (dx, dy) = (dst.x, dst.y);
    let style = Style::default().fg(color);
    let mut put = |x: u16, y: u16, s: &str| {
        if x >= bounds.x && x < bounds.right() && y >= bounds.y && y < bounds.bottom() {
            buf.set_string(x, y, s, style);
        }
    };

    // (row, from_x, to_x) of the segment leading into the destination --
    // where the badge, if any, gets centered.
    let final_run = if sy == dy {
        let (from, to) = if sx <= dx { (sx, dx) } else { (dx, sx) };
        for x in from..=to {
            put(x, sy, "─");
        }
        (sy, from, to)
    } else {
        // Route the vertical leg through a lane offset by edge index, so
        // multiple wires leaving the same box don't all stack on one column.
        let (lo, hi) = if sx <= dx { (sx, dx) } else { (dx, sx) };
        let mid = if hi.saturating_sub(lo) >= 2 {
            (lo + 1 + lane).clamp(lo + 1, hi.saturating_sub(1))
        } else {
            lo + (hi - lo) / 2
        };

        let going_right = dx >= sx;
        let going_down = dy > sy;

        let (a, b) = if sx <= mid { (sx, mid) } else { (mid, sx) };
        for x in a..=b {
            put(x, sy, "─");
        }
        put(
            mid,
            sy,
            match (going_right, going_down) {
                (true, true) => "┐",
                (true, false) => "┘",
                (false, true) => "┌",
                (false, false) => "└",
            },
        );

        let (top, bottom) = if sy <= dy { (sy, dy) } else { (dy, sy) };
        for y in top..=bottom {
            put(mid, y, "│");
        }
        put(
            mid,
            dy,
            match (going_right, going_down) {
                (true, true) => "└",
                (true, false) => "┌",
                (false, true) => "┘",
                (false, false) => "┐",
            },
        );

        let (c, d) = if mid <= dx { (mid, dx) } else { (dx, mid) };
        for x in c..=d {
            put(x, dy, "─");
        }
        (dy, c, d)
    };

    let Some(text) = badge else { return };
    let (row, from, to) = final_run;
    let run_len = to.saturating_sub(from) + 1;
    let text_len = text.chars().count() as u16;
    if text_len + 2 > run_len {
        return; // not enough room to show the badge with the wire around it
    }
    let start_x = from + (run_len - text_len) / 2;
    let badge_style = Style::default().fg(Color::Black).bg(color).add_modifier(Modifier::BOLD);
    if row >= bounds.y && row < bounds.bottom() && start_x >= bounds.x && start_x + text_len <= bounds.right() {
        buf.set_string(start_x, row, text, badge_style);
    }
}

/// Node position -> screen Rect, clipped to the graph panel so a node
/// nudged far off-canvas never produces an out-of-bounds Rect.
fn node_rect(canvas_area: Rect, pos: (f64, f64), width: u16, rows: u16) -> Option<Rect> {
    let x = canvas_area.x.saturating_add(pos.0 as u16);
    let y = canvas_area.y.saturating_add(pos.1 as u16);
    let max_w = canvas_area.right().saturating_sub(x);
    let max_h = canvas_area.bottom().saturating_sub(y);
    let width = width.min(max_w);
    let height = rows.min(max_h);
    if width == 0 || height == 0 {
        None
    } else {
        Some(Rect { x, y, width, height })
    }
}

fn draw_input_node(
    frame: &mut Frame,
    canvas_area: Rect,
    node: &InputNode,
    edges: &[Edge],
    focused: bool,
    port_idx: usize,
    armed: Option<(usize, usize)>,
) {
    let rows = node_rows(node.streams.len());
    let Some(rect) = node_rect(canvas_area, node.pos, node.width, rows) else {
        return;
    };
    let border_color = if focused { Color::Yellow } else { Color::White };

    let mut lines = Vec::new();
    if node.streams.is_empty() {
        lines.push(TextLine::from("(no streams)"));
    }
    for (i, stream) in node.streams.iter().enumerate() {
        let is_port_focused = focused && i == port_idx;
        let is_armed = armed == Some((node.id, i));
        let marker = if is_armed { "◎" } else { "○" };
        let color = if is_armed {
            Color::Yellow
        } else {
            kind_color(stream.kind)
        };
        let mut style = Style::default().fg(color);
        if is_port_focused {
            style = style.add_modifier(Modifier::REVERSED);
        }
        let codec_tag = edges
            .iter()
            .find(|e| e.from_node == node.id && e.from_stream_idx == i)
            .and_then(|e| match e.codec {
                Codec::Copy => None,
                Codec::Encode(_) => Some(e.codec.label()),
            });
        let text = match codec_tag {
            Some(tag) => format!("{marker} {} → {tag}", stream.label()),
            None => format!("{marker} {}", stream.label()),
        };
        lines.push(TextLine::styled(text, style));
    }

    let basename = node.path.rsplit('/').next().unwrap_or(&node.path);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            format!(" [{}] {} ", node.file_index, basename),
            Style::default().fg(border_color).add_modifier(Modifier::BOLD),
        ));

    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

fn draw_output_node(frame: &mut Frame, canvas_area: Rect, app: &App) {
    let node = &app.graph.output;
    let rows = node_rows(app.graph.edges.len());
    let Some(rect) = node_rect(canvas_area, node.pos, node.width, rows) else {
        return;
    };
    let focused = matches!(app.focus, Focus::Output);
    let border_color = if focused { Color::Yellow } else { Color::Cyan };

    let mut lines = Vec::new();
    if app.graph.edges.is_empty() {
        lines.push(TextLine::from("(nothing mapped — arm a stream with 'c')"));
    }
    for edge in &app.graph.edges {
        let codec_suffix = match edge.codec {
            Codec::Copy => String::new(),
            Codec::Encode(_) => format!(" [{}]", edge.codec.label()),
        };
        let label = app
            .graph
            .input(edge.from_node)
            .and_then(|n| n.streams.get(edge.from_stream_idx).map(|s| (n, s)))
            .map(|(n, s)| {
                format!(
                    "● {}{codec_suffix} <- [{}] {}",
                    s.label(),
                    n.file_index,
                    n.path.rsplit('/').next().unwrap_or(&n.path)
                )
            })
            .unwrap_or_else(|| "● (dangling)".to_string());
        lines.push(TextLine::from(label));
    }

    let container_tag = match &node.container {
        Some(name) => format!(" [{name}]"),
        None => String::new(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            format!(" OUTPUT: {}{container_tag} ", node.path),
            Style::default().fg(border_color).add_modifier(Modifier::BOLD),
        ));

    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

fn kind_color(kind: StreamKind) -> Color {
    match kind {
        StreamKind::Video => Color::LightBlue,
        StreamKind::Audio => Color::LightGreen,
        StreamKind::Subtitle => Color::LightMagenta,
        StreamKind::Other => Color::Gray,
    }
}

fn draw_log(frame: &mut Frame, app: &App, area: Rect) {
    let inner_height = area.height.saturating_sub(2) as usize; // minus borders
    let start = app.log.len().saturating_sub(inner_height);
    let lines: Vec<TextLine> = app.log[start..].iter().map(|l| TextLine::from(l.as_str())).collect();
    let block = Block::default().borders(Borders::ALL).title(" log ");
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// Floating, centered, scrollable option list -- the dropdown for codec and
/// container selection. Drawn last so it sits on top of everything else.
/// Supports a vim-like `/` search that filters the list live as you type.
fn draw_picker_popup(frame: &mut Frame, app: &App) {
    let Mode::Picker { title, options, selected, query, searching, .. } = &app.mode else {
        return;
    };
    let filtered = crate::app::filtered_indices(options, query);
    let show_search_line = *searching || !query.is_empty();

    let area = frame.area();
    let popup_width = area.width.saturating_sub(4).clamp(20, 60);
    let content_rows = filtered.len().max(1) as u16 + u16::from(show_search_line);
    let popup_height = (content_rows + 2).clamp(3, 17).min(area.height.saturating_sub(2));
    let popup = centered_rect(popup_width, popup_height, area);

    let hint = if *searching {
        " type to filter · Enter confirm · Esc cancel search "
    } else {
        " ↑↓/jk move · / search · Enter select · Esc cancel "
    };

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Span::styled(
            format!(" {title} "),
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(hint, Style::default().fg(Color::DarkGray)));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height == 0 {
        return;
    }

    let mut list_area = inner;
    if show_search_line {
        let cursor = if *searching { "_" } else { "" };
        let search_line = TextLine::from(vec![
            Span::styled("/", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)),
            Span::raw(query.as_str()),
            Span::styled(cursor, Style::default().add_modifier(Modifier::SLOW_BLINK)),
        ]);
        frame.render_widget(
            Paragraph::new(search_line),
            Rect { x: inner.x, y: inner.y, width: inner.width, height: 1 },
        );
        list_area = Rect {
            x: inner.x,
            y: inner.y + 1,
            width: inner.width,
            height: inner.height.saturating_sub(1),
        };
    }

    if list_area.height == 0 {
        return;
    }

    if filtered.is_empty() {
        frame.render_widget(
            Paragraph::new(TextLine::styled(" (no matches)", Style::default().fg(Color::DarkGray))),
            list_area,
        );
        return;
    }

    let visible = list_area.height as usize;
    let scroll = if filtered.len() <= visible {
        0
    } else {
        (*selected).saturating_sub(visible / 2).min(filtered.len() - visible)
    };

    let lines: Vec<TextLine> = filtered
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .map(|(i, &real_idx)| {
            let style = if i == *selected {
                Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            TextLine::styled(format!(" {}", options[real_idx].display), style)
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), list_area);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect { x, y, width, height }
}

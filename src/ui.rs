use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::app::{App, Focus, Mode};
use crate::graph::{
    Codec, Endpoint, Graph, InputNode, ModifierKind, ModifierNode, NodeId, OutputNode, StreamKind, Target,
};

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
    draw_suggestions_popup(frame, app, root[2]);
}

fn draw_header(frame: &mut Frame, area: Rect) {
    let line = TextLine::from(
        " tff — node-based ffmpeg  │  Tab focus  ↑↓ row  hjkl move  a add-input  O add-output  m add-modifier  o output-path  c arm/connect  d disconnect  e edit  f container  x delete-node  r render  p preview  q quit ",
    )
    .style(Style::default().fg(Color::Black).bg(Color::Cyan));
    frame.render_widget(Paragraph::new(line), area);
}

fn describe_endpoint(graph: &Graph, ep: Endpoint) -> Option<String> {
    match ep {
        Endpoint::Stream { node, stream_idx } => {
            let input = graph.input(node)?;
            let stream = input.streams.get(stream_idx)?;
            Some(format!("{} from {}", stream.label(), input.path))
        }
        Endpoint::ModifierOut(id) => {
            let m = graph.modifier(id)?;
            Some(format!("output of [{}]", m.kind.short_label()))
        }
    }
}

fn draw_status_line(frame: &mut Frame, app: &App, area: Rect) {
    let line = match &app.mode {
        Mode::TextInput { target, buffer, .. } => {
            let prompt = match target {
                crate::app::TextTarget::NewInputPath => "add input file path: ".to_string(),
                crate::app::TextTarget::OutputPath(_) => "output file path: ".to_string(),
                crate::app::TextTarget::ModifierMetadataValue { key, .. } => format!("{key}: "),
                crate::app::TextTarget::ModifierCustomKey(_) => "custom metadata key: ".to_string(),
                crate::app::TextTarget::ModifierFilterValue { key, .. } => format!("{key}: "),
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
            if let Some(ep) = app.armed {
                match describe_endpoint(&app.graph, ep) {
                    Some(desc) => TextLine::from(Span::styled(
                        format!("armed {desc} — focus a modifier or output, press 'c' to connect (Esc to cancel)"),
                        Style::default().fg(Color::Yellow),
                    )),
                    None => TextLine::from(""),
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

/// A Metadata modifier's box has an upper section (one row per field it
/// sets, or a placeholder row) plus a divider, on top of the connections
/// section every modifier has; a Convert modifier has just the connections
/// section, since its one "field" already fits in the title bar.
fn modifier_field_rows(kind: &ModifierKind) -> u16 {
    match kind {
        ModifierKind::Convert(_) => 0,
        ModifierKind::Metadata { fields } => fields.len().max(1) as u16 + 1, // + divider
        ModifierKind::Disposition { flags } => flags.len().max(1) as u16 + 1, // + divider
        ModifierKind::Filter { fields, .. } => fields.len().max(1) as u16 + 1, // + divider
    }
}

/// Row (0-based from the box's top border) where the incoming-connection
/// line sits -- right after the field section, if any.
fn modifier_incoming_row(kind: &ModifierKind) -> u16 {
    1 + modifier_field_rows(kind)
}

/// Row where the outgoing-connection list starts.
fn modifier_outgoing_start_row(kind: &ModifierKind) -> u16 {
    modifier_incoming_row(kind) + 1
}

fn modifier_rows(kind: &ModifierKind, outgoing_count: usize) -> u16 {
    modifier_outgoing_start_row(kind) + outgoing_count.max(1) as u16 + 1 // + bottom border
}

/// Renders the node graph: a bordered panel with connection wires drawn
/// straight into the buffer as box-drawing characters (so they read as
/// thin, crisp lines rather than a braille-dot blob), then node boxes
/// layered on top.
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
        draw_input_node(frame, inner, &app.graph, input, focused, app.row_idx, app.armed);
    }
    for (i, modifier) in app.graph.modifiers.iter().enumerate() {
        draw_modifier_node(frame, inner, app, i, modifier);
    }
    for (i, output) in app.graph.outputs.iter().enumerate() {
        draw_output_node(frame, inner, app, i, output);
    }
}

/// Every node's screen Rect, keyed by id, computed once per frame so edge
/// drawing and node drawing agree on where everything is.
fn compute_rects(app: &App, area: Rect) -> Vec<(NodeId, Rect)> {
    let mut rects = Vec::new();
    for input in &app.graph.inputs {
        let rows = node_rows(input.streams.len());
        if let Some(r) = node_rect(area, input.pos, input.width, rows) {
            rects.push((input.id, r));
        }
    }
    for m in &app.graph.modifiers {
        let rows = modifier_rows(&m.kind, app.graph.outgoing(Endpoint::ModifierOut(m.id)).len());
        if let Some(r) = node_rect(area, m.pos, m.width, rows) {
            rects.push((m.id, r));
        }
    }
    for output in &app.graph.outputs {
        let rows = node_rows(app.graph.incoming(Target::Output(output.id)).len());
        if let Some(r) = node_rect(area, output.pos, output.width, rows) {
            rects.push((output.id, r));
        }
    }
    rects
}

fn rect_for(rects: &[(NodeId, Rect)], id: NodeId) -> Option<Rect> {
    rects.iter().find(|(rid, _)| *rid == id).map(|(_, r)| *r)
}

fn wire_color(graph: &Graph, from: Endpoint) -> Color {
    graph
        .resolve(from)
        .and_then(|r| graph.input(r.from_node).and_then(|inp| inp.streams.get(r.from_stream_idx)).map(|s| s.kind))
        .map(kind_color)
        .unwrap_or(Color::DarkGray)
}

/// Draws each connection as an orthogonal wire (one or two `─` runs joined
/// by a `│` and box-drawing corners when source and destination rows
/// differ), colored by the stream kind resolved at its ultimate source.
/// Attaches one cell outside each node's border so it never overlaps the
/// box the node widgets draw later. A wire leaving a non-Copy Convert
/// node's output is tagged with that node's codec right where it happens.
fn draw_edges(frame: &mut Frame, app: &App, area: Rect) {
    let rects = compute_rects(app, area);
    let buf = frame.buffer_mut();

    for (wire_idx, wire) in app.graph.wires.iter().enumerate() {
        let src_id = match wire.from {
            Endpoint::Stream { node, .. } => node,
            Endpoint::ModifierOut(id) => id,
        };
        let Some(src_rect) = rect_for(&rects, src_id) else { continue };
        let (src_row_offset, src_row) = match wire.from {
            Endpoint::Stream { stream_idx, .. } => (1u16, stream_idx),
            Endpoint::ModifierOut(mid) => {
                let row = app.graph.outgoing(wire.from).iter().position(|&wi| wi == wire_idx).unwrap_or(0);
                let offset = app.graph.modifier(mid).map(|m| modifier_outgoing_start_row(&m.kind)).unwrap_or(2);
                (offset, row) // outgoing rows sit below the field/incoming sections
            }
        };
        let src = Position::new(src_rect.right(), src_rect.y + src_row_offset + src_row as u16);

        let dst_id = match wire.to {
            Target::ModifierIn(id) => id,
            Target::Output(id) => id,
        };
        let Some(dst_rect) = rect_for(&rects, dst_id) else { continue };
        let (dst_row_offset, dst_row) = match wire.to {
            Target::ModifierIn(mid) => {
                let offset = app.graph.modifier(mid).map(|m| modifier_incoming_row(&m.kind)).unwrap_or(1);
                (offset, 0u16) // a modifier only ever has one incoming row
            }
            Target::Output(_) => {
                let row = app.graph.incoming(wire.to).iter().position(|&wi| wi == wire_idx).unwrap_or(0);
                (1u16, row as u16)
            }
        };
        let dst = Position::new(dst_rect.x.saturating_sub(1), dst_rect.y + dst_row_offset + dst_row);

        let badge = match wire.from {
            Endpoint::ModifierOut(mid) => match app.graph.modifier(mid).map(|m| &m.kind) {
                Some(ModifierKind::Convert(codec)) if !matches!(codec, Codec::Copy) => {
                    Some(codec.label().to_string())
                }
                _ => None,
            },
            Endpoint::Stream { .. } => None,
        };

        draw_wire(buf, area, src, dst, wire_idx as u16, wire_color(&app.graph, wire.from), badge.as_deref());
    }
}

/// Draws one wire, then -- if `badge` is set -- overlays a small colored
/// label on the segment leading into the destination, acting as a
/// "converter" sitting on the wire itself. Skipped if the segment isn't
/// long enough to hold it.
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
        // Route the vertical leg through a lane offset by wire index, so
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
    graph: &Graph,
    node: &InputNode,
    focused: bool,
    row_idx: usize,
    armed: Option<Endpoint>,
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
        let is_row_focused = focused && i == row_idx;
        let ep = Endpoint::Stream { node: node.id, stream_idx: i };
        let is_armed = armed == Some(ep);
        let marker = if is_armed { "◎" } else { "○" };
        let color = if is_armed {
            Color::Yellow
        } else {
            kind_color(stream.kind)
        };
        let mut style = Style::default().fg(color);
        if is_row_focused {
            style = style.add_modifier(Modifier::REVERSED);
        }
        // A stream can fan out to more than one downstream node; the wire
        // itself shows where a single connection goes, so only call out
        // the count when there's more than one to disambiguate.
        let count = graph.outgoing(ep).len();
        let suffix = if count > 1 { format!(" → {count} connections") } else { String::new() };
        lines.push(TextLine::styled(format!("{marker} {}{suffix}", stream.label()), style));
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

/// A Metadata or Disposition modifier's box has an upper section listing
/// everything it sets (field values, or active flags) above a divider;
/// every modifier then has a connections section below -- first its single
/// incoming connection (or lack of one), then its outgoing connections one
/// per row, the same way an output node lists its incoming ones.
fn draw_modifier_node(frame: &mut Frame, canvas_area: Rect, app: &App, index: usize, node: &ModifierNode) {
    let outgoing = app.graph.outgoing(Endpoint::ModifierOut(node.id));
    let rows = modifier_rows(&node.kind, outgoing.len());
    let Some(rect) = node_rect(canvas_area, node.pos, node.width, rows) else {
        return;
    };
    let focused = matches!(app.focus, Focus::Modifier(mi) if mi == index);
    let border_color = if focused { Color::Yellow } else { Color::Magenta };

    let mut lines = Vec::new();

    match &node.kind {
        ModifierKind::Metadata { fields } => {
            if fields.is_empty() {
                lines.push(TextLine::styled("(no metadata set)", Style::default().fg(Color::DarkGray)));
            } else {
                for (key, value) in fields {
                    lines.push(TextLine::from(format!("{key}: {value}")));
                }
            }
            let divider_width = rect.width.saturating_sub(2) as usize;
            lines.push(TextLine::styled("─".repeat(divider_width), Style::default().fg(Color::DarkGray)));
        }
        ModifierKind::Disposition { flags } => {
            if flags.is_empty() {
                lines.push(TextLine::styled("(no dispositions set)", Style::default().fg(Color::DarkGray)));
            } else {
                for flag in flags {
                    lines.push(TextLine::from(flag.clone()));
                }
            }
            let divider_width = rect.width.saturating_sub(2) as usize;
            lines.push(TextLine::styled("─".repeat(divider_width), Style::default().fg(Color::DarkGray)));
        }
        ModifierKind::Filter { fields, .. } => {
            if fields.is_empty() {
                lines.push(TextLine::styled("(no parameters set)", Style::default().fg(Color::DarkGray)));
            } else {
                for (key, value) in fields {
                    lines.push(TextLine::from(format!("{key}: {value}")));
                }
            }
            let divider_width = rect.width.saturating_sub(2) as usize;
            lines.push(TextLine::styled("─".repeat(divider_width), Style::default().fg(Color::DarkGray)));
        }
        ModifierKind::Convert(_) => {}
    }

    let incoming_wire = app.graph.wires.iter().find(|w| w.to == Target::ModifierIn(node.id));
    let incoming_text = match incoming_wire {
        Some(w) => match app.graph.resolve(w.from) {
            Some(r) => app
                .graph
                .input(r.from_node)
                .and_then(|inp| inp.streams.get(r.from_stream_idx))
                .map(|s| format!("← {}", s.label()))
                .unwrap_or_else(|| "← (unknown)".to_string()),
            None => "← (broken chain)".to_string(),
        },
        None => "← (unconnected)".to_string(),
    };
    lines.push(TextLine::styled(incoming_text, Style::default().fg(Color::DarkGray)));

    if outgoing.is_empty() {
        lines.push(TextLine::styled(
            "(nothing downstream)",
            Style::default().fg(Color::DarkGray),
        ));
    }
    for (row, &wi) in outgoing.iter().enumerate() {
        let wire = &app.graph.wires[wi];
        let target_label = match wire.to {
            Target::ModifierIn(id) => app
                .graph
                .modifier(id)
                .map(|m| format!("→ [{}]", m.kind.short_label()))
                .unwrap_or_else(|| "→ (?)".to_string()),
            Target::Output(id) => match app.graph.outputs.iter().position(|o| o.id == id) {
                Some(oi) => format!("→ OUTPUT {}: {}", oi + 1, app.graph.outputs[oi].path),
                None => "→ (?)".to_string(),
            },
        };
        let mut style = Style::default();
        if focused && row == app.row_idx {
            style = style.add_modifier(Modifier::REVERSED);
        }
        lines.push(TextLine::styled(target_label, style));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            format!(" [{}] ", node.kind.short_label()),
            Style::default().fg(border_color).add_modifier(Modifier::BOLD),
        ));

    frame.render_widget(Paragraph::new(lines).block(block), rect);
}

fn draw_output_node(frame: &mut Frame, canvas_area: Rect, app: &App, index: usize, node: &OutputNode) {
    let incoming = app.graph.incoming(Target::Output(node.id));
    let rows = node_rows(incoming.len());
    let Some(rect) = node_rect(canvas_area, node.pos, node.width, rows) else {
        return;
    };
    let focused = matches!(app.focus, Focus::Output(oi) if oi == index);
    let border_color = if focused { Color::Yellow } else { Color::Cyan };

    let mut lines = Vec::new();
    if incoming.is_empty() {
        lines.push(TextLine::from("(nothing mapped — arm a stream with 'c')"));
    }
    for (row, &wi) in incoming.iter().enumerate() {
        let wire = &app.graph.wires[wi];
        let label = match app.graph.resolve(wire.from) {
            Some(r) => {
                let mut tags = Vec::new();
                if r.codec.ffmpeg_name().is_some() {
                    tags.push(r.codec.label().to_string());
                }
                for (key, value) in &r.metadata {
                    tags.push(format!("{key}:{value}"));
                }
                // The tag goes right after the stream label -- before the
                // "<- source file" part -- so it survives the box's width
                // truncation instead of being the first thing clipped off.
                let tag = if tags.is_empty() { String::new() } else { format!(" [{}]", tags.join(", ")) };
                app.graph
                    .input(r.from_node)
                    .and_then(|n| n.streams.get(r.from_stream_idx).map(|s| (n, s)))
                    .map(|(n, s)| {
                        format!(
                            "{}{tag} <- [{}] {}",
                            s.label(),
                            n.file_index,
                            n.path.rsplit('/').next().unwrap_or(&n.path)
                        )
                    })
                    .unwrap_or_else(|| format!("(dangling){tag}"))
            }
            None => "● (broken chain)".to_string(),
        };
        let mut style = Style::default();
        if focused && row == app.row_idx {
            style = style.add_modifier(Modifier::REVERSED);
        }
        lines.push(TextLine::styled(label, style));
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
            format!(" OUTPUT {}: {}{container_tag} ", index + 1, node.path),
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

/// Floating, centered, scrollable option list -- the dropdown for codec,
/// container, and new-modifier selection. Drawn last so it sits on top of
/// everything else. Supports a vim-like `/` search that filters the list
/// live as you type.
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

/// Live path-completion dropdown shown under the input line while adding an
/// input file or editing an output's path -- non-modal (typing keeps
/// working normally), unlike the picker popup. Hidden whenever there are no
/// matches (including for free-text fields like metadata, which never
/// populate suggestions), so it never sits there empty.
fn draw_suggestions_popup(frame: &mut Frame, app: &App, status_area: Rect) {
    let Mode::TextInput { suggestions, selected, .. } = &app.mode else {
        return;
    };
    if suggestions.is_empty() {
        return;
    }

    let area = frame.area();
    let popup_width = area.width.saturating_sub(4).clamp(20, 60);
    let max_visible = 8u16;
    let popup_height = (suggestions.len() as u16).min(max_visible) + 2;
    let available_height = area.height.saturating_sub(status_area.bottom());
    if available_height < 3 {
        return;
    }
    let popup = Rect {
        x: status_area.x.min(area.width.saturating_sub(popup_width)),
        y: status_area.bottom(),
        width: popup_width,
        height: popup_height.min(available_height),
    };

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::DarkGray))
        .title(Span::styled(" suggestions ", Style::default().fg(Color::DarkGray)))
        .title_bottom(Span::styled(" Tab complete · ↑↓ cycle ", Style::default().fg(Color::DarkGray)));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height == 0 {
        return;
    }

    let visible = inner.height as usize;
    let scroll = if suggestions.len() <= visible {
        0
    } else {
        (*selected).saturating_sub(visible / 2).min(suggestions.len() - visible)
    };

    let lines: Vec<TextLine> = suggestions
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .map(|(i, s)| {
            let style = if i == *selected {
                Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            TextLine::styled(format!(" {s}"), style)
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

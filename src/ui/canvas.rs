use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use ratatui::Frame;

use super::describe_endpoint;
use crate::app::{App, Focus};
use crate::graph::{Endpoint, InputNode, ModifierKind, ModifierNode, NodeId, OutputNode, StreamKind, Target};

/// Drops the first `scroll` characters of `text` -- used to horizontally
/// scroll a focused node's title (see `App::scroll_node_text`), since a
/// `Block`'s title is drawn outside the `Paragraph` body and so isn't
/// reached by `Paragraph::scroll`, which only shifts the body. Scrolling
/// past the end of the text yields "", not a panic.
fn scroll_text(text: &str, scroll: u16) -> &str {
    match text.char_indices().nth(scroll as usize) {
        Some((byte_idx, _)) => &text[byte_idx..],
        None => "",
    }
}

/// The scroll actually applied to a node's title, which can lag behind
/// the shared `App::text_scroll` -- a longer body line can push that
/// past what the (usually shorter) title needs, and the title shouldn't
/// keep sliding out of view once it's fully shown just because the body
/// still has more to reveal. Clamped to the title's own length so it
/// stops the moment its last character reaches the box's left edge.
fn title_scroll(title: &str, scroll: u16, inner_width: u16) -> u16 {
    scroll.min((title.chars().count() as u16).saturating_sub(inner_width))
}

// --- Text-scroll bounds -------------------------------------------------
//
// `App::scroll_node_text` needs to know how far the *focused* node's
// longest line runs before it can decide how far scrolling is allowed to
// go -- but that content is only otherwise computed at render time, deep
// inside each `draw_*_node` function, styled and all. Rather than have
// `App` reach into rendering (or duplicate the render call just to throw
// the styling away), each `draw_*_node` has a plain-text twin below --
// title plus every body line, no color or markers -- that `App` calls
// directly. Divider lines are deliberately left out: they're always
// exactly as wide as the box already is, so they can never be the
// longest line and never change the answer. Keep each twin in sync by
// hand with its `draw_*_node` counterpart -- a change to one is a strong
// hint the other needs the same edit.

pub(crate) fn input_node_text_extent(app: &App, node: &InputNode) -> (String, Vec<String>) {
    let mut lines = Vec::new();
    for (key, value) in &node.extra_args {
        lines.push(if value.is_empty() { format!("-{key}") } else { format!("-{key} {value}") });
    }
    if node.streams.is_empty() {
        lines.push("(no streams)".to_string());
    }
    for (i, stream) in node.streams.iter().enumerate() {
        let ep = Endpoint::Stream { node: node.id, stream_idx: i };
        let count = app.graph.outgoing(ep).len();
        let suffix = if count > 1 { format!(" → {count} connections") } else { String::new() };
        lines.push(format!("○ {}{suffix}", stream.label()));
    }
    let basename = node.path.rsplit('/').next().unwrap_or(&node.path);
    (format!(" [{}] {} ", node.file_index, basename), lines)
}

pub(crate) fn modifier_node_text_extent(app: &App, node: &ModifierNode) -> (String, Vec<String>) {
    let mut lines = Vec::new();
    match &node.kind {
        ModifierKind::Metadata { fields } => {
            if fields.is_empty() {
                lines.push("(no metadata set)".to_string());
            } else {
                lines.extend(fields.iter().map(|(key, value)| format!("{key}: {value}")));
            }
        }
        ModifierKind::Disposition { flags } => {
            if flags.is_empty() {
                lines.push("(no dispositions set)".to_string());
            } else {
                lines.extend(flags.iter().cloned());
            }
        }
        ModifierKind::Filter { fields, .. } => {
            if fields.is_empty() {
                lines.push("(no parameters set)".to_string());
            } else {
                lines.extend(fields.iter().map(|(key, value)| format!("{key}: {value}")));
            }
        }
        ModifierKind::Convert(_) => {}
        ModifierKind::ChapterEdit { chapters } => {
            if chapters.is_empty() {
                lines.push("(no chapters set)".to_string());
            } else {
                for c in chapters {
                    let label = if c.title.is_empty() { "(untitled)" } else { &c.title };
                    lines.push(format!(
                        "{}–{}  {label}",
                        crate::graph::format_time(c.start_secs),
                        crate::graph::format_time(c.end_secs)
                    ));
                }
            }
        }
    }

    let incoming_wire = app.graph.wires.iter().find(|w| w.to == Target::ModifierIn(node.id));
    lines.push(match incoming_wire {
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
    });

    let outgoing = app.graph.outgoing(Endpoint::ModifierOut(node.id));
    if outgoing.is_empty() {
        lines.push("(nothing downstream)".to_string());
    }
    for &wi in &outgoing {
        let wire = &app.graph.wires[wi];
        lines.push(match wire.to {
            Target::ModifierIn(id) => app
                .graph
                .modifier(id)
                .map(|m| format!("→ [{}]", m.kind.short_label()))
                .unwrap_or_else(|| "→ (?)".to_string()),
            Target::Output(id) => match app.graph.outputs.iter().position(|o| o.id == id) {
                Some(oi) => format!("→ OUTPUT {}: {}", oi + 1, app.graph.outputs[oi].path),
                None => "→ (?)".to_string(),
            },
            Target::OutputChapters(id) => match app.graph.outputs.iter().position(|o| o.id == id) {
                Some(oi) => format!("→ OUTPUT {} chapters", oi + 1),
                None => "→ (?)".to_string(),
            },
        });
    }

    (format!(" [{}] ", node.kind.short_label()), lines)
}

pub(crate) fn output_node_text_extent(app: &App, index: usize, node: &OutputNode) -> (String, Vec<String>) {
    let mut lines = Vec::new();
    for (key, value) in &node.extra_args {
        lines.push(if value.is_empty() { format!("-{key}") } else { format!("-{key} {value}") });
    }
    let incoming = app.graph.incoming(Target::Output(node.id));
    if incoming.is_empty() {
        lines.push("(nothing mapped — arm a stream with 'c')".to_string());
    }
    for &wi in &incoming {
        let wire = &app.graph.wires[wi];
        lines.push(match app.graph.resolve(wire.from) {
            Some(r) => {
                let mut tags = Vec::new();
                if r.codec.ffmpeg_name().is_some() {
                    tags.push(r.codec.label().to_string());
                }
                tags.extend(r.metadata.iter().map(|(key, value)| format!("{key}:{value}")));
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
        });
    }
    let chapter_wires = app.graph.incoming(Target::OutputChapters(node.id));
    if let Some(&wi) = chapter_wires.first() {
        lines.push(match describe_endpoint(&app.graph, app.graph.wires[wi].from) {
            Some(desc) => format!("chapters <- {desc}"),
            None => "chapters <- (?)".to_string(),
        });
    }

    let container_tag = match &node.container {
        Some(name) => format!(" [{name}]"),
        None => String::new(),
    };
    (format!(" OUTPUT {}: {}{container_tag} ", index + 1, node.path), lines)
}

/// An input/output node's box gets the same two-part body a Metadata/
/// Filter/Disposition modifier has: an upper section listing its extra
/// ffmpeg args, if any, a divider, then its usual content (streams for an
/// input, mapped connections plus a chapters row for an output). Unlike
/// those modifier kinds, the upper section is entirely absent (not even a
/// placeholder row) when there's nothing to show -- extra_args is a rare,
/// advanced add-on here, not the node's whole reason for existing, so it
/// shouldn't add a blank line to every ordinary input/output node the way
/// it does for e.g. an empty Metadata node (where showing "(no metadata
/// set)" makes sense because setting metadata *is* that node's entire
/// purpose).
pub(super) fn extra_args_field_rows(extra_args: &std::collections::BTreeMap<String, String>) -> u16 {
    if extra_args.is_empty() { 0 } else { extra_args.len() as u16 + 1 } // + divider
}

fn node_rows(n_streams: usize, upper_section_rows: u16) -> u16 {
    2 + upper_section_rows + n_streams.max(1) as u16
}

/// An output's body rows: one per mapped stream (its `incoming` list), plus
/// one more for its chapters slot -- but only when something's actually
/// connected there, same as video/audio streams don't get a placeholder
/// row when unmapped. See `App::cycle_row`'s Output arm, which this
/// mirrors.
fn output_body_rows(incoming_count: usize, has_chapters: bool) -> usize {
    // At least one row for the mapped streams (even with none connected,
    // that's a "(nothing mapped)" placeholder row -- see `draw_output_node`).
    incoming_count.max(1) + usize::from(has_chapters)
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
        ModifierKind::ChapterEdit { chapters } => chapters.len().max(1) as u16 + 1, // + divider
    }
}

/// Row (0-based from the box's top border) where the incoming-connection
/// line sits -- right after the field section, if any.
pub(super) fn modifier_incoming_row(kind: &ModifierKind) -> u16 {
    1 + modifier_field_rows(kind)
}

/// Row where the outgoing-connection list starts.
pub(super) fn modifier_outgoing_start_row(kind: &ModifierKind) -> u16 {
    modifier_incoming_row(kind) + 1
}

fn modifier_rows(kind: &ModifierKind, outgoing_count: usize) -> u16 {
    modifier_outgoing_start_row(kind) + outgoing_count.max(1) as u16 + 1 // + bottom border
}

/// Renders the node graph: a bordered panel with connection wires drawn
/// straight into the buffer as box-drawing characters (so they read as
/// thin, crisp lines rather than a braille-dot blob), then node boxes
/// layered on top.
pub(super) fn draw_graph(frame: &mut Frame, app: &App, area: Rect) {
    let panel = Block::default().borders(Borders::ALL).title(" graph ");
    let inner = panel.inner(area);
    frame.render_widget(panel, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    super::wires::draw_edges(frame, app, inner);

    for (i, input) in app.graph.inputs.iter().enumerate() {
        draw_input_node(frame, inner, app, i, input);
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
pub(super) fn compute_rects(app: &App, area: Rect) -> Vec<(NodeId, Rect)> {
    let mut rects = Vec::new();
    for input in &app.graph.inputs {
        let rows = node_rows(input.streams.len(), extra_args_field_rows(&input.extra_args));
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
        let has_chapters = !app.graph.incoming(Target::OutputChapters(output.id)).is_empty();
        let body_rows = output_body_rows(app.graph.incoming(Target::Output(output.id)).len(), has_chapters);
        let rows = node_rows(body_rows, extra_args_field_rows(&output.extra_args));
        if let Some(r) = node_rect(area, output.pos, output.width, rows) {
            rects.push((output.id, r));
        }
    }
    rects
}

pub(super) fn rect_for(rects: &[(NodeId, Rect)], id: NodeId) -> Option<Rect> {
    rects.iter().find(|(rid, _)| *rid == id).map(|(_, r)| *r)
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

fn draw_input_node(frame: &mut Frame, canvas_area: Rect, app: &App, index: usize, node: &InputNode) {
    let focused = matches!(app.focus, Focus::Input(fi) if fi == index);
    let row_idx = app.row_idx;
    let rows = node_rows(node.streams.len(), extra_args_field_rows(&node.extra_args));
    let Some(rect) = node_rect(canvas_area, node.pos, node.width, rows) else {
        return;
    };
    let border_color = if focused { Color::Yellow } else { Color::White };

    let mut lines = Vec::new();
    if !node.extra_args.is_empty() {
        for (key, value) in &node.extra_args {
            let text = if value.is_empty() { format!("-{key}") } else { format!("-{key} {value}") };
            lines.push(TextLine::from(text));
        }
        let divider_width = rect.width.saturating_sub(2) as usize;
        lines.push(TextLine::styled("─".repeat(divider_width), Style::default().fg(Color::DarkGray)));
    }
    if node.streams.is_empty() {
        lines.push(TextLine::from("(no streams)"));
    }
    for (i, stream) in node.streams.iter().enumerate() {
        let is_row_focused = focused && i == row_idx;
        let ep = Endpoint::Stream { node: node.id, stream_idx: i };
        let is_armed = app.armed.contains(&ep);
        let is_selected = app.selected.contains(&ep);
        let marker = if is_armed { "◎" } else if is_selected { "●" } else { "○" };
        let color = if is_armed {
            Color::Yellow
        } else if is_selected {
            Color::Cyan
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
        let count = app.graph.outgoing(ep).len();
        let suffix = if count > 1 { format!(" → {count} connections") } else { String::new() };
        lines.push(TextLine::styled(format!("{marker} {}{suffix}", stream.label()), style));
    }

    let basename = node.path.rsplit('/').next().unwrap_or(&node.path);
    let scroll = if focused { app.text_scroll } else { 0 };
    let title = format!(" [{}] {} ", node.file_index, basename);
    let inner_width = rect.width.saturating_sub(2);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            scroll_text(&title, title_scroll(&title, scroll, inner_width)),
            Style::default().fg(border_color).add_modifier(Modifier::BOLD),
        ));

    frame.render_widget(Paragraph::new(lines).block(block).scroll((0, scroll)), rect);
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
        ModifierKind::ChapterEdit { chapters } => {
            if chapters.is_empty() {
                lines.push(TextLine::styled("(no chapters set)", Style::default().fg(Color::DarkGray)));
            } else {
                for c in chapters {
                    let label = if c.title.is_empty() { "(untitled)" } else { &c.title };
                    lines.push(TextLine::from(format!(
                        "{}–{}  {label}",
                        crate::graph::format_time(c.start_secs),
                        crate::graph::format_time(c.end_secs)
                    )));
                }
            }
            let divider_width = rect.width.saturating_sub(2) as usize;
            lines.push(TextLine::styled("─".repeat(divider_width), Style::default().fg(Color::DarkGray)));
        }
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
            Target::OutputChapters(id) => match app.graph.outputs.iter().position(|o| o.id == id) {
                Some(oi) => format!("→ OUTPUT {} chapters", oi + 1),
                None => "→ (?)".to_string(),
            },
        };
        let mut style = Style::default();
        if focused && row == app.row_idx {
            style = style.add_modifier(Modifier::REVERSED);
        }
        lines.push(TextLine::styled(target_label, style));
    }

    let scroll = if focused { app.text_scroll } else { 0 };
    let title = format!(" [{}] ", node.kind.short_label());
    let inner_width = rect.width.saturating_sub(2);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            scroll_text(&title, title_scroll(&title, scroll, inner_width)),
            Style::default().fg(border_color).add_modifier(Modifier::BOLD),
        ));

    frame.render_widget(Paragraph::new(lines).block(block).scroll((0, scroll)), rect);
}

fn draw_output_node(frame: &mut Frame, canvas_area: Rect, app: &App, index: usize, node: &OutputNode) {
    let incoming = app.graph.incoming(Target::Output(node.id));
    let chapter_wires = app.graph.incoming(Target::OutputChapters(node.id));
    let rows =
        node_rows(output_body_rows(incoming.len(), !chapter_wires.is_empty()), extra_args_field_rows(&node.extra_args));
    let Some(rect) = node_rect(canvas_area, node.pos, node.width, rows) else {
        return;
    };
    let focused = matches!(app.focus, Focus::Output(oi) if oi == index);
    let border_color = if focused { Color::Yellow } else { Color::Cyan };

    let mut lines = Vec::new();
    if !node.extra_args.is_empty() {
        for (key, value) in &node.extra_args {
            let text = if value.is_empty() { format!("-{key}") } else { format!("-{key} {value}") };
            lines.push(TextLine::from(text));
        }
        let divider_width = rect.width.saturating_sub(2) as usize;
        lines.push(TextLine::styled("─".repeat(divider_width), Style::default().fg(Color::DarkGray)));
    }
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

    // The chapters slot only gets a row when something's actually
    // connected to it -- same as an unmapped video/audio stream doesn't
    // get a placeholder row of its own. When shown, it's always the last
    // row, right after the mapped streams (see `output_body_rows`/
    // `App::cycle_row`'s Output arm).
    if let Some(&wi) = chapter_wires.first() {
        let chapters_label = match describe_endpoint(&app.graph, app.graph.wires[wi].from) {
            Some(desc) => format!("chapters <- {desc}"),
            None => "chapters <- (?)".to_string(),
        };
        let mut chapters_style = Style::default().fg(Color::DarkGray);
        if focused && app.row_idx == incoming.len() {
            chapters_style = chapters_style.add_modifier(Modifier::REVERSED);
        }
        lines.push(TextLine::styled(chapters_label, chapters_style));
    }

    let container_tag = match &node.container {
        Some(name) => format!(" [{name}]"),
        None => String::new(),
    };
    let scroll = if focused { app.text_scroll } else { 0 };
    let title = format!(" OUTPUT {}: {}{container_tag} ", index + 1, node.path);
    let inner_width = rect.width.saturating_sub(2);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title(Span::styled(
            scroll_text(&title, title_scroll(&title, scroll, inner_width)),
            Style::default().fg(border_color).add_modifier(Modifier::BOLD),
        ));

    frame.render_widget(Paragraph::new(lines).block(block).scroll((0, scroll)), rect);
}

pub(super) fn kind_color(kind: StreamKind) -> Color {
    match kind {
        StreamKind::Video => Color::LightBlue,
        StreamKind::Audio => Color::LightGreen,
        StreamKind::Subtitle => Color::LightMagenta,
        StreamKind::Chapter => Color::LightYellow,
        StreamKind::Other => Color::Gray,
    }
}

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
        " tff — node-based ffmpeg  │  Tab focus  ↑↓ row  hjkl move  a add-node  o output-path  c arm/connect  d disconnect  e edit  f container  x delete-node  r render  p preview  q quit ",
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
        Mode::TextInput { target, input, .. } => {
            let buffer = input.value();
            let cursor = input.cursor();
            let prompt = match target {
                crate::app::TextTarget::NewInputPath => "add input file path: ".to_string(),
                crate::app::TextTarget::OutputPath(_) => "output file path: ".to_string(),
                crate::app::TextTarget::ModifierMetadataValue { key, .. } => format!("{key}: "),
                crate::app::TextTarget::ModifierCustomKey(_) => "custom metadata key: ".to_string(),
                crate::app::TextTarget::ModifierFilterValue { key, .. } => format!("{key}: "),
                crate::app::TextTarget::ExtraArgValue { key, .. } => format!("{key}: "),
                crate::app::TextTarget::ExtraArgCustomKey(_) => "custom extra-arg key: ".to_string(),
                crate::app::TextTarget::ChapterTime { field, .. } => match field {
                    crate::app::ChapterTimeField::Start => "start (HH:MM:SS or seconds): ".to_string(),
                    crate::app::ChapterTimeField::End => "end (HH:MM:SS or seconds): ".to_string(),
                },
                crate::app::TextTarget::ChapterTitle { .. } => "chapter title: ".to_string(),
            };
            // A reversed-video block over whatever's at the cursor (or a
            // blank block past the last character) shows the insertion
            // point in place, since it's no longer always the end of the
            // buffer -- a trailing blinking underscore would be
            // misleading once the cursor can sit mid-string.
            let byte_off = crate::app::char_byte_offset(buffer, cursor);
            let before = &buffer[..byte_off];
            let mut after_chars = buffer[byte_off..].chars();
            let mut spans = vec![
                Span::styled(prompt, Style::default().fg(Color::Yellow)),
                Span::raw(before.to_string()),
            ];
            match after_chars.next() {
                Some(c) => {
                    spans.push(Span::styled(c.to_string(), Style::default().add_modifier(Modifier::REVERSED)));
                    spans.push(Span::raw(after_chars.as_str().to_string()));
                }
                None => {
                    spans.push(Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)));
                }
            }
            TextLine::from(spans)
        }
        Mode::Picker { .. } => TextLine::from(Span::styled(
            "↑↓/jk move · / search · Enter select · Esc cancel",
            Style::default().fg(Color::Yellow),
        )),
        Mode::Normal => {
            if app.armed.len() == 1 {
                let ep = *app.armed.iter().next().expect("checked len == 1");
                match describe_endpoint(&app.graph, ep) {
                    Some(desc) => TextLine::from(Span::styled(
                        format!("armed {desc} — focus a modifier or output, press 'c' to connect (Esc to cancel)"),
                        Style::default().fg(Color::Yellow),
                    )),
                    None => TextLine::from(""),
                }
            } else if app.armed.len() > 1 {
                TextLine::from(Span::styled(
                    format!(
                        "armed {} ports — focus a modifier or output, press 'c' to connect (Esc to cancel)",
                        app.armed.len()
                    ),
                    Style::default().fg(Color::Yellow),
                ))
            } else if !app.selected.is_empty() {
                TextLine::from(Span::styled(
                    format!(
                        "{} port(s) selected — Space/Shift+↑↓ to adjust, 'c' to arm them (Esc to clear)",
                        app.selected.len()
                    ),
                    Style::default().fg(Color::Cyan),
                ))
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
fn extra_args_field_rows(extra_args: &std::collections::BTreeMap<String, String>) -> u16 {
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
fn compute_rects(app: &App, area: Rect) -> Vec<(NodeId, Rect)> {
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

fn rect_for(rects: &[(NodeId, Rect)], id: NodeId) -> Option<Rect> {
    rects.iter().find(|(rid, _)| *rid == id).map(|(_, r)| *r)
}

fn wire_color(graph: &Graph, from: Endpoint) -> Color {
    if graph.resolve_chapters(from).is_some() {
        return kind_color(StreamKind::Chapter);
    }
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
            Endpoint::Stream { node, stream_idx } => {
                let offset = app.graph.input(node).map(|n| 1 + extra_args_field_rows(&n.extra_args)).unwrap_or(1);
                (offset, stream_idx) // stream rows sit below the extra-args section, if any
            }
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
            Target::OutputChapters(id) => id,
        };
        let Some(dst_rect) = rect_for(&rects, dst_id) else { continue };
        let (dst_row_offset, dst_row) = match wire.to {
            Target::ModifierIn(mid) => {
                let offset = app.graph.modifier(mid).map(|m| modifier_incoming_row(&m.kind)).unwrap_or(1);
                (offset, 0u16) // a modifier only ever has one incoming row
            }
            Target::Output(id) => {
                let row = app.graph.incoming(wire.to).iter().position(|&wi| wi == wire_idx).unwrap_or(0);
                let offset = app.graph.output(id).map(|n| 1 + extra_args_field_rows(&n.extra_args)).unwrap_or(1);
                (offset, row as u16) // mapped rows sit below the extra-args section, if any
            }
            Target::OutputChapters(id) => {
                // Always the last row, right after every mapped-stream row
                // -- but the mapped section itself is never less than one
                // visual row (a "(nothing mapped)" placeholder takes that
                // row when there are no real wires), so this has to match
                // that floor too, not just the raw wire count (see
                // `output_body_rows`).
                let row = app.graph.incoming(Target::Output(id)).len().max(1);
                let offset = app.graph.output(id).map(|n| 1 + extra_args_field_rows(&n.extra_args)).unwrap_or(1);
                (offset, row as u16)
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

// --- Box-drawing junction merging ---------------------------------------
//
// In a dense layout, more than one wire can legitimately pass through the
// same buffer cell (e.g. one wire's vertical lane crossing another's
// horizontal run). Drawing each wire independently and just overwriting
// whatever was there left those spots ambiguous -- a lone "─" or "│" with
// no visual sign that something else also touches that cell, so "does
// this line actually connect here or does it just happen to end next to
// another one" wasn't answerable at a glance. Instead each of the four
// directions a glyph touches is a bit, cells are re-rendered from the OR
// of every wire that's passed through them so far, and the resulting bit
// set picks the matching box-drawing glyph -- so a real crossing renders
// as "┼", a corner grazed by a straight run becomes a "┬"/"┴"/"├"/"┤", and
// so on. Only a plain, untouched corner (exactly two bits) gets the
// rounded variant -- there's no rounded T-junction or cross in Unicode
// box drawing, so those fall back to the sharp glyph.
const LINE_UP: u8 = 0b0001;
const LINE_RIGHT: u8 = 0b0010;
const LINE_DOWN: u8 = 0b0100;
const LINE_LEFT: u8 = 0b1000;

/// Which directions a box-drawing glyph touches, as a bitset of the
/// constants above. Recognizes both the sharp corners this file's own
/// `match` arms name and the rounded ones `glyph_for_bits` renders, so
/// re-reading an already-merged cell keeps merging correctly instead of
/// losing track of what's there. Anything else (blank space, a node's own
/// border) isn't a wire segment, so it contributes no bits -- an isolated
/// wire still renders exactly as before.
fn line_bits(glyph: &str) -> u8 {
    match glyph {
        "─" => LINE_LEFT | LINE_RIGHT,
        "│" => LINE_UP | LINE_DOWN,
        "╭" | "┌" => LINE_RIGHT | LINE_DOWN,
        "╮" | "┐" => LINE_LEFT | LINE_DOWN,
        "╰" | "└" => LINE_UP | LINE_RIGHT,
        "╯" | "┘" => LINE_UP | LINE_LEFT,
        "┬" => LINE_LEFT | LINE_RIGHT | LINE_DOWN,
        "┴" => LINE_LEFT | LINE_RIGHT | LINE_UP,
        "├" => LINE_UP | LINE_DOWN | LINE_RIGHT,
        "┤" => LINE_UP | LINE_DOWN | LINE_LEFT,
        "┼" => LINE_UP | LINE_DOWN | LINE_LEFT | LINE_RIGHT,
        _ => 0,
    }
}

/// The box-drawing glyph for a set of touched directions. Every reachable
/// input has exactly 2, 3, or 4 bits set: `line_bits` always yields >= 2
/// bits, and OR-ing two such values can only ever add bits, never remove
/// them, so 0- or 1-bit results (and thus the fallback arm) never actually
/// occur -- it's there only so this stays total.
fn glyph_for_bits(bits: u8) -> &'static str {
    match bits {
        b if b == LINE_LEFT | LINE_RIGHT => "─",
        b if b == LINE_UP | LINE_DOWN => "│",
        b if b == LINE_RIGHT | LINE_DOWN => "╭",
        b if b == LINE_LEFT | LINE_DOWN => "╮",
        b if b == LINE_UP | LINE_RIGHT => "╰",
        b if b == LINE_UP | LINE_LEFT => "╯",
        b if b == LINE_LEFT | LINE_RIGHT | LINE_DOWN => "┬",
        b if b == LINE_LEFT | LINE_RIGHT | LINE_UP => "┴",
        b if b == LINE_UP | LINE_DOWN | LINE_RIGHT => "├",
        b if b == LINE_UP | LINE_DOWN | LINE_LEFT => "┤",
        _ => "┼",
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

    // Plot this wire's own path into a scratch buffer first, deduping by
    // cell (last write wins, same as the plain overwrite this used to do
    // straight into the screen buffer) -- a corner deliberately gets drawn
    // on top of the straight run it grows out of, and merging that
    // self-overwrite into the shared buffer's junction logic would
    // misread it as a second wire crossing itself, turning a clean corner
    // into a spurious T. Only once this wire's own path is fully decided
    // does each of its cells get merged into the screen buffer, where a
    // genuine overlap with a different wire still combines correctly.
    let mut path: Vec<(u16, u16, &'static str)> = Vec::new();
    let mut put = |x: u16, y: u16, s: &'static str| {
        if x < bounds.x || x >= bounds.right() || y < bounds.y || y >= bounds.bottom() {
            return;
        }
        match path.iter_mut().find(|(px, py, _)| *px == x && *py == y) {
            Some(cell) => cell.2 = s,
            None => path.push((x, y, s)),
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
        let top_corner = match (going_right, going_down) {
            (true, true) => "┐",
            (true, false) => "┘",
            (false, true) => "┌",
            (false, false) => "└",
        };
        let bottom_corner = match (going_right, going_down) {
            (true, true) => "└",
            (true, false) => "┌",
            (false, true) => "┘",
            (false, false) => "┐",
        };

        // Each of the three legs stops short of the corner cell it leads
        // into, so nothing later overwrites a corner glyph back into a
        // plain "─"/"│" -- every cell on the path is touched exactly once.
        let (a, b) = if sx <= mid { (sx, mid.saturating_sub(1)) } else { (mid + 1, sx) };
        if a <= b {
            for x in a..=b {
                put(x, sy, "─");
            }
        }
        put(mid, sy, top_corner);

        let (top, bottom) = if sy <= dy { (sy, dy) } else { (dy, sy) };
        if bottom > top + 1 {
            for y in (top + 1)..bottom {
                put(mid, y, "│");
            }
        }
        put(mid, dy, bottom_corner);

        let (c, d) = if mid <= dx { (mid + 1, dx) } else { (dx, mid.saturating_sub(1)) };
        if c <= d {
            for x in c..=d {
                put(x, dy, "─");
            }
        }
        (dy, c.min(mid), d.max(mid))
    };

    for (x, y, s) in path {
        let existing = buf.cell(Position::new(x, y)).map(|c| line_bits(c.symbol())).unwrap_or(0);
        let glyph = glyph_for_bits(existing | line_bits(s));
        buf.set_string(x, y, glyph, style);
    }

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
        StreamKind::Chapter => Color::LightYellow,
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

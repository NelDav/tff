use ratatui::buffer::Buffer;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::Frame;

use super::canvas::{compute_rects, extra_args_field_rows, kind_color, modifier_incoming_row, modifier_outgoing_start_row, rect_for};
use crate::app::App;
use crate::graph::{Codec, Endpoint, Graph, ModifierKind, Target};

pub(super) fn wire_color(graph: &Graph, from: Endpoint) -> Color {
    if graph.resolve_chapters(from).is_some() {
        return kind_color(crate::graph::StreamKind::Chapter);
    }
    graph
        .resolve(from)
        .and_then(|r| graph.resolved_stream_kind(&r))
        .map(kind_color)
        .unwrap_or(Color::DarkGray)
}

/// Draws each connection as an orthogonal wire (one or two `─` runs joined
/// by a `│` and box-drawing corners when source and destination rows
/// differ), colored by the stream kind resolved at its ultimate source.
/// Attaches one cell outside each node's border so it never overlaps the
/// box the node widgets draw later. A wire leaving a non-Copy Convert
/// node's output is tagged with that node's codec right where it happens.
pub(super) fn draw_edges(frame: &mut Frame, app: &App, area: Rect) {
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
                let segment_count = app.graph.incoming(Target::ModifierIn(mid)).len();
                let offset =
                    app.graph.modifier(mid).map(|m| modifier_outgoing_start_row(&m.kind, segment_count)).unwrap_or(2);
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
                // Every modifier kind except Concat only ever has one
                // incoming wire, always landing on the section's first row;
                // Concat can have any number of segments, so the row is
                // that wire's position among them, same as an output's
                // mapped-stream rows below.
                let row = app.graph.incoming(wire.to).iter().position(|&wi| wi == wire_idx).unwrap_or(0);
                (offset, row as u16)
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
    // cell (last write wins) -- a corner deliberately gets drawn on top of
    // the straight run it grows out of, and merging that self-overwrite
    // into the shared buffer's junction logic would misread it as a
    // second wire crossing itself, turning a clean corner into a spurious
    // T. Only once this wire's own path is fully decided does each of its
    // cells get merged into the screen buffer, where a genuine overlap
    // with a different wire still combines correctly.
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

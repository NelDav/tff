mod canvas;
mod popups;
mod wires;

pub(crate) use canvas::{input_node_text_extent, modifier_node_text_extent, output_node_text_extent};
// Internal callers reach this through `popups::suggestion_label` directly;
// this re-export exists solely so tests can call it as
// `crate::ui::suggestion_label`, which -- being outside `#[cfg(test)]` --
// a plain `cargo build` can't see using either path.
#[allow(unused_imports)]
pub(crate) use popups::suggestion_label;

use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::app::{App, Mode};
use crate::graph::{Endpoint, Graph};

/// The log pane's fixed height (including its own border rows) -- shared
/// between the root layout below, `draw_log`'s scroll-clamping, and
/// `App::scroll_log`'s, so a future change to one can't silently desync
/// from the others.
pub(crate) const LOG_PANE_HEIGHT: u16 = 10;

pub fn draw(frame: &mut Frame, app: &App) {
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title / hotkeys
            Constraint::Min(10),   // node canvas
            Constraint::Length(1), // status / text-input line
            Constraint::Length(LOG_PANE_HEIGHT), // log pane
        ])
        .split(frame.area());

    draw_header(frame, root[0]);
    canvas::draw_graph(frame, app, root[1]);
    draw_status_line(frame, app, root[2]);
    draw_log(frame, app, root[3]);
    popups::draw_picker_popup(frame, app);
    popups::draw_chapter_table_popup(frame, app);
    popups::draw_suggestions_popup(frame, app, root[2]);
}

fn draw_header(frame: &mut Frame, area: Rect) {
    let line = TextLine::from(
        " tff — node-based ffmpeg  │  Tab focus  ↑↓ row  hjkl move  ←→ scroll text  Shift+←→ resize  a add-node  o output-path  c arm/connect  d disconnect  e edit  f container  x delete-node  r render  p preview  q quit ",
    )
    .style(Style::default().fg(Color::Black).bg(Color::Cyan));
    frame.render_widget(Paragraph::new(line), area);
}

pub(crate) fn describe_endpoint(graph: &Graph, ep: Endpoint) -> Option<String> {
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
                crate::app::TextTarget::ExtraArgValue { target, key } => {
                    format!("{}: ", crate::app::extra_arg_label(*target, key))
                }
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
        Mode::ChapterTable { .. } => TextLine::from(Span::styled(
            "↑↓/jk row · ←→/hl/Tab column · Enter edit/add · d delete · Esc close",
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

fn draw_log(frame: &mut Frame, app: &App, area: Rect) {
    let (start, end) = app.visible_log_range();
    let lines: Vec<TextLine> = app.log[start..end].iter().map(|l| TextLine::from(l.as_str())).collect();
    let title = match (app.log_scroll.is_some(), app.log_hscroll > 0) {
        (true, true) => " log (scrolled -- PgDn/Ctrl+Left to catch up) ".to_string(),
        (true, false) => " log (scrolled -- PgDn to catch up) ".to_string(),
        (false, true) => " log (scrolled right -- Ctrl+Left to catch up) ".to_string(),
        (false, false) => " log ".to_string(),
    };
    let block = Block::default().borders(Borders::ALL).title(title);
    frame.render_widget(Paragraph::new(lines).block(block).scroll((0, app.log_hscroll)), area);
}

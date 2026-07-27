use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line as TextLine, Span};
use ratatui::widgets::{Block, BorderType, Borders, Cell, Clear, Paragraph, Row, Table};
use ratatui::Frame;

use crate::app::{App, ChapterColumn, Mode};
use crate::graph::ModifierKind;

/// Floating, centered, scrollable option list -- the dropdown for codec,
/// container, and new-modifier selection. Drawn last so it sits on top of
/// everything else. Supports a vim-like `/` search that filters the list
/// live as you type.
pub(super) fn draw_picker_popup(frame: &mut Frame, app: &App) {
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

/// Renders a `ChapterEdit` modifier's chapter list as a real table --
/// start/end/title columns, one row per chapter -- with the currently
/// selected cell highlighted, plus a trailing "add chapter" row
/// highlighted as a whole when it's the one selected. Direct row/column
/// navigation means editing a field is one Enter press away.
pub(super) fn draw_chapter_table_popup(frame: &mut Frame, app: &App) {
    let Mode::ChapterTable { modifier, row, col } = &app.mode else {
        return;
    };
    let Some(ModifierKind::ChapterEdit { chapters }) = app.graph.modifier(*modifier).map(|m| &m.kind) else {
        return;
    };

    let area = frame.area();
    let popup_width = area.width.saturating_sub(4).clamp(30, 70);
    let popup_height = (chapters.len() as u16 + 4).clamp(5, 20).min(area.height.saturating_sub(2));
    let popup = centered_rect(popup_width, popup_height, area);

    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Yellow))
        .title(Span::styled(
            " chapters ",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Span::styled(
            " ↑↓/jk row · ←→/hl/Tab column · Enter edit/add · d delete · Esc close ",
            Style::default().fg(Color::DarkGray),
        ));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let selected_style = Style::default().fg(Color::Black).bg(Color::Yellow).add_modifier(Modifier::BOLD);
    let cell_style = |r: usize, c: ChapterColumn| if r == *row && c == *col { selected_style } else { Style::default() };

    let header = Row::new(["start", "end", "title"]).style(Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD));

    let mut rows: Vec<Row> = chapters
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let title = if c.title.is_empty() { "(untitled)".to_string() } else { c.title.clone() };
            let title = if c.imported { format!("{title} [imported]") } else { title };
            Row::new([
                Cell::from(crate::graph::format_time(c.start_secs)).style(cell_style(i, ChapterColumn::Start)),
                Cell::from(crate::graph::format_time(c.end_secs)).style(cell_style(i, ChapterColumn::End)),
                Cell::from(title).style(cell_style(i, ChapterColumn::Title)),
            ])
        })
        .collect();

    // The label goes in the wide title column, not the fixed-width start
    // column, so it isn't truncated to 10 characters.
    let add_style = if *row == chapters.len() { selected_style } else { Style::default().fg(Color::Green) };
    rows.push(Row::new([
        Cell::from("").style(add_style),
        Cell::from("").style(add_style),
        Cell::from("+ add chapter…").style(add_style),
    ]));

    let table = Table::new(rows, [Constraint::Length(10), Constraint::Length(10), Constraint::Min(10)]).header(header);
    frame.render_widget(table, inner);
}

pub(super) fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
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
pub(super) fn draw_suggestions_popup(frame: &mut Frame, app: &App, status_area: Rect) {
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
            TextLine::styled(format!(" {}", suggestion_label(s)), style)
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Every suggestion returned for one `path_suggestions` call shares the
/// same directory prefix (they're all entries from the same `read_dir`
/// call) -- so within a single popup listing, that prefix never actually
/// distinguishes one candidate from another, and showing it just pushes
/// the one part that *does* differ (the file/dir's own name) further right,
/// off the edge of a long input's popup entirely. This shows only that
/// trailing name (keeping a directory's own trailing '/' marker), while
/// `s` itself -- the full path -- stays what actually gets written into
/// the buffer on accept (see `App::text_input_accept_suggestion`).
pub(crate) fn suggestion_label(s: &str) -> String {
    let (body, trailing_slash) = match s.strip_suffix('/') {
        Some(rest) => (rest, "/"),
        None => (s, ""),
    };
    let name = body.rsplit('/').next().unwrap_or(body);
    format!("{name}{trailing_slash}")
}

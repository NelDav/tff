mod app;
mod ffmpeg;
mod graph;
#[cfg(test)]
mod tests;
mod ui;

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use app::{App, Mode};

fn main() -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    let result = run(&mut terminal);
    ratatui::restore();
    result
}

fn run(terminal: &mut ratatui::DefaultTerminal) -> anyhow::Result<()> {
    let mut app = App::new();

    loop {
        app.poll_ffmpeg();
        terminal.draw(|frame| ui::draw(frame, &app))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                handle_key(&mut app, key);
            }

        if app.should_quit {
            break;
        }
    }
    Ok(())
}

fn handle_key(app: &mut App, key: KeyEvent) {
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        app.should_quit = true;
        return;
    }

    match &app.mode {
        Mode::TextInput { .. } => match key.code {
            KeyCode::Enter => app.confirm_text_input(),
            KeyCode::Esc => app.cancel_text_input(),
            KeyCode::Backspace => app.text_input_backspace(),
            KeyCode::Tab => app.text_input_accept_suggestion(),
            KeyCode::Up => app.text_input_move_suggestion(-1),
            KeyCode::Down => app.text_input_move_suggestion(1),
            KeyCode::Char(c) => app.text_input_char(c),
            _ => {}
        },
        Mode::Picker { searching: true, .. } => match key.code {
            KeyCode::Enter => app.picker_confirm_search(),
            KeyCode::Esc => app.picker_escape(),
            KeyCode::Backspace => app.picker_search_backspace(),
            KeyCode::Char(c) => app.picker_search_char(c),
            _ => {}
        },
        Mode::Picker { .. } => match key.code {
            KeyCode::Up | KeyCode::Char('k') => app.picker_move(-1),
            KeyCode::Down | KeyCode::Char('j') => app.picker_move(1),
            KeyCode::Char('/') => app.picker_start_search(),
            KeyCode::Enter => app.picker_confirm(),
            KeyCode::Esc => app.picker_escape(),
            _ => {}
        },
        Mode::Normal => match key.code {
            KeyCode::Char('q') => app.should_quit = true,
            KeyCode::Tab => app.cycle_focus(true),
            KeyCode::BackTab => app.cycle_focus(false),
            KeyCode::Up => app.cycle_row(false),
            KeyCode::Down => app.cycle_row(true),
            KeyCode::Char('h') => app.move_focused_node(-1.0, 0.0),
            KeyCode::Char('l') => app.move_focused_node(1.0, 0.0),
            KeyCode::Char('k') => app.move_focused_node(0.0, -1.0),
            KeyCode::Char('j') => app.move_focused_node(0.0, 1.0),
            KeyCode::Char('a') => app.start_add_input(),
            KeyCode::Char('O') => app.add_output_node(),
            KeyCode::Char('o') => app.start_edit_output(),
            KeyCode::Char('m') => app.open_add_modifier_picker(),
            KeyCode::Char('c') => app.toggle_connect(),
            KeyCode::Char('d') => app.disconnect_focused(),
            KeyCode::Char('e') => app.activate_modifier(),
            KeyCode::Char('f') => app.open_container_picker(),
            KeyCode::Char('x') => app.delete_focused_node(),
            KeyCode::Char('r') => app.start_render(),
            KeyCode::Esc => app.armed = None,
            _ => {}
        },
    }
}

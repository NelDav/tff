mod app;
mod ffmpeg;
mod graph;
#[cfg(test)]
mod tests;
mod ui;

use std::io::stdout;
use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;

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
        if let Some(path) = app.preview_ready.take() {
            play_preview(terminal, &mut app, &path);
        }
        // A headless scrub session (mpv rendering `--vo=tct` into this same
        // terminal, no display available -- see `App::start_scrub`) needs
        // the whole terminal to itself for as long as it's open, checked
        // here (before the draw call below) rather than after `handle_key`,
        // so the iteration that just spawned it never tries to draw
        // ratatui's own frame over it.
        if app.scrub_is_headless_mpv() {
            run_headless_scrub(terminal, &mut app)?;
            continue;
        }
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

/// Drives a headless scrub session's keys directly, bypassing the normal
/// draw-then-read-one-key cycle above: mpv is writing `--vo=tct` video
/// frames straight to this same terminal (see `ffmpeg::spawn_scrub_mpv`),
/// so ratatui can't also be repainting it as a TUI without the two
/// fighting over the same screen -- this leaves the alternate screen for
/// the duration (letting mpv's frames show through) while still reading
/// keys itself (raw mode stays enabled throughout, unlike `play_preview`'s
/// fully-blocking mpv fallback, since `--input-terminal=no` means mpv
/// isn't reading this terminal's input at all -- every keystroke is tff's
/// to relay over the IPC socket, exactly like the windowed mpv/ffplay path
/// already does through `dispatch_scrub_key`). Returns once the session
/// closes (Esc/'q', or Ctrl+C requesting quit).
fn run_headless_scrub(terminal: &mut ratatui::DefaultTerminal, app: &mut App) -> anyhow::Result<()> {
    execute!(stdout(), LeaveAlternateScreen)?;
    loop {
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                    app.should_quit = true;
                    app.close_scrub();
                } else {
                    dispatch_scrub_key(app, key);
                }
            }
        if app.should_quit || !matches!(app.mode, Mode::Scrub) {
            break;
        }
    }
    Ok(resume_tui(terminal)?)
}

/// Plays a finished preview render. mpv is the preferred player when it's
/// installed (proper playback controls, in its own window), with ffplay as
/// the fallback otherwise; both open their own window and are left running
/// detached, so the TUI keeps going undisturbed underneath. With no display
/// to open a window on at all (a bare SSH session with no X forwarding),
/// there's nowhere for either to draw one, so this falls back to mpv's
/// terminal video output (`--vo=tct`) directly in this terminal instead --
/// which means yielding the TUI's alternate screen for the duration (mpv
/// draws straight to this process's own stdout) and forcing a full repaint
/// once it's done. ffplay has no such terminal mode, so a missing mpv with
/// no display leaves nothing left to try.
fn play_preview(terminal: &mut ratatui::DefaultTerminal, app: &mut App, path: &str) {
    let mpv_installed = ffmpeg::mpv_is_installed();

    if ffmpeg::has_display() {
        if mpv_installed {
            app.log.push(format!("$ mpv {path}"));
            if let Err(e) = ffmpeg::play_mpv(path) {
                app.status = format!("couldn't launch mpv: {e:#}");
                app.log.push(app.status.clone());
            }
        } else {
            app.log.push(format!("$ ffplay {path}"));
            if let Err(e) = ffmpeg::play(path) {
                app.status = format!("couldn't launch ffplay: {e:#}");
                app.log.push(app.status.clone());
            }
        }
        return;
    }

    if !mpv_installed {
        app.status = "no display available and mpv isn't installed -- can't play the preview".to_string();
        app.log.push(app.status.clone());
        return;
    }

    app.log.push(format!("no display available -- playing in the terminal via mpv: {path}"));
    if let Err(e) = suspend_tui() {
        app.status = format!("couldn't suspend the TUI to play the preview: {e}");
        app.log.push(app.status.clone());
        return;
    }
    let result = ffmpeg::play_in_terminal(path);
    if let Err(e) = resume_tui(terminal) {
        // The TUI's own terminal state may now be inconsistent, but this is
        // already reported to stderr by the resume attempt itself; nothing
        // more productive to do than keep going with whatever draws next.
        app.log.push(format!("couldn't restore the TUI after mpv: {e}"));
    }
    if let Err(e) = result {
        app.status = format!("couldn't play preview via mpv: {e:#}");
        app.log.push(app.status.clone());
    }
}

/// Leaves the TUI's alternate screen and disables raw mode so an external
/// terminal-drawing process (mpv, via `--vo=tct`) can take over this
/// process's stdout, the same way a shell suspends its own terminal
/// handling for `$EDITOR`.
fn suspend_tui() -> std::io::Result<()> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    Ok(())
}

/// Reverses `suspend_tui` and forces a full repaint on the next `draw`,
/// since whatever mpv left on the primary screen buffer has no relation
/// to ratatui's last-known buffer state.
///
/// Deliberately uses `resize` rather than `Terminal::clear`: `clear`
/// queries the backend's cursor position first (to restore it afterward),
/// which sends a DSR escape sequence and blocks waiting for the terminal's
/// reply -- verified to time out under some pty conditions (a genuine "The
/// cursor position could not be read within a normal duration" error),
/// which would abort the clear entirely, leaving stale content on screen.
/// `resize` forces the same full-repaint side effect for a fullscreen
/// viewport (the only kind `ratatui::init()`'s `DefaultTerminal` uses)
/// without touching cursor position at all.
fn resume_tui(terminal: &mut ratatui::DefaultTerminal) -> std::io::Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let area = terminal.size()?.into();
    terminal.resize(area)
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
            KeyCode::Tab => app.text_input_accept_suggestion(),
            KeyCode::Up => app.text_input_move_suggestion(-1),
            KeyCode::Down => app.text_input_move_suggestion(1),
            // Everything else (typing, Backspace/Delete, Left/Right,
            // Home/End, ...) goes straight to tui-input's own key
            // handling -- see `App::text_input_handle_key`.
            _ => app.text_input_handle_key(key),
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
        Mode::ChapterTable { .. } => match key.code {
            KeyCode::Up | KeyCode::Char('k') => app.chapter_table_move_row(false),
            KeyCode::Down | KeyCode::Char('j') => app.chapter_table_move_row(true),
            KeyCode::Left | KeyCode::Char('h') => app.chapter_table_move_col(false),
            KeyCode::Right | KeyCode::Char('l') => app.chapter_table_move_col(true),
            KeyCode::Tab => app.chapter_table_tab(true),
            KeyCode::BackTab => app.chapter_table_tab(false),
            KeyCode::Enter => app.chapter_table_confirm(),
            KeyCode::Char('d') => app.chapter_table_delete(),
            KeyCode::Esc => app.chapter_table_close(),
            _ => {}
        },
        Mode::Normal => match key.code {
            KeyCode::Char('q') => app.should_quit = true,
            KeyCode::Tab => app.cycle_focus(true),
            KeyCode::BackTab => app.cycle_focus(false),
            // Ctrl+Up/Down reorders the hovered output row; Shift+Up/Down
            // extends a port-selection range; both checked ahead of the
            // plain Up/Down arms below since match picks the first hit.
            KeyCode::Up if key.modifiers.contains(KeyModifiers::CONTROL) => app.move_focused_row(false),
            KeyCode::Down if key.modifiers.contains(KeyModifiers::CONTROL) => app.move_focused_row(true),
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => app.extend_port_selection(false),
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => app.extend_port_selection(true),
            KeyCode::Up => app.cycle_row(false),
            KeyCode::Down => app.cycle_row(true),
            // Ctrl+Left/Right scrolls the log pane horizontally; Shift+Left/
            // Right resizes the focused node's right edge; plain Left/Right
            // scrolls its (possibly truncated) text instead -- the
            // Ctrl/Shift arms are checked first since match picks the
            // first hit.
            KeyCode::Right if key.modifiers.contains(KeyModifiers::CONTROL) => app.scroll_log_horizontal(true),
            KeyCode::Left if key.modifiers.contains(KeyModifiers::CONTROL) => app.scroll_log_horizontal(false),
            KeyCode::Right if key.modifiers.contains(KeyModifiers::SHIFT) => app.resize_focused_node(true),
            KeyCode::Left if key.modifiers.contains(KeyModifiers::SHIFT) => app.resize_focused_node(false),
            KeyCode::Right => app.scroll_node_text(true),
            KeyCode::Left => app.scroll_node_text(false),
            KeyCode::Char(' ') => app.toggle_port_selection(),
            // Ctrl+A selects every port on the focused node; checked ahead
            // of the plain 'a' arm below (add-node picker) since match
            // picks the first hit.
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => app.select_all_ports(),
            KeyCode::Char('h') => app.move_focused_node(-1.0, 0.0),
            KeyCode::Char('l') => app.move_focused_node(1.0, 0.0),
            KeyCode::Char('k') => app.move_focused_node(0.0, -1.0),
            KeyCode::Char('j') => app.move_focused_node(0.0, 1.0),
            KeyCode::Char('a') => app.open_add_node_picker(),
            KeyCode::Char('o') => app.start_edit_output(),
            KeyCode::Char('c') => app.toggle_connect(),
            KeyCode::Char('d') => app.disconnect_focused(),
            KeyCode::Char('e') => app.activate_focused(),
            KeyCode::Char('f') => app.open_container_picker(),
            KeyCode::Char('x') => app.delete_focused_node(),
            KeyCode::Char('r') => app.start_render(),
            KeyCode::Char('p') => app.start_preview(),
            KeyCode::Char('s') => app.start_scrub(),
            KeyCode::PageUp => app.scroll_log(false),
            KeyCode::PageDown => app.scroll_log(true),
            KeyCode::Esc => {
                app.armed.clear();
                app.selected.clear();
            }
            _ => {}
        },
        Mode::Scrub => dispatch_scrub_key(app, key),
    }
}

/// Every `Mode::Scrub` key, shared between the normal `handle_key` dispatch
/// above (the windowed mpv/ffplay case) and `run_headless_scrub`'s own
/// loop (the headless mpv case) -- the keys mean the same thing either way,
/// only how the surrounding terminal is being driven differs.
fn dispatch_scrub_key(app: &mut App, key: KeyEvent) {
    match key.code {
        // Shift+Left/Right is checked ahead of the plain arms below since
        // match picks the first hit, same as Normal mode's own
        // Ctrl/Shift-then-plain ordering.
        KeyCode::Right if key.modifiers.contains(KeyModifiers::SHIFT) => app.scrub_seek_relative(true),
        KeyCode::Left if key.modifiers.contains(KeyModifiers::SHIFT) => app.scrub_seek_relative(false),
        KeyCode::Right | KeyCode::Char('l') => app.scrub_step_frame(true),
        KeyCode::Left | KeyCode::Char('h') => app.scrub_step_frame(false),
        KeyCode::Char(' ') => app.scrub_play_pause(),
        KeyCode::Char('g') => app.start_scrub_seek(),
        KeyCode::Char('i') => app.mark_scrub_point("start"),
        KeyCode::Char('o') => app.mark_scrub_point("end"),
        KeyCode::Esc | KeyCode::Char('q') => app.close_scrub(),
        _ => {}
    }
}

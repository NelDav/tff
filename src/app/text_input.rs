use crossterm::event::{Event as CrosstermEvent, KeyEvent};
use tui_input::Input;
use tui_input::backend::crossterm::EventHandler;

use super::chapters::chapter_edit_chapters_mut;
use super::picker::{extra_args_of, extra_args_of_mut};
use super::{App, ChapterColumn, ChapterTimeField, Focus, Mode, TextTarget};
use crate::ffmpeg;
use crate::graph::{FilterName, ModifierKind};

/// Paths typed into the text field are passed straight to `ffprobe`/`ffmpeg`
/// via `Command`, with no shell in between — so `~` never gets expanded and
/// stray wrapping quotes (common when pasting from a file manager) are taken
/// literally. Clean those up here, once, at the point of entry.
fn clean_path_input(raw: &str) -> String {
    let mut s = raw.trim();
    if s.len() >= 2 {
        let bytes = s.as_bytes();
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'\'' && last == b'\'') || (first == b'"' && last == b'"') {
            s = &s[1..s.len() - 1];
        }
    }
    expand_tilde(s)
}

/// Expands a leading `~` or `~/...` to `$HOME`. Anything else is returned
/// unchanged (including a bare relative path, which is left for the OS to
/// resolve against the process's own working directory).
fn expand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Ok(home) = std::env::var("HOME") {
            return format!("{home}/{rest}");
        }
    } else if s == "~"
        && let Ok(home) = std::env::var("HOME")
    {
        return home;
    }
    s.to_string()
}

/// Builds a `Mode::TextInput` with the cursor placed at the end of
/// `buffer` -- the natural starting position whether the field opens empty
/// or prefilled with an existing value (e.g. re-editing a metadata field).
pub(super) fn text_input_mode(
    target: TextTarget,
    buffer: String,
    suggestions: Vec<String>,
) -> Mode {
    Mode::TextInput {
        target,
        input: Input::new(buffer),
        suggestions,
        selected: 0,
    }
}

/// The byte offset in `s` where its `char_idx`-th character starts --
/// `tui_input::Input::cursor()` returns a char index (safe for multi-byte
/// UTF-8), but rendering the cursor (see `ui::draw_status_line`) needs a
/// byte offset to split the string around it. `char_idx ==
/// s.chars().count()` (one past the last character, the end-of-buffer
/// position) falls through to `s.len()`, since there's no char at that
/// index to report a start byte for.
pub(crate) fn char_byte_offset(s: &str, char_idx: usize) -> usize {
    s.char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len())
}

/// Files/directories matching what's typed after the last '/' in `buffer`,
/// each returned as a full replacement value for the buffer (so accepting
/// one is just `buffer = suggestion`). Directories get a trailing '/' so
/// the user can keep completing deeper. Mirrors familiar shell completion:
/// entries are listed relative to whatever the user already typed (so `~`
/// notation is preserved in the buffer even though listing itself needs
/// the expanded path), sorted alphabetically, and dotfiles are hidden
/// unless the user is already typing a dot-prefix.
pub fn path_suggestions(buffer: &str) -> Vec<String> {
    // A bare "~" (no '/' yet) should offer the home directory's contents,
    // same as "~/" -- normalize once and recurse rather than duplicating
    // the split/scan logic for that one case.
    if buffer.starts_with('~') && !buffer.contains('/') {
        return path_suggestions(&format!("~/{}", &buffer[1..]));
    }

    let (dir_part, prefix) = match buffer.rfind('/') {
        Some(idx) => (&buffer[..idx + 1], &buffer[idx + 1..]),
        None => ("", buffer),
    };

    let scan_target = if dir_part.is_empty() {
        ".".to_string()
    } else {
        expand_tilde(dir_part)
    };
    let Ok(read_dir) = std::fs::read_dir(&scan_target) else {
        return Vec::new();
    };

    let show_hidden = prefix.starts_with('.');
    let mut entries: Vec<(String, bool)> = read_dir
        .filter_map(|e| e.ok())
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(prefix) || (name.starts_with('.') && !show_hidden) {
                return None;
            }
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            Some((name, is_dir))
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    entries
        .into_iter()
        .map(|(name, is_dir)| format!("{dir_part}{name}{}", if is_dir { "/" } else { "" }))
        .collect()
}

impl App {
    /// Esc while typing: discards the buffer and, for every other text
    /// target, drops straight back to Normal. A chapter's start/end/title
    /// is the one exception -- it was reached by drilling into the
    /// chapter table (see `Mode::ChapterTable`), not straight from Normal,
    /// so cancelling just the text edit should land back in that table at
    /// the same cell rather than throwing away the whole chapter-editing
    /// session.
    pub fn cancel_text_input(&mut self) {
        let Mode::TextInput { target, .. } = &self.mode else {
            self.mode = Mode::Normal;
            return;
        };
        self.mode = match target {
            TextTarget::ChapterTime {
                modifier,
                index,
                field,
            } => {
                let col = match field {
                    ChapterTimeField::Start => ChapterColumn::Start,
                    ChapterTimeField::End => ChapterColumn::End,
                };
                Mode::ChapterTable {
                    modifier: *modifier,
                    row: *index,
                    col,
                }
            }
            TextTarget::ChapterTitle { modifier, index } => Mode::ChapterTable {
                modifier: *modifier,
                row: *index,
                col: ChapterColumn::Title,
            },
            TextTarget::ScrubSeek => Mode::Scrub,
            _ => Mode::Normal,
        };
    }

    pub fn confirm_text_input(&mut self) {
        let Mode::TextInput { target, input, .. } = std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            return;
        };
        let buffer = input.value().to_string();
        match target {
            TextTarget::NewInputPath => {
                let path = clean_path_input(&buffer);
                if path.is_empty() {
                    return;
                }
                match ffmpeg::probe(&path) {
                    Ok(result) => {
                        let chapter_count = result.chapters.len();
                        let id =
                            self.graph
                                .add_input(path.clone(), result.streams, result.chapters);
                        self.log.push(format!("added input: {path}"));
                        if chapter_count > 0 {
                            self.log
                                .push(format!("found {chapter_count} chapter(s) in {path}"));
                        }
                        let idx = self.graph.inputs.len() - 1;
                        debug_assert_eq!(self.graph.inputs[idx].id, id);
                        self.set_focus_index(idx);
                    }
                    Err(e) => {
                        self.log.push(format!("error probing '{path}': {e}"));
                    }
                }
            }
            TextTarget::OutputPath(output_id) => {
                let path = clean_path_input(&buffer);
                if !path.is_empty()
                    && let Some(node) = self.graph.output_mut(output_id)
                {
                    node.path = path;
                }
            }
            TextTarget::ModifierMetadataValue { modifier, key } => {
                let value = buffer.trim().to_string();
                if let Some(m) = self.graph.modifier_mut(modifier)
                    && let ModifierKind::Metadata { fields } = &mut m.kind
                {
                    if value.is_empty() {
                        fields.remove(&key);
                        self.log.push(format!("{key} cleared"));
                    } else {
                        fields.insert(key.clone(), value.clone());
                        self.log.push(format!("{key} set to {value}"));
                    }
                }
            }
            TextTarget::ModifierCustomKey(modifier) => {
                let key = buffer.trim().to_string();
                if key.is_empty() {
                    return;
                }
                let current = self
                    .graph
                    .modifier(modifier)
                    .and_then(|m| match &m.kind {
                        ModifierKind::Metadata { fields } => fields.get(&key).cloned(),
                        ModifierKind::Convert(_)
                        | ModifierKind::Disposition { .. }
                        | ModifierKind::Filter { .. }
                        | ModifierKind::ChapterEdit { .. }
                        | ModifierKind::Concat => None,
                    })
                    .unwrap_or_default();
                self.mode = text_input_mode(
                    TextTarget::ModifierMetadataValue { modifier, key },
                    current,
                    Vec::new(),
                );
            }
            TextTarget::ModifierFilterValue { modifier, key } => {
                let value = buffer.trim().to_string();
                if let Some(m) = self.graph.modifier_mut(modifier)
                    && let ModifierKind::Filter { name, fields } = &mut m.kind
                {
                    // Trim is the only Filter kind with time-valued fields.
                    let is_trim_time_field =
                        *name == FilterName::Trim && (key == "start" || key == "end");
                    if value.is_empty() {
                        fields.remove(&key);
                        self.log.push(format!("{key} cleared"));
                    } else if is_trim_time_field {
                        match crate::graph::parse_time(&value) {
                            Some(secs) => {
                                fields.insert(key.clone(), secs.to_string());
                                self.log.push(format!(
                                    "{key} set to {}",
                                    crate::graph::format_time(secs)
                                ));
                            }
                            None => {
                                self.log.push(format!(
                                    "couldn't parse '{value}' as a time -- try seconds (12.5) or HH:MM:SS"
                                ));
                            }
                        }
                    } else {
                        fields.insert(key.clone(), value.clone());
                        self.log.push(format!("{key} set to {value}"));
                    }
                }
            }
            TextTarget::ExtraArgValue { target, key } => {
                let value = buffer.trim().to_string();
                if let Some(fields) = extra_args_of_mut(&mut self.graph, target) {
                    if value.is_empty() {
                        fields.remove(&key);
                        self.log.push(format!("{key} cleared"));
                    } else {
                        fields.insert(key.clone(), value.clone());
                        self.log.push(format!("{key} set to {value}"));
                    }
                }
            }
            TextTarget::ExtraArgCustomKey(target) => {
                let key = buffer.trim().to_string();
                if key.is_empty() {
                    return;
                }
                let current = extra_args_of(&self.graph, target)
                    .and_then(|f| f.get(&key).cloned())
                    .unwrap_or_default();
                self.mode = text_input_mode(
                    TextTarget::ExtraArgValue { target, key },
                    current,
                    Vec::new(),
                );
            }
            TextTarget::ChapterTime {
                modifier,
                index,
                field,
            } => {
                match crate::graph::parse_time(&buffer) {
                    Some(secs) => {
                        if let Some(chapter) = chapter_edit_chapters_mut(&mut self.graph, modifier)
                            .and_then(|cs| cs.get_mut(index))
                        {
                            match field {
                                ChapterTimeField::Start => chapter.start_secs = secs,
                                ChapterTimeField::End => chapter.end_secs = secs,
                            }
                        }
                        self.log.push(format!(
                            "chapter time set to {}",
                            crate::graph::format_time(secs)
                        ));
                    }
                    None => {
                        self.log.push(format!(
                            "couldn't parse '{}' as a time -- try seconds (12.5) or HH:MM:SS",
                            buffer.trim()
                        ));
                    }
                }
                // Return to the table at the same cell either way, so a
                // mistyped time doesn't lose the user's place.
                let col = match field {
                    ChapterTimeField::Start => ChapterColumn::Start,
                    ChapterTimeField::End => ChapterColumn::End,
                };
                self.mode = Mode::ChapterTable {
                    modifier,
                    row: index,
                    col,
                };
            }
            TextTarget::ChapterTitle { modifier, index } => {
                if let Some(chapter) = chapter_edit_chapters_mut(&mut self.graph, modifier)
                    .and_then(|cs| cs.get_mut(index))
                {
                    chapter.title = buffer.trim().to_string();
                }
                self.log.push("chapter title set".to_string());
                self.mode = Mode::ChapterTable {
                    modifier,
                    row: index,
                    col: ChapterColumn::Title,
                };
            }
            TextTarget::ScrubSeek => {
                match crate::graph::parse_time(&buffer) {
                    Some(secs) => self.scrub_seek_absolute(secs),
                    None => {
                        self.log.push(format!(
                            "couldn't parse '{}' as a time -- try seconds (12.5) or HH:MM:SS",
                            buffer.trim()
                        ));
                    }
                }
                self.mode = Mode::Scrub;
            }
            TextTarget::SaveProjectPath => {
                let path = clean_path_input(&buffer);
                if path.is_empty() {
                    return;
                }
                match crate::project::save(&self.graph, &path) {
                    Ok(()) => self.log.push(format!("saved project to {path}")),
                    Err(e) => self.log.push(format!("couldn't save project: {e:#}")),
                }
            }
            TextTarget::LoadProjectPath => {
                let path = clean_path_input(&buffer);
                if path.is_empty() {
                    return;
                }
                match crate::project::load(&path) {
                    Ok(result) => {
                        self.graph = result.graph;
                        self.focus = Focus::Output(0);
                        self.row_idx = 0;
                        self.armed.clear();
                        self.selected.clear();
                        self.selection_anchor = None;
                        self.text_scroll = 0;
                        self.log.push(format!("loaded project from {path}"));
                        if !result.missing_inputs.is_empty() {
                            self.log.push(format!(
                                "{} input file(s) couldn't be found and are shown grayed out: {}",
                                result.missing_inputs.len(),
                                result.missing_inputs.join(", ")
                            ));
                        }
                    }
                    Err(e) => self.log.push(format!("couldn't load project: {e:#}")),
                }
            }
        }
    }

    /// Forwards a key event straight to `tui_input`'s own key handling --
    /// covers typing, Backspace/Delete, Left/Right, Home/End, and (if ever
    /// bound in `main.rs`) word-jump/kill-line, all via its
    /// `to_input_request` mapping -- then refreshes path suggestions if
    /// this field is one that offers them. `main.rs` routes every
    /// `TextInput`-mode key here except Enter/Esc/Tab/Up/Down, which
    /// `to_input_request` leaves unmapped anyway (they're mode transitions
    /// and suggestion-list navigation, not text edits).
    pub fn text_input_handle_key(&mut self, key: KeyEvent) {
        if let Mode::TextInput {
            target,
            input,
            suggestions,
            selected,
        } = &mut self.mode
        {
            input.handle_event(&CrosstermEvent::Key(key));
            if matches!(
                target,
                TextTarget::NewInputPath
                    | TextTarget::OutputPath(_)
                    | TextTarget::SaveProjectPath
                    | TextTarget::LoadProjectPath
            ) {
                *suggestions = path_suggestions(input.value());
                *selected = 0;
            }
        }
    }

    /// Up/Down while typing a path: move the highlighted suggestion.
    pub fn text_input_move_suggestion(&mut self, delta: isize) {
        if let Mode::TextInput {
            suggestions,
            selected,
            ..
        } = &mut self.mode
        {
            let len = suggestions.len() as isize;
            if len == 0 {
                return;
            }
            *selected = (*selected as isize + delta).rem_euclid(len) as usize;
        }
    }

    /// Tab: replace the buffer's current path segment with the highlighted
    /// suggestion (shell-style completion), and refresh suggestions against
    /// the new, longer buffer so drilling into a directory keeps working.
    pub fn text_input_accept_suggestion(&mut self) {
        if let Mode::TextInput {
            input,
            suggestions,
            selected,
            ..
        } = &mut self.mode
            && let Some(chosen) = suggestions.get(*selected).cloned()
        {
            *input = Input::new(chosen);
            *suggestions = path_suggestions(input.value());
            *selected = 0;
        }
    }
}

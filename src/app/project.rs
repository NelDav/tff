use super::text_input::{path_suggestions, text_input_mode};
use super::{App, TextTarget};

impl App {
    /// Ctrl+S: prompts for a path to save the current graph to (see
    /// `crate::project::save`) -- prefilled with a sensible default name so
    /// just pressing Enter works, same idea as `add_output`'s default path.
    pub fn start_save_project(&mut self) {
        let buffer = "project.tffproj".to_string();
        let suggestions = path_suggestions(&buffer);
        self.mode = text_input_mode(TextTarget::SaveProjectPath, buffer, suggestions);
    }

    /// Ctrl+O: prompts for a path to load a graph from, replacing the
    /// current one entirely (see `crate::project::load`).
    pub fn start_load_project(&mut self) {
        let buffer = String::new();
        let suggestions = path_suggestions(&buffer);
        self.mode = text_input_mode(TextTarget::LoadProjectPath, buffer, suggestions);
    }
}

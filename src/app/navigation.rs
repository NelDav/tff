use super::{App, Focus};
use crate::graph::{Endpoint, Target};

impl App {
    pub fn cycle_focus(&mut self, forward: bool) {
        let n = self.node_count();
        let cur = self.focus_index();
        let next = if forward {
            (cur + 1) % n
        } else {
            (cur + n - 1) % n
        };
        self.set_focus_index(next);
    }

    /// Up/Down while a node is focused: cycles the selected stream (input),
    /// selected outgoing connection (modifier), or selected incoming
    /// connection (output) -- an output's row list includes one more row
    /// than its mapped-stream count when its chapters slot is connected
    /// (see `disconnect_focused`'s Output branch), same as an unconnected
    /// chapters slot doesn't get a row at all in the UI.
    pub fn cycle_row(&mut self, forward: bool) {
        let len = match self.focus {
            Focus::Input(i) => self.graph.inputs.get(i).map_or(0, |n| n.streams.len()),
            Focus::Modifier(i) => self.graph.modifiers.get(i).map_or(0, |m| {
                self.graph.outgoing(Endpoint::ModifierOut(m.id)).len()
            }),
            Focus::Output(i) => self.graph.outputs.get(i).map_or(0, |o| {
                let mapped = self.graph.incoming(Target::Output(o.id)).len();
                let has_chapters = !self.graph.incoming(Target::OutputChapters(o.id)).is_empty();
                mapped + usize::from(has_chapters)
            }),
        };
        if len == 0 {
            return;
        }
        self.row_idx = if forward {
            (self.row_idx + 1) % len
        } else {
            (self.row_idx + len - 1) % len
        };
        self.selection_anchor = None; // plain navigation ends any in-progress Shift+range
    }

    /// Space: toggle the currently-hovered stream's membership in the
    /// pending selection -- only meaningful on an input node, since that's
    /// the only place multiple distinct ports genuinely exist to choose
    /// among (a modifier has exactly one output port already toggled
    /// directly by 'c', see `toggle_connect`). Building up a selection
    /// this way lets several ports -- even across different input nodes --
    /// be armed together in one 'c' press instead of one at a time.
    pub fn toggle_port_selection(&mut self) {
        let Focus::Input(i) = self.focus else { return };
        let Some(node) = self.graph.inputs.get(i) else { return };
        if node.streams.get(self.row_idx).is_none() {
            return;
        }
        let ep = Endpoint::Stream { node: node.id, stream_idx: self.row_idx };
        self.selection_anchor = None; // an explicit single toggle isn't part of a range
        if !self.selected.insert(ep) {
            self.selected.remove(&ep);
        }
    }

    /// Ctrl+A: select every port on the focused input node in one action,
    /// adding to whatever's already selected elsewhere (e.g. on a
    /// different input node picked before Tab-ing here) rather than
    /// replacing it -- same additive spirit as `toggle_port_selection`.
    pub fn select_all_ports(&mut self) {
        let Focus::Input(i) = self.focus else { return };
        let Some(node) = self.graph.inputs.get(i) else { return };
        self.selection_anchor = None; // an explicit select-all isn't part of a range
        for idx in 0..node.streams.len() {
            self.selected.insert(Endpoint::Stream { node: node.id, stream_idx: idx });
        }
    }

    /// Shift+Up/Down: extend the pending selection as a contiguous range
    /// from wherever it started to the row now under the cursor -- the
    /// same anchor-then-extend model a text editor uses for shift-click
    /// range selection. Recomputes the range from the anchor on every
    /// press (rather than incrementally toggling) so shrinking it back
    /// (Shift+Down then Shift+Up) correctly un-selects rows outside the
    /// new range instead of leaving stale selections behind; only this
    /// node's own rows are touched, so a range already picked on a
    /// different input node is left alone.
    pub fn extend_port_selection(&mut self, forward: bool) {
        let Focus::Input(i) = self.focus else { return };
        let Some(node) = self.graph.inputs.get(i) else { return };
        let len = node.streams.len();
        if len == 0 {
            return;
        }
        let node_id = node.id;
        let anchor = *self.selection_anchor.get_or_insert(self.row_idx);
        self.row_idx = if forward { (self.row_idx + 1).min(len - 1) } else { self.row_idx.saturating_sub(1) };

        let (lo, hi) = if anchor <= self.row_idx { (anchor, self.row_idx) } else { (self.row_idx, anchor) };
        self.selected.retain(|ep| !matches!(ep, Endpoint::Stream { node, .. } if *node == node_id));
        for idx in lo..=hi {
            self.selected.insert(Endpoint::Stream { node: node_id, stream_idx: idx });
        }
    }

    pub fn move_focused_node(&mut self, dx: f64, dy: f64) {
        let step = 2.0;
        let pos = match self.focus {
            Focus::Input(i) => self.graph.inputs.get_mut(i).map(|n| &mut n.pos),
            Focus::Modifier(i) => self.graph.modifiers.get_mut(i).map(|n| &mut n.pos),
            Focus::Output(i) => self.graph.outputs.get_mut(i).map(|n| &mut n.pos),
        };
        if let Some(pos) = pos {
            pos.0 = (pos.0 + dx * step).max(0.0);
            pos.1 = (pos.1 + dy * step).max(0.0);
        }
    }

    /// Left/Right on the focused node: scrolls its title and body text
    /// horizontally, for reading a path or label wider than the box draws
    /// it -- see `App::text_scroll` and `ui::scroll_text`. Can't scroll
    /// past the point where the node's longest line is fully revealed
    /// (see `focused_node_max_scroll`); Left simply can't go past the
    /// unscrolled start.
    pub fn scroll_node_text(&mut self, forward: bool) {
        const STEP: u16 = 4;
        let max_scroll = self.focused_node_max_scroll();
        self.text_scroll = if forward {
            self.text_scroll.saturating_add(STEP).min(max_scroll)
        } else {
            self.text_scroll.saturating_sub(STEP)
        };
    }

    /// How far the focused node's text can scroll before nothing further
    /// would become visible -- the longest of its title and body lines,
    /// minus the box's own width (so once that line's last character sits
    /// flush with the right edge, scrolling stops). 0 if nothing's
    /// focused or the node vanished out from under the focus index.
    fn focused_node_max_scroll(&self) -> u16 {
        let (title, lines, width) = match self.focus {
            Focus::Input(i) => {
                let Some(n) = self.graph.inputs.get(i) else { return 0 };
                let (title, lines) = crate::ui::input_node_text_extent(self, n);
                (title, lines, n.width)
            }
            Focus::Modifier(i) => {
                let Some(n) = self.graph.modifiers.get(i) else { return 0 };
                let (title, lines) = crate::ui::modifier_node_text_extent(self, n);
                (title, lines, n.width)
            }
            Focus::Output(i) => {
                let Some(n) = self.graph.outputs.get(i) else { return 0 };
                let (title, lines) = crate::ui::output_node_text_extent(self, i, n);
                (title, lines, n.width)
            }
        };
        let max_len = lines
            .iter()
            .map(|l| l.chars().count())
            .max()
            .unwrap_or(0)
            .max(title.chars().count());
        (max_len as u16).saturating_sub(width.saturating_sub(2))
    }

    /// Shift+Right/Left on the focused node: grows or shrinks its box by
    /// widening or narrowing its right edge (the left edge -- `pos.0` --
    /// never moves), between a floor just wide enough to still show a
    /// title and a generous ceiling against runaway growth from holding
    /// the key down.
    pub fn resize_focused_node(&mut self, grow: bool) {
        const STEP: u16 = 2;
        const MIN_WIDTH: u16 = 14;
        const MAX_WIDTH: u16 = 200;
        let width = match self.focus {
            Focus::Input(i) => self.graph.inputs.get_mut(i).map(|n| &mut n.width),
            Focus::Modifier(i) => self.graph.modifiers.get_mut(i).map(|n| &mut n.width),
            Focus::Output(i) => self.graph.outputs.get_mut(i).map(|n| &mut n.width),
        };
        if let Some(width) = width {
            *width = if grow {
                width.saturating_add(STEP).min(MAX_WIDTH)
            } else {
                width.saturating_sub(STEP).max(MIN_WIDTH)
            };
        }
    }
}

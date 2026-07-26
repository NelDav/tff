use super::{App, ChapterTimeField, Mode, TextTarget};
use crate::graph::{Chapter, Endpoint, Graph, ModifierKind, NodeId, StreamKind, Target};

/// Which column of the chapter table (see `Mode::ChapterTable`) is
/// selected on the current row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChapterColumn {
    Start,
    End,
    Title,
}

impl ChapterColumn {
    pub(super) fn next(self) -> Self {
        match self {
            ChapterColumn::Start => ChapterColumn::End,
            ChapterColumn::End => ChapterColumn::Title,
            ChapterColumn::Title => ChapterColumn::Start,
        }
    }

    pub(super) fn prev(self) -> Self {
        match self {
            ChapterColumn::Start => ChapterColumn::Title,
            ChapterColumn::End => ChapterColumn::Start,
            ChapterColumn::Title => ChapterColumn::End,
        }
    }
}

/// A `ChapterEdit` modifier's own chapter list, if `id` refers to one.
pub(super) fn chapter_edit_chapters(graph: &Graph, id: NodeId) -> Option<&Vec<Chapter>> {
    match graph.modifier(id).map(|m| &m.kind) {
        Some(ModifierKind::ChapterEdit { chapters }) => Some(chapters),
        _ => None,
    }
}

pub(super) fn chapter_edit_chapters_mut(graph: &mut Graph, id: NodeId) -> Option<&mut Vec<Chapter>> {
    match graph.modifier_mut(id).map(|m| &mut m.kind) {
        Some(ModifierKind::ChapterEdit { chapters }) => Some(chapters),
        _ => None,
    }
}

/// The chapters connected to a `ChapterEdit` node's input, if its single
/// incoming wire resolves to a chapter-kind source -- used by
/// `sync_chapter_edit_import` to keep that node's auto-imported entries in
/// sync with whatever's currently wired in.
fn connected_input_chapters(graph: &Graph, modifier: NodeId) -> Option<Vec<Chapter>> {
    let wire = graph.wires.iter().find(|w| w.to == Target::ModifierIn(modifier))?;
    let Endpoint::Stream { node, stream_idx } = wire.from else { return None };
    let input = graph.input(node)?;
    (input.streams.get(stream_idx)?.kind == StreamKind::Chapter).then(|| input.chapters.clone())
}

/// Reconciles a `ChapterEdit` node's auto-imported entries against
/// whatever's *currently* wired into its input: strips every entry
/// previously tagged `imported` (see `graph::Chapter::imported`'s doc
/// comment), then, if a chapter-kind source is connected, re-imports its
/// current chapters fresh, merging them in alongside whatever manually
/// added entries remain untouched.
///
/// Idempotent and safe to call any time this node's own incoming wire
/// might have changed -- covers connecting a new source, reconnecting to a
/// different one (a modifier's input only ever holds one wire, so this
/// naturally replaces the old imported set with the new one), and
/// disconnecting entirely (nothing to re-import, so the old set is simply
/// removed). No-op if `id` isn't a `ChapterEdit` node. Deliberately *not*
/// called on every graph mutation -- only from the handful of call sites
/// that can actually change *this* node's own incoming wire -- since
/// running it unconditionally would silently discard a user's edits to an
/// already-imported chapter the next time anything else in the graph
/// happened to change.
pub(super) fn sync_chapter_edit_import(graph: &mut Graph, id: NodeId) {
    let Some(chapters) = chapter_edit_chapters_mut(graph, id) else { return };
    chapters.retain(|c| !c.imported);
    if let Some(imported) = connected_input_chapters(graph, id) {
        let Some(chapters) = chapter_edit_chapters_mut(graph, id) else { return };
        chapters.extend(imported.into_iter().map(|c| Chapter::imported(c.start_secs, c.end_secs, c.title)));
    }
}

/// The `ChapterEdit` modifier ids fed by any wire matching `predicate`,
/// collected *before* a bulk wire removal so the affected nodes' imported
/// chapters can be resynced afterward via `sync_chapter_edit_import` --
/// removing the wire first would make it impossible to tell which nodes
/// were affected.
pub(super) fn chapter_edit_modifiers_fed_by(graph: &Graph, predicate: impl Fn(&crate::graph::Wire) -> bool) -> Vec<NodeId> {
    graph
        .wires
        .iter()
        .filter(|w| predicate(w))
        .filter_map(|w| match w.to {
            Target::ModifierIn(mid) if matches!(graph.modifier(mid).map(|m| &m.kind), Some(ModifierKind::ChapterEdit { .. })) => {
                Some(mid)
            }
            _ => None,
        })
        .collect()
}

impl App {
    /// 'e' on a focused `ChapterEdit` modifier: opens its chapter table
    /// directly, landing on the first chapter's start column -- or, if the
    /// list is empty, straight on the trailing "add chapter" row, so
    /// adding the very first chapter is just 'e' then Enter.
    pub(super) fn open_chapter_table(&mut self, modifier: NodeId) {
        self.mode = Mode::ChapterTable { modifier, row: 0, col: ChapterColumn::Start };
    }

    /// Up/Down while the chapter table is open: moves between chapter
    /// rows, including the trailing "add chapter" row one past the last
    /// real chapter (see `Mode::ChapterTable`).
    pub fn chapter_table_move_row(&mut self, forward: bool) {
        let Mode::ChapterTable { modifier, row, .. } = &mut self.mode else { return };
        let len = chapter_edit_chapters(&self.graph, *modifier).map_or(0, |cs| cs.len());
        if forward {
            if *row < len {
                *row += 1;
            }
        } else if *row > 0 {
            *row -= 1;
        }
    }

    /// Left/Right while the chapter table is open: cycles which column is
    /// selected on the current row. Meaningless on the trailing "add
    /// chapter" row, but harmless to move anyway -- Enter there always
    /// just adds, regardless of column.
    pub fn chapter_table_move_col(&mut self, forward: bool) {
        let Mode::ChapterTable { col, .. } = &mut self.mode else { return };
        *col = if forward { col.next() } else { col.prev() };
    }

    /// Tab/Shift+Tab while the chapter table is open: moves through cells
    /// in reading order rather than staying within a row like Left/Right
    /// do -- past the last column of a row, it wraps to the first column
    /// of the next one (and symmetrically backward), landing on the
    /// trailing "add chapter" row same as any other row. Clamped at the
    /// very first and very last cell.
    pub fn chapter_table_tab(&mut self, forward: bool) {
        let Mode::ChapterTable { modifier, row, col } = &mut self.mode else { return };
        let len = chapter_edit_chapters(&self.graph, *modifier).map_or(0, |cs| cs.len());
        if forward {
            if *row >= len {
                return; // already on the add row -- nothing further to tab into
            }
            if *col == ChapterColumn::Title {
                *row += 1;
                *col = ChapterColumn::Start;
            } else {
                *col = col.next();
            }
        } else if *col == ChapterColumn::Start {
            if *row == 0 {
                return; // already at the very first cell
            }
            *row -= 1;
            *col = ChapterColumn::Title;
        } else {
            *col = col.prev();
        }
    }

    /// Enter while the chapter table is open. On the trailing "add
    /// chapter" row, appends a new chapter right away -- prefilled with
    /// the previous chapter's end time as its own start, so a run of
    /// additions chains without retyping -- and lands the cursor on it,
    /// with no further menu needed to actually have a chapter in the
    /// list. On an existing chapter's row, opens the text input for
    /// whichever column is selected.
    pub fn chapter_table_confirm(&mut self) {
        let Mode::ChapterTable { modifier, row, col } = &self.mode else { return };
        let (modifier, row, col) = (*modifier, *row, *col);
        let len = chapter_edit_chapters(&self.graph, modifier).map_or(0, |cs| cs.len());

        if row >= len {
            let start = chapter_edit_chapters(&self.graph, modifier).and_then(|cs| cs.last()).map_or(0.0, |c| c.end_secs);
            if let Some(chapters) = chapter_edit_chapters_mut(&mut self.graph, modifier) {
                chapters.push(Chapter::new(start, start, String::new()));
            }
            self.log.push("added chapter".to_string());
            self.mode = Mode::ChapterTable { modifier, row: len, col: ChapterColumn::Start };
            return;
        }

        let Some(chapter) = chapter_edit_chapters(&self.graph, modifier).and_then(|cs| cs.get(row)) else {
            return;
        };
        match col {
            ChapterColumn::Start | ChapterColumn::End => {
                let field = match col {
                    ChapterColumn::Start => ChapterTimeField::Start,
                    _ => ChapterTimeField::End,
                };
                let current = match field {
                    ChapterTimeField::Start => chapter.start_secs,
                    ChapterTimeField::End => chapter.end_secs,
                };
                self.mode = super::text_input::text_input_mode(
                    TextTarget::ChapterTime { modifier, index: row, field },
                    crate::graph::format_time(current),
                    Vec::new(),
                );
            }
            ChapterColumn::Title => {
                let current = chapter.title.clone();
                self.mode =
                    super::text_input::text_input_mode(TextTarget::ChapterTitle { modifier, index: row }, current, Vec::new());
            }
        }
    }

    /// 'd' while the chapter table is open: removes the chapter at the
    /// current row. A no-op on the trailing "add chapter" row -- there's
    /// nothing there to delete.
    pub fn chapter_table_delete(&mut self) {
        let Mode::ChapterTable { modifier, row, .. } = &self.mode else { return };
        let (modifier, row) = (*modifier, *row);
        let Some(chapters) = chapter_edit_chapters_mut(&mut self.graph, modifier) else { return };
        if row >= chapters.len() {
            return;
        }
        chapters.remove(row);
        self.log.push("chapter deleted".to_string());
        let new_len = chapter_edit_chapters(&self.graph, modifier).map_or(0, |cs| cs.len());
        if let Mode::ChapterTable { row, .. } = &mut self.mode {
            *row = (*row).min(new_len);
        }
    }

    /// Esc while the chapter table is open: just closes it back to
    /// Normal, same as every other overlay in this app.
    pub fn chapter_table_close(&mut self) {
        self.mode = Mode::Normal;
    }
}

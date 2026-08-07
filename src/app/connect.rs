use super::chapters::{chapter_edit_modifiers_fed_by, sync_chapter_edit_import};
use super::{App, Focus};
use crate::graph::{Endpoint, ModifierKind, NodeId, StreamKind, Target};

impl App {
    /// 'c': on an input, arm the whole pending selection at once if
    /// there's one (see `toggle_port_selection`/`extend_port_selection`),
    /// else just arm the single hovered stream (the original, low-friction
    /// one-at-a-time behavior) -- either way *replacing* whatever was
    /// armed before, not adding to it, so pressing 'c' again after
    /// selecting or hovering something else doesn't keep piling more ports
    /// onto an earlier armed set. The one exception is pressing 'c' again
    /// on a single hovered port that's already the sole armed one, which
    /// disarms it instead -- otherwise there'd be no way to un-arm a
    /// mistaken single press short of Esc (which also drops any pending
    /// selection). On a modifier, arm/disarm its own single output, or --
    /// if something else is armed -- wire it in (rejecting more than one,
    /// since a modifier's input only ever holds one wire). On an output,
    /// connect every currently-armed source in one action.
    pub fn toggle_connect(&mut self) {
        match self.focus {
            Focus::Input(i) => {
                let Some(node) = self.graph.inputs.get(i) else {
                    return;
                };
                if !self.selected.is_empty() {
                    let n = self.selected.len();
                    self.armed = std::mem::take(&mut self.selected);
                    self.log.push(format!(
                        "armed {n} port(s) — focus a modifier or output, press 'c' to connect"
                    ));
                    return;
                }
                let Some(stream) = node.streams.get(self.row_idx) else {
                    return;
                };
                let ep = Endpoint::Stream {
                    node: node.id,
                    stream_idx: self.row_idx,
                };
                if self.armed.len() == 1 && self.armed.contains(&ep) {
                    self.armed.clear(); // re-pressing 'c' on the only armed port disarms it
                } else {
                    self.armed.clear();
                    self.armed.insert(ep);
                    self.log.push(format!(
                        "armed {} from {} — focus a modifier or output, press 'c' to connect",
                        stream.label(),
                        node.path
                    ));
                }
            }
            Focus::Modifier(i) => {
                let Some(m) = self.graph.modifiers.get(i) else {
                    return;
                };
                let mid = m.id;
                let this_output = Endpoint::ModifierOut(mid);
                if self.armed.is_empty() {
                    self.armed.insert(this_output);
                    self.log.push(
                        "armed this node's output — focus the next node, press 'c' to connect"
                            .to_string(),
                    );
                } else if self.armed.len() == 1 && self.armed.contains(&this_output) {
                    self.armed.remove(&this_output); // disarm
                } else if matches!(m.kind, ModifierKind::Concat) {
                    self.connect_concat_segments(mid);
                } else if self.armed.len() > 1 {
                    self.log.push(format!(
                        "can't connect {} streams to a modifier -- it only accepts one",
                        self.armed.len()
                    ));
                } else {
                    let source = *self.armed.iter().next().expect("checked non-empty above");
                    // Only reject when the source's kind is actually known
                    // and wrong -- an armed endpoint whose own chain isn't
                    // fully resolved yet (e.g. another modifier with
                    // nothing wired into *its* input) has no determinable
                    // kind, and should still be connectable optimistically,
                    // same as before this check existed.
                    if let Some(source_kind) = self.endpoint_stream_kind(source)
                        && !m.kind.accepts_stream_kind(source_kind)
                    {
                        self.log.push(format!(
                            "{} doesn't accept a {} stream",
                            m.kind.short_label(),
                            source_kind.noun()
                        ));
                        return;
                    }
                    self.graph.connect(source, Target::ModifierIn(mid));
                    sync_chapter_edit_import(&mut self.graph, mid);
                    self.armed.clear();
                    self.log.push("connected".to_string());
                }
            }
            Focus::Output(i) => {
                let Some(output_id) = self.graph.outputs.get(i).map(|o| o.id) else {
                    return;
                };
                if self.armed.is_empty() {
                    self.log.push(
                        "nothing armed -- arm a stream or modifier output first ('c')".to_string(),
                    );
                    return;
                }
                let n = self.armed.len();
                for source in std::mem::take(&mut self.armed) {
                    let target = if self.endpoint_stream_kind(source) == Some(StreamKind::Chapter) {
                        Target::OutputChapters(output_id)
                    } else {
                        Target::Output(output_id)
                    };
                    self.graph.connect(source, target);
                }
                self.log.push(if n == 1 {
                    "connected to output".to_string()
                } else {
                    format!("connected {n} ports to output")
                });
            }
        }
    }

    /// 'd': on an input port, disconnect it from everything downstream --
    /// or, with a pending selection (see `toggle_port_selection`/
    /// `extend_port_selection`), disconnect *every* selected port from
    /// everything downstream in one action, the same way 'c' arms them all
    /// at once, and regardless of which input node is currently focused
    /// (the selection is global, same as `armed`). On a modifier,
    /// disconnect just the selected outgoing connection; on an output,
    /// disconnect just the selected incoming connection.
    pub fn disconnect_focused(&mut self) {
        match self.focus {
            Focus::Input(i) => {
                let Some(node) = self.graph.inputs.get(i) else {
                    return;
                };
                if !self.selected.is_empty() {
                    let ports = std::mem::take(&mut self.selected);
                    let n = ports.len();
                    let affected = chapter_edit_modifiers_fed_by(&self.graph, |w| ports.contains(&w.from));
                    let before = self.graph.wires.len();
                    self.graph.wires.retain(|w| !ports.contains(&w.from));
                    let removed = before != self.graph.wires.len();
                    self.log.push(if removed {
                        format!("disconnected {n} port(s) from everything downstream")
                    } else {
                        format!("{n} port(s) had nothing connected")
                    });
                    for mid in affected {
                        sync_chapter_edit_import(&mut self.graph, mid);
                    }
                    return;
                }
                let Some(stream) = node.streams.get(self.row_idx) else {
                    return;
                };
                let ep = Endpoint::Stream {
                    node: node.id,
                    stream_idx: self.row_idx,
                };
                let label = stream.label();
                let affected = chapter_edit_modifiers_fed_by(&self.graph, |w| w.from == ep);
                let before = self.graph.wires.len();
                self.graph.wires.retain(|w| w.from != ep);
                if self.graph.wires.len() != before {
                    self.log
                        .push(format!("disconnected {label} from everything downstream"));
                }
                for mid in affected {
                    sync_chapter_edit_import(&mut self.graph, mid);
                }
            }
            Focus::Modifier(i) => {
                let Some(m) = self.graph.modifiers.get(i) else {
                    return;
                };
                let mid = m.id;
                // A Concat node's row list is segments (its incoming wires,
                // one per row) followed by its outgoing wires -- same
                // combining scheme as an output's mapped-streams-then-
                // chapters list (see `cycle_row`'s Output arm) -- so
                // `row_idx` needs splitting between the two regions; every
                // other modifier kind has no segments section at all, and
                // `row_idx` indexes its outgoing wires directly, as before.
                if matches!(m.kind, ModifierKind::Concat) {
                    let segments = self.graph.incoming(Target::ModifierIn(mid));
                    if self.row_idx < segments.len() {
                        self.graph.remove_wire_at(segments[self.row_idx]);
                        self.log.push("segment removed from concat".to_string());
                        let new_len = self.graph.incoming(Target::ModifierIn(mid)).len();
                        if new_len > 0 && self.row_idx >= new_len {
                            self.row_idx = new_len - 1;
                        }
                        return;
                    }
                    let outgoing = self.graph.outgoing(Endpoint::ModifierOut(mid));
                    let Some(&wi) = outgoing.get(self.row_idx - segments.len()) else {
                        return;
                    };
                    self.graph.remove_wire_at(wi);
                    self.log.push("disconnected".to_string());
                    let new_total = segments.len() + self.graph.outgoing(Endpoint::ModifierOut(mid)).len();
                    if new_total > 0 && self.row_idx >= new_total {
                        self.row_idx = new_total - 1;
                    }
                    return;
                }
                let ep = Endpoint::ModifierOut(mid);
                let outgoing = self.graph.outgoing(ep);
                let Some(&wi) = outgoing.get(self.row_idx) else {
                    return;
                };
                self.graph.remove_wire_at(wi);
                self.log.push("disconnected".to_string());
                let new_len = self.graph.outgoing(ep).len();
                if new_len > 0 && self.row_idx >= new_len {
                    self.row_idx = new_len - 1;
                }
            }
            Focus::Output(i) => {
                let Some(output_id) = self.graph.outputs.get(i).map(|o| o.id) else {
                    return;
                };
                let incoming = self.graph.incoming(Target::Output(output_id));
                // The chapters slot is always one more row after the
                // mapped-stream rows (see `cycle_row`'s Output arm).
                if self.row_idx >= incoming.len() {
                    let chapter_wires = self.graph.incoming(Target::OutputChapters(output_id));
                    if let Some(&wi) = chapter_wires.first() {
                        self.graph.remove_wire_at(wi);
                        self.log.push("chapters disconnected".to_string());
                    }
                    return;
                }
                let wi = incoming[self.row_idx];
                self.graph.remove_wire_at(wi);
                self.log.push("disconnected".to_string());
                let new_len = self.graph.incoming(Target::Output(output_id)).len();
                if new_len > 0 && self.row_idx >= new_len {
                    self.row_idx = new_len - 1;
                }
            }
        }
    }

    /// Ctrl+Up/Down: moves the hovered row past its neighbor, reordering
    /// the underlying wires (see `Graph::swap_wires`) -- an output's
    /// mapped-stream row (reordering the streams in the muxed container),
    /// or a Concat modifier's segment row (reordering the join order in
    /// the `concat` filter). A no-op anywhere else: an output's chapters
    /// row (only ever one, nothing to reorder it against), a Concat's own
    /// outgoing rows (reordering where its result feeds doesn't mean
    /// anything), any other modifier kind (single input, nothing to
    /// reorder), or either edge of whichever list applies.
    pub fn move_focused_row(&mut self, forward: bool) {
        let incoming = match self.focus {
            Focus::Output(i) => {
                let Some(output_id) = self.graph.outputs.get(i).map(|o| o.id) else {
                    return;
                };
                self.graph.incoming(Target::Output(output_id))
            }
            Focus::Modifier(i) => {
                let Some(m) = self.graph.modifiers.get(i) else {
                    return;
                };
                if !matches!(m.kind, ModifierKind::Concat) {
                    return;
                }
                self.graph.incoming(Target::ModifierIn(m.id))
            }
            Focus::Input(_) => return,
        };
        if self.row_idx >= incoming.len() {
            return;
        }
        let Some(new_row) = (if forward { self.row_idx.checked_add(1) } else { self.row_idx.checked_sub(1) })
        else {
            return;
        };
        if new_row >= incoming.len() {
            return;
        }
        self.graph.swap_wires(incoming[self.row_idx], incoming[new_row]);
        self.row_idx = new_row;
    }

    /// 'c' on a Concat modifier with one or more armed sources: appends
    /// every one of them as a new segment, in `App::armed`'s (stable,
    /// sorted) iteration order -- rejecting a source outright if it isn't
    /// video/audio, or if it doesn't match the kind every other segment
    /// (already connected, or already accepted earlier in this same batch)
    /// shares, so a Concat node never ends up joining a mix ffmpeg's
    /// `concat` filter couldn't actually handle (see `resolve`, which
    /// validates the same invariant when reading the graph back).
    fn connect_concat_segments(&mut self, mid: NodeId) {
        let mut expected_kind = self
            .graph
            .incoming(Target::ModifierIn(mid))
            .into_iter()
            .find_map(|wi| self.endpoint_stream_kind(self.graph.wires[wi].from));
        let sources: Vec<Endpoint> = std::mem::take(&mut self.armed).into_iter().collect();
        let n = sources.len();
        let mut connected = 0usize;
        for source in sources {
            match self.endpoint_stream_kind(source) {
                // Not yet resolvable (e.g. an armed modifier output with
                // nothing wired into it yet) -- accept optimistically,
                // same as the single-input connect path does.
                None => {
                    self.graph.connect(source, Target::ModifierIn(mid));
                    connected += 1;
                }
                Some(kind) if !ModifierKind::Concat.accepts_stream_kind(kind) => {
                    self.log.push(format!("concat doesn't accept a {} stream", kind.noun()));
                }
                Some(kind) if expected_kind.is_some_and(|e| e != kind) => {
                    self.log.push(format!(
                        "concat already has a {} segment -- can't mix in a {} one",
                        expected_kind.expect("checked Some above").noun(),
                        kind.noun()
                    ));
                }
                Some(kind) => {
                    expected_kind = Some(kind);
                    self.graph.connect(source, Target::ModifierIn(mid));
                    connected += 1;
                }
            }
        }
        self.log.push(if connected == n {
            format!("added {connected} segment(s) to concat")
        } else {
            format!("added {connected}/{n} segment(s) to concat (rest rejected, see above)")
        });
    }
}

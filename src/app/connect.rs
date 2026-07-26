use super::chapters::{chapter_edit_modifiers_fed_by, sync_chapter_edit_import};
use super::{App, Focus};
use crate::graph::{Endpoint, StreamKind, Target};

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
                } else if self.armed.len() > 1 {
                    self.log.push(format!(
                        "can't connect {} streams to a modifier -- it only accepts one",
                        self.armed.len()
                    ));
                } else {
                    let source = *self.armed.iter().next().expect("checked non-empty above");
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
                let ep = Endpoint::ModifierOut(m.id);
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

    /// Ctrl+Up/Down while an output node is focused: moves the hovered
    /// mapped-stream row past its neighbor, which reorders the streams in
    /// the muxed container (see `Graph::swap_wires`). A no-op on the
    /// chapters row (there's only ever one, nothing to reorder it against)
    /// or at either edge of the list.
    pub fn move_output_row(&mut self, forward: bool) {
        let Focus::Output(i) = self.focus else { return };
        let Some(output_id) = self.graph.outputs.get(i).map(|o| o.id) else {
            return;
        };
        let incoming = self.graph.incoming(Target::Output(output_id));
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
}

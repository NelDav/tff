use std::collections::{BTreeMap, BTreeSet};

use super::text_input::text_input_mode;
use super::{App, ExtraArgsTarget, Mode, PickerEntry, PickerKind, TextTarget};
use crate::graph::{Codec, FilterName, Graph, ModifierKind};

/// Common containers offered first in the picker, ahead of the rest of
/// ffmpeg's discovered muxer list. Paired with the file extension to switch
/// the output path to for convenience -- purely cosmetic, since the actual
/// container is set via an explicit `-f` argument regardless of extension.
const COMMON_CONTAINERS: &[(&str, &str)] = &[
    ("matroska", "mkv"),
    ("mp4", "mp4"),
    ("mov", "mov"),
    ("webm", "webm"),
    ("avi", "avi"),
];

/// The options whose display text matches `query` (case-insensitive
/// substring), as indices into `options`. Empty query matches everything.
pub fn filtered_indices(options: &[PickerEntry], query: &str) -> Vec<usize> {
    if query.is_empty() {
        return (0..options.len()).collect();
    }
    let query = query.to_lowercase();
    options
        .iter()
        .enumerate()
        .filter(|(_, o)| o.display.to_lowercase().contains(&query))
        .map(|(i, _)| i)
        .collect()
}

/// If ffmpeg actually reported a discovered list, keep only the curated
/// `names` that are really present in it (never claim support for
/// something the local build lacks), then append the rest of the
/// discovered list alphabetically. Leaves `names` untouched if discovery
/// failed (empty), so the curated fallback is still offered.
pub(super) fn prioritize_and_extend<'a>(names: &mut Vec<String>, discovered: impl Iterator<Item = &'a str>) {
    let discovered: Vec<&str> = discovered.collect();
    if discovered.is_empty() {
        return;
    }
    names.retain(|n| discovered.contains(&n.as_str()));
    let mut rest: Vec<String> = discovered
        .iter()
        .filter(|d| !names.iter().any(|n| n == *d))
        .map(|d| d.to_string())
        .collect();
    rest.sort();
    names.extend(rest);
}

pub(super) fn picker_options(reset_label: &str, names: Vec<String>) -> Vec<PickerEntry> {
    let mut options = vec![PickerEntry {
        display: reset_label.to_string(),
        value: None,
    }];
    options.extend(names.into_iter().map(|n| PickerEntry {
        display: n.clone(),
        value: Some(n),
    }));
    options
}

pub(super) fn selected_index(options: &[PickerEntry], current: Option<&str>) -> usize {
    match current {
        None => 0,
        Some(name) => options
            .iter()
            .position(|o| o.value.as_deref() == Some(name))
            .unwrap_or(0),
    }
}

fn filter_modifier(name: FilterName) -> ModifierKind {
    ModifierKind::Filter { name, fields: BTreeMap::new() }
}

/// Shared by Metadata and Filter nodes: one entry per curated key, showing
/// its current value or "(not set)". `allow_custom` additionally lists any
/// already-set key outside the curated list (reachable only via Metadata's
/// "custom key..." escape hatch) plus that escape hatch itself -- a Filter
/// node's parameter set is fixed, so it never needs one.
pub(super) fn field_picker_options(fields: &BTreeMap<String, String>, keys: &[&str], allow_custom: bool) -> Vec<PickerEntry> {
    let mut options: Vec<PickerEntry> = keys
        .iter()
        .map(|k| {
            let display = match fields.get(*k) {
                Some(v) => format!("{k}: {v}"),
                None => format!("{k}: (not set)"),
            };
            PickerEntry { display, value: Some((*k).to_string()) }
        })
        .collect();
    if allow_custom {
        for (k, v) in fields {
            if !keys.contains(&k.as_str()) {
                options.push(PickerEntry { display: format!("{k}: {v}"), value: Some(k.clone()) });
            }
        }
        options.push(PickerEntry { display: "custom key…".to_string(), value: None });
    }
    options
}

/// The disposition picker's option list: one entry per curated flag, with a
/// checkbox reflecting whether it's currently set on this node -- rebuilt
/// after every toggle so the display stays in sync.
pub(super) fn disposition_picker_options(flags: &BTreeSet<String>) -> Vec<PickerEntry> {
    crate::graph::disposition_flags()
        .iter()
        .map(|f| {
            let mark = if flags.contains(*f) { "x" } else { " " };
            PickerEntry {
                display: format!("[{mark}] {f}"),
                value: Some((*f).to_string()),
            }
        })
        .collect()
}

fn curated_extra_arg_keys(target: ExtraArgsTarget, has_teletext_decoder: bool) -> Vec<(&'static str, bool, &'static str)> {
    match target {
        ExtraArgsTarget::Input(_) => crate::graph::input_extra_arg_keys(has_teletext_decoder),
        ExtraArgsTarget::Output(_) => crate::graph::output_extra_arg_keys().to_vec(),
    }
}

/// The friendly label for a curated extra-arg key (see
/// `extra_args_picker_options`'s doc comment), or `key` itself if it's not
/// one of the curated ones -- a custom key the user typed in has no label
/// beyond its own name. Used by the text-input prompt so it matches
/// whatever the picker entry it came from was actually showing.
pub fn extra_arg_label(target: ExtraArgsTarget, key: &str, has_teletext_decoder: bool) -> &str {
    curated_extra_arg_keys(target, has_teletext_decoder)
        .iter()
        .find(|&&(k, _, _)| k == key)
        .map_or(key, |&(_, _, label)| label)
}

pub(super) fn extra_args_of(graph: &Graph, target: ExtraArgsTarget) -> Option<&BTreeMap<String, String>> {
    match target {
        ExtraArgsTarget::Input(id) => graph.input(id).map(|n| &n.extra_args),
        ExtraArgsTarget::Output(id) => graph.output(id).map(|n| &n.extra_args),
    }
}

pub(super) fn extra_args_of_mut(graph: &mut Graph, target: ExtraArgsTarget) -> Option<&mut BTreeMap<String, String>> {
    match target {
        ExtraArgsTarget::Input(id) => graph.input_mut(id).map(|n| &mut n.extra_args),
        ExtraArgsTarget::Output(id) => graph.output_mut(id).map(|n| &mut n.extra_args),
    }
}

/// The extra-args picker's option list: one entry per curated key --
/// a `[x]`/`[ ]` checkbox for a valueless switch flag (toggled in place),
/// or "label: value"/"label: (not set)" for one that takes an operand --
/// plus any already-set custom key outside the curated list and the
/// "custom key..." escape hatch itself, mirroring `field_picker_options`.
/// Displays each curated entry's friendly label (see
/// `input_extra_arg_keys`'s doc comment), not necessarily the raw `-<key>`
/// flag name stored under `value` -- picking an entry, and the actual arg
/// ffmpeg gets, are still keyed by the raw name regardless of label.
pub(super) fn extra_args_picker_options(graph: &Graph, target: ExtraArgsTarget, has_teletext_decoder: bool) -> Vec<PickerEntry> {
    let empty = BTreeMap::new();
    let fields = extra_args_of(graph, target).unwrap_or(&empty);
    let curated = curated_extra_arg_keys(target, has_teletext_decoder);

    let mut options: Vec<PickerEntry> = curated.iter().map(|&(key, is_boolean, label)| {
        let display = if is_boolean {
            let mark = if fields.contains_key(key) { "x" } else { " " };
            format!("[{mark}] {label}")
        } else {
            match fields.get(key) {
                Some(v) => format!("{label}: {v}"),
                None => format!("{label}: (not set)"),
            }
        };
        PickerEntry { display, value: Some(key.to_string()) }
    }).collect();
    for (k, v) in fields {
        if !curated.iter().any(|&(ck, _, _)| ck == k) {
            options.push(PickerEntry { display: format!("{k}: {v}"), value: Some(k.clone()) });
        }
    }
    options.push(PickerEntry { display: "custom key…".to_string(), value: None });
    options
}

impl App {
    /// Opens the extra-ffmpeg-args picker (the advanced escape hatch for
    /// options the node graph doesn't model, e.g. `-itsoffset 2.5` on an
    /// input, `-max_interleave_delta 5000000` on an output) for a given
    /// input or output.
    pub(super) fn open_extra_args_picker(&mut self, target: ExtraArgsTarget) {
        self.mode = Mode::Picker {
            kind: PickerKind::ExtraArgField { target },
            title: "extra ffmpeg args: choose flag".to_string(),
            options: extra_args_picker_options(&self.graph, target, self.has_teletext_decoder),
            selected: 0,
            query: String::new(),
            searching: false,
        };
    }

    /// 'f': open a picker listing ffmpeg's available output containers for
    /// the focused output node.
    pub fn open_container_picker(&mut self) {
        let super::Focus::Output(i) = self.focus else {
            self.log
                .push("focus an output node first, then 'f' picks its container".to_string());
            return;
        };
        let Some(output) = self.graph.outputs.get(i) else {
            return;
        };
        let output_id = output.id;
        let current = output.container.clone();

        let mut names: Vec<String> = COMMON_CONTAINERS
            .iter()
            .map(|(name, _)| name.to_string())
            .collect();
        prioritize_and_extend(&mut names, self.available_muxers.iter().map(String::as_str));

        let options = picker_options("auto (infer from file extension)", names);
        let selected = selected_index(&options, current.as_deref());

        self.mode = Mode::Picker {
            kind: PickerKind::Container { output: output_id },
            title: "output container".to_string(),
            options,
            selected,
            query: String::new(),
            searching: false,
        };
    }

    /// Up/Down (or j/k) while a picker is open. Moves within the filtered
    /// view, so it only ever lands on something currently visible.
    pub fn picker_move(&mut self, delta: isize) {
        if let Mode::Picker {
            options,
            selected,
            query,
            ..
        } = &mut self.mode
        {
            let len = filtered_indices(options, query).len() as isize;
            if len == 0 {
                return;
            }
            *selected = (*selected as isize + delta).rem_euclid(len) as usize;
        }
    }

    /// '/': start typing a query to filter the picker's options.
    pub fn picker_start_search(&mut self) {
        if let Mode::Picker {
            query,
            searching,
            selected,
            ..
        } = &mut self.mode
        {
            query.clear();
            *searching = true;
            *selected = 0;
        }
    }

    pub fn picker_search_char(&mut self, c: char) {
        if let Mode::Picker {
            query, selected, ..
        } = &mut self.mode
        {
            query.push(c);
            *selected = 0;
        }
    }

    pub fn picker_search_backspace(&mut self) {
        if let Mode::Picker {
            query, selected, ..
        } = &mut self.mode
        {
            query.pop();
            *selected = 0;
        }
    }

    /// Enter while typing a query: stop typing, keep the filter applied so
    /// arrow keys go back to navigating the (now filtered) list.
    pub fn picker_confirm_search(&mut self) {
        if let Mode::Picker { searching, .. } = &mut self.mode {
            *searching = false;
        }
    }

    /// Esc: while typing a query, cancel it outright. Otherwise, clear an
    /// already-applied filter first (mirrors vim's "clear search" on a bare
    /// Esc); only close the picker once there's no filter left to clear.
    pub fn picker_escape(&mut self) {
        let Mode::Picker {
            kind,
            title,
            options,
            mut query,
            searching,
            ..
        } = std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            return;
        };
        if searching || !query.is_empty() {
            query.clear();
            self.mode = Mode::Picker {
                kind,
                title,
                options,
                selected: 0,
                query,
                searching: false,
            };
        }
        // else: leave as Mode::Normal, set by the replace above -- this is
        // the "close the picker" case.
    }

    pub fn picker_confirm(&mut self) {
        let Mode::Picker {
            kind,
            title,
            options,
            selected,
            query,
            searching,
        } = std::mem::replace(&mut self.mode, Mode::Normal)
        else {
            return;
        };
        let real_idx = filtered_indices(&options, &query).get(selected).copied();

        // Unlike every other picker kind, toggling a disposition flag
        // doesn't close the picker -- it's a multi-select, so Enter here
        // just flips the flag and redraws the same list with its checkbox
        // updated, leaving the user in the picker to toggle more.
        if let PickerKind::DispositionFlags { modifier } = kind {
            if let Some(flag) = real_idx
                .and_then(|i| options.get(i))
                .and_then(|e| e.value.clone())
                && let Some(m) = self.graph.modifier_mut(modifier)
                && let ModifierKind::Disposition { flags } = &mut m.kind
            {
                if !flags.remove(&flag) {
                    flags.insert(flag.clone());
                }
                self.log.push(format!("{flag} toggled"));
            }
            let options = match self.graph.modifier(modifier).map(|m| &m.kind) {
                Some(ModifierKind::Disposition { flags }) => disposition_picker_options(flags),
                _ => Vec::new(),
            };
            self.mode = Mode::Picker {
                kind: PickerKind::DispositionFlags { modifier },
                title,
                options,
                selected,
                query,
                searching,
            };
            return;
        }

        // A curated valueless extra-arg key (e.g. "shortest", "re") toggles
        // in place, same idea as a disposition flag -- everything else
        // (a value-taking curated key, an already-set custom key, or
        // "custom key..." itself) falls through to the normal match below,
        // which opens a value text input instead.
        if let PickerKind::ExtraArgField { target } = kind {
            let selected_key = real_idx.and_then(|i| options.get(i)).and_then(|e| e.value.clone());
            let is_boolean = selected_key
                .as_deref()
                .is_some_and(|k| curated_extra_arg_keys(target, self.has_teletext_decoder).iter().any(|&(ck, b, _)| ck == k && b));
            if is_boolean {
                let key = selected_key.unwrap();
                if let Some(fields) = extra_args_of_mut(&mut self.graph, target) {
                    if fields.remove(&key).is_some() {
                        self.log.push(format!("{key} disabled"));
                    } else {
                        fields.insert(key.clone(), String::new());
                        self.log.push(format!("{key} enabled"));
                    }
                }
                self.mode = Mode::Picker {
                    kind: PickerKind::ExtraArgField { target },
                    title,
                    options: extra_args_picker_options(&self.graph, target, self.has_teletext_decoder),
                    selected,
                    query,
                    searching,
                };
                return;
            }
        }

        let Some(entry) = real_idx.and_then(|i| options.into_iter().nth(i)) else {
            return;
        };

        match kind {
            PickerKind::Codec { modifier } => {
                let codec = match entry.value {
                    None => Codec::Copy,
                    Some(name) => Codec::Encode(name),
                };
                self.log.push(match &codec {
                    Codec::Copy => "codec set to copy (no re-encode)".to_string(),
                    Codec::Encode(_) => format!("codec set to {}", codec.label()),
                });
                if let Some(m) = self.graph.modifier_mut(modifier) {
                    m.kind = ModifierKind::Convert(codec);
                }
            }
            PickerKind::Container { output } => {
                let Some(node) = self.graph.output_mut(output) else {
                    return;
                };
                node.container = entry.value.clone();
                match &entry.value {
                    Some(name) => {
                        if let Some((_, ext)) = COMMON_CONTAINERS.iter().find(|(n, _)| n == name) {
                            let stem = std::path::Path::new(&node.path).with_extension("");
                            node.path = format!("{}.{ext}", stem.to_string_lossy());
                        }
                        self.log
                            .push(format!("output container set to {name} ({})", node.path));
                    }
                    None => self.log.push(
                        "output container set to auto (inferred from file extension)".to_string(),
                    ),
                }
            }
            PickerKind::NewNode => {
                match entry.value.as_deref() {
                    Some("input") => {
                        self.start_add_input();
                        return;
                    }
                    Some("output") => {
                        self.add_output_node();
                        return;
                    }
                    _ => {}
                }
                let (kind, name) = match entry.value.as_deref() {
                    Some("metadata") => (
                        ModifierKind::Metadata {
                            fields: BTreeMap::new(),
                        },
                        "metadata",
                    ),
                    Some("disposition") => (
                        ModifierKind::Disposition {
                            flags: BTreeSet::new(),
                        },
                        "disposition",
                    ),
                    Some("chapters") => (ModifierKind::ChapterEdit { chapters: Vec::new() }, "chapters"),
                    Some("shift") => (filter_modifier(FilterName::Shift), "shift"),
                    Some("volume") => (filter_modifier(FilterName::Volume), "volume"),
                    Some("scale") => (filter_modifier(FilterName::Scale), "scale"),
                    Some("crop") => (filter_modifier(FilterName::Crop), "crop"),
                    Some("fade") => (filter_modifier(FilterName::Fade), "fade"),
                    Some("rotate") => (filter_modifier(FilterName::Rotate), "rotate"),
                    Some("trim") => (filter_modifier(FilterName::Trim), "trim"),
                    _ => (ModifierKind::Convert(Codec::Copy), "convert"),
                };
                self.graph.add_modifier(kind);
                self.set_focus_index(self.node_count() - self.graph.outputs.len() - 1);
                self.log.push(format!("added {name} node"));
            }
            PickerKind::MetadataKey { modifier } => match entry.value {
                Some(key) => {
                    let current = self
                        .graph
                        .modifier(modifier)
                        .and_then(|m| match &m.kind {
                            ModifierKind::Metadata { fields } => fields.get(&key).cloned(),
                            ModifierKind::Convert(_)
                            | ModifierKind::Disposition { .. }
                            | ModifierKind::Filter { .. }
                            | ModifierKind::ChapterEdit { .. } => None,
                        })
                        .unwrap_or_default();
                    self.mode = text_input_mode(TextTarget::ModifierMetadataValue { modifier, key }, current, Vec::new());
                }
                None => {
                    // "custom key..." -- first ask for the key name itself.
                    self.mode = text_input_mode(TextTarget::ModifierCustomKey(modifier), String::new(), Vec::new());
                }
            },
            PickerKind::DispositionFlags { .. } => {
                unreachable!("handled above before `entry` is computed")
            }
            PickerKind::FilterField { modifier } => {
                // No "custom key..." entry for Filter fields (see
                // field_picker_options), so entry.value is always Some.
                let Some(key) = entry.value else { return };
                let Some(ModifierKind::Filter { name, fields }) =
                    self.graph.modifier(modifier).map(|m| &m.kind)
                else {
                    return;
                };
                let current = fields.get(&key).cloned();

                // A field with a fixed set of valid values (e.g. Rotate's
                // "direction") gets a selection instead of free text --
                // anything else typed there is simply invalid, not just an
                // unusual choice.
                if let Some(values) = name.value_options(&key) {
                    let options =
                        picker_options("(not set)", values.iter().map(|v| v.to_string()).collect());
                    let selected = selected_index(&options, current.as_deref());
                    self.mode = Mode::Picker {
                        kind: PickerKind::FilterFieldValue { modifier, key: key.clone() },
                        title: format!("{}: {key}", name.label()),
                        options,
                        selected,
                        query: String::new(),
                        searching: false,
                    };
                    return;
                }

                self.mode = text_input_mode(TextTarget::ModifierFilterValue { modifier, key }, current.unwrap_or_default(), Vec::new());
            }
            PickerKind::FilterFieldValue { modifier, key } => {
                if let Some(m) = self.graph.modifier_mut(modifier)
                    && let ModifierKind::Filter { fields, .. } = &mut m.kind
                {
                    match entry.value {
                        Some(value) => {
                            fields.insert(key.clone(), value.clone());
                            self.log.push(format!("{key} set to {value}"));
                        }
                        None => {
                            fields.remove(&key);
                            self.log.push(format!("{key} cleared"));
                        }
                    }
                }
            }
            PickerKind::ExtraArgField { target } => {
                let current =
                    entry.value.as_ref().and_then(|key| extra_args_of(&self.graph, target).and_then(|f| f.get(key).cloned()));
                match entry.value {
                    Some(key) => {
                        self.mode = text_input_mode(TextTarget::ExtraArgValue { target, key }, current.unwrap_or_default(), Vec::new());
                    }
                    None => {
                        // "custom key..." -- first ask for the key name itself.
                        self.mode = text_input_mode(TextTarget::ExtraArgCustomKey(target), String::new(), Vec::new());
                    }
                }
            }
        }
    }
}

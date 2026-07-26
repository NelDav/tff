use std::collections::BTreeMap;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use super::{App, Focus};
use crate::ffmpeg;
use crate::graph::{ModifierKind, NodeId};

/// How much of the focused output's timeline 'p' renders before handing it
/// to ffplay -- long enough to judge codec/metadata choices, short enough
/// to stay fast even with a slow re-encode.
const PREVIEW_SECONDS: u32 = 20;

impl App {
    /// Writes each `ChapterEdit` modifier's chapter list (if it has one)
    /// out as a temp FFMETADATA file, keyed by that node's id, ready to
    /// hand to `Graph::build_ffmpeg_args`/`build_preview_args` as an extra
    /// input -- an output whose chapters trace straight back to a real
    /// input file (no `ChapterEdit` node in the chain) needs no file at
    /// all, since `-map_chapters` can just point at that input directly
    /// (see `Graph::resolve_chapters`). `Graph` itself does no file I/O, so
    /// this lives here rather than there. A write failure just skips that
    /// node's chapters (logged) instead of blocking the whole render --
    /// consistent with how a filtergraph error or similar isn't treated as
    /// fatal until ffmpeg itself reports it.
    fn write_chapter_files(&mut self) -> BTreeMap<NodeId, String> {
        let mut files = BTreeMap::new();
        for modifier in &self.graph.modifiers {
            let ModifierKind::ChapterEdit { chapters } = &modifier.kind else { continue };
            if chapters.is_empty() {
                continue;
            }
            let path = std::env::temp_dir().join(format!("tff-chapters-{}.ffmeta", modifier.id));
            let content = crate::graph::chapters_ffmetadata(chapters);
            match std::fs::write(&path, content) {
                Ok(()) => {
                    files.insert(modifier.id, path.to_string_lossy().into_owned());
                }
                Err(e) => {
                    self.log.push(format!("couldn't write chapter metadata: {e}"));
                }
            }
        }
        files
    }

    pub fn start_render(&mut self) {
        if self.running {
            return;
        }
        if self.graph.wires.is_empty() {
            self.log.push(
                "nothing mapped yet — arm a stream with 'c', then focus a modifier or output and press 'c' again"
                    .to_string(),
            );
            return;
        }
        let chapter_files = self.write_chapter_files();
        let (tx, rx): (Sender<String>, Receiver<String>) = mpsc::channel();
        self.rx = Some(rx);
        self.running = true;
        self.status = "running ffmpeg...".to_string();
        let args = self.graph.build_ffmpeg_args(&chapter_files);
        self.log.push(format!("$ ffmpeg {}", args.join(" ")));
        thread::spawn(move || {
            ffmpeg::run_args(args, tx);
        });
    }

    /// 'p': render the first `PREVIEW_SECONDS` of the focused output's
    /// current mapping to a temp file, then hand it to ffplay once that
    /// finishes -- lets the user see how codec/metadata choices actually
    /// turn out without waiting for (or overwriting) the real output.
    pub fn start_preview(&mut self) {
        if self.running {
            self.log.push(
                "already running ffmpeg — wait for it to finish before previewing".to_string(),
            );
            return;
        }
        let Focus::Output(i) = self.focus else {
            self.log
                .push("focus an output node first, then 'p' previews it".to_string());
            return;
        };
        let Some(output) = self.graph.outputs.get(i) else {
            return;
        };
        let output_id = output.id;
        let ext = std::path::Path::new(&output.path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("mkv");
        let preview_path = std::env::temp_dir()
            .join(format!("tff-preview-{output_id}.{ext}"))
            .to_string_lossy()
            .into_owned();

        let chapter_files = self.write_chapter_files();
        let Some(args) = self
            .graph
            .build_preview_args(output_id, &preview_path, PREVIEW_SECONDS, &chapter_files)
        else {
            self.log.push(
                "nothing mapped to this output yet — arm a stream with 'c', then focus it and press 'c' again"
                    .to_string(),
            );
            return;
        };

        let (tx, rx): (Sender<String>, Receiver<String>) = mpsc::channel();
        self.rx = Some(rx);
        self.running = true;
        self.status = format!("rendering {PREVIEW_SECONDS}s preview...");
        self.preview_target = Some(preview_path);
        self.log.push(format!("$ ffmpeg {}", args.join(" ")));
        thread::spawn(move || {
            ffmpeg::run_args(args, tx);
        });
    }

    pub fn poll_ffmpeg(&mut self) {
        let Some(rx) = &self.rx else { return };
        let mut done = None;
        while let Ok(line) = rx.try_recv() {
            if let Some(code) = line.strip_prefix("__DONE__") {
                done = Some(code.to_string());
            } else {
                self.log.push(line);
            }
        }
        if let Some(code) = done {
            self.running = false;
            self.rx = None;
            if let Some(path) = self.preview_target.take() {
                if code == "0" {
                    self.status = "preview ready".to_string();
                    self.preview_ready = Some(path);
                } else {
                    self.status = format!("preview render failed (exit code {code})");
                    self.log.push(self.status.clone());
                }
            } else {
                self.status = format!("ffmpeg exited with code {code}");
                self.log.push(self.status.clone());
            }
        }
    }
}

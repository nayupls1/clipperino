mod preview;
mod ui;

use clipperino_core::{
    config::{AppConfig, Axis, LayoutNode, PanelId, Theme},
    media,
    project::{Project, ProjectStore, TranscriptEntry},
};
use futures::{
    StreamExt,
    channel::mpsc::{UnboundedSender, unbounded},
};
use gpui::{
    App, Application, Bounds, Context, DragMoveEvent, ExternalPaths, FocusHandle, Hsla,
    IntoElement, KeyBinding, ListAlignment, ListState, MouseButton, MouseDownEvent, MouseMoveEvent,
    ObjectFit, PathPromptOptions, Render, RenderImage, SharedString, Timer, Window, WindowBounds,
    WindowOptions, actions, canvas, div, img, list, prelude::*, px, relative, rgb, size,
};
use preview::{PreviewEvent, PreviewPlayer};
use std::{
    cell::Cell,
    fs,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, SystemTime},
};

actions!(
    clipperino,
    [
        TogglePlay,
        StepBack,
        StepForward,
        JumpBack,
        JumpForward,
        GoToStart,
        GoToEnd,
        MarkIn,
        MarkOut,
        ClearMarks,
        RemoveRange,
        Undo,
    ]
);

#[derive(Clone, Copy)]
struct PanelDrag(PanelId);
#[derive(Clone)]
struct SplitDrag(Vec<bool>);
#[derive(Clone, Copy)]
struct TimelineDrag;
struct DragGhost(&'static str);
struct TimelineGhost;

enum JobEvent {
    Imported(Option<String>, String),
    Transcript(String, Vec<TranscriptEntry>),
    Kept(String, Vec<(u64, u64)>),
    Model(PathBuf),
    Error(String),
}

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().p_2().bg(rgb(0xdddddd)).border_1().child(self.0)
    }
}

impl Render for TimelineGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().w(px(1.0)).h(px(1.0))
    }
}

/// Theme colors parsed once, instead of on every render.
#[derive(Clone, Copy)]
struct Palette {
    background: Hsla,
    panel: Hsla,
    text: Hsla,
    muted_text: Hsla,
    border: Hsla,
    accent: Hsla,
    selected: Hsla,
}

impl Palette {
    fn new(theme: &Theme) -> Self {
        Self {
            background: color(&theme.background),
            panel: color(&theme.panel),
            text: color(&theme.text),
            muted_text: color(&theme.muted_text),
            border: color(&theme.border),
            accent: color(&theme.accent),
            selected: color(&theme.selected),
        }
    }

    fn from_config(config: &AppConfig) -> Self {
        Self::new(&config.resolved_theme().unwrap_or_else(|_| Theme::light()))
    }
}

/// A transcript passage, prepared once per transcript change for cheap rendering.
struct TranscriptRow {
    id: SharedString,
    asset_id: String,
    start_ms: u64,
    time: SharedString,
    text: SharedString,
}

struct Editor {
    store: ProjectStore,
    project: Project,
    config: AppConfig,
    palette: Palette,
    focus_handle: FocusHandle,
    selected_asset: Option<String>,
    /// The playhead while stopped. During playback the preview clock is authoritative.
    preview_at_ms: u64,
    preview_image: Option<Arc<RenderImage>>,
    /// Frames no longer shown, whose GPU textures are released on the next render.
    retired_images: Vec<Arc<RenderImage>>,
    preview: PreviewPlayer,
    job_tx: UnboundedSender<JobEvent>,
    playing: bool,
    scrubbing: bool,
    timeline_bounds: Rc<Cell<(f32, f32)>>,
    mark_in: Option<u64>,
    mark_out: Option<u64>,
    status: SharedString,
    transcript_rows: Rc<[TranscriptRow]>,
    transcript_list: ListState,
    active_row: Option<usize>,
    project_modified: Option<SystemTime>,
    config_modified: Option<SystemTime>,
    theme_modified: Option<SystemTime>,
}

impl Editor {
    fn new(
        store: ProjectStore,
        project: Project,
        config: AppConfig,
        cx: &mut Context<Self>,
    ) -> Self {
        let (job_tx, mut job_rx) = unbounded();
        let (preview, mut preview_rx) = PreviewPlayer::new();
        cx.spawn(async move |this, cx| {
            while let Some(event) = job_rx.next().await {
                if this
                    .update(cx, |editor, cx| editor.handle_job(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            while let Some(event) = preview_rx.next().await {
                if this
                    .update(cx, |editor, cx| editor.handle_preview(event, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        cx.spawn(async move |this, cx| {
            loop {
                Timer::after(Duration::from_millis(500)).await;
                if this
                    .update(cx, |editor, cx| editor.check_files(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();

        let mut editor = Self {
            palette: Palette::from_config(&config),
            focus_handle: cx.focus_handle(),
            selected_asset: project.assets.first().map(|asset| asset.id.clone()),
            project_modified: modified(store.path()),
            config_modified: AppConfig::path().ok().and_then(|path| modified(&path)),
            theme_modified: theme_modified(&config),
            store,
            project,
            config,
            preview_at_ms: 0,
            preview_image: None,
            retired_images: Vec::new(),
            preview,
            job_tx,
            playing: false,
            scrubbing: false,
            timeline_bounds: Rc::new(Cell::new((0.0, 0.0))),
            mark_in: None,
            mark_out: None,
            status: "Ready".into(),
            transcript_rows: Rc::from([]),
            transcript_list: ListState::new(0, ListAlignment::Top, px(400.0)),
            active_row: None,
        };
        editor.rebuild_transcript();
        editor.refresh_frame();
        editor
    }

    fn handle_job(&mut self, event: JobEvent, cx: &mut Context<Self>) {
        match event {
            JobEvent::Imported(asset_id, message) => {
                if asset_id.is_some() {
                    self.selected_asset = asset_id;
                }
                self.status = message.into();
                self.reload_project();
            }
            JobEvent::Transcript(asset_id, entries) => {
                let count = entries.len();
                let result = self.store.update(|project| {
                    project
                        .transcript
                        .retain(|entry| entry.asset_id != asset_id);
                    project.transcript.extend(entries);
                    Ok(())
                });
                self.status = match result {
                    Ok(()) => format!("Transcribed {count} passages").into(),
                    Err(error) => error.to_string().into(),
                };
                self.reload_project();
            }
            JobEvent::Kept(asset_id, ranges) => {
                let result = self
                    .store
                    .update(|project| project.replace_asset_with_kept_ranges(&asset_id, &ranges));
                self.status = match result {
                    Ok(()) => format!("Kept {} ranges from {asset_id}", ranges.len()).into(),
                    Err(error) => error.to_string().into(),
                };
                self.reload_project();
            }
            JobEvent::Model(path) => {
                self.status = match AppConfig::update(|config| {
                    config.model_path = Some(path);
                    Ok(())
                }) {
                    Ok((config, _)) => {
                        self.config = config;
                        "Local English model ready".into()
                    }
                    Err(error) => error.to_string().into(),
                };
                self.config_modified = AppConfig::path().ok().and_then(|path| modified(&path));
            }
            JobEvent::Error(error) => self.status = error.into(),
        }
        cx.notify();
    }

    fn handle_preview(&mut self, event: PreviewEvent, cx: &mut Context<Self>) {
        match event {
            PreviewEvent::FrameReady => {
                if let Some(image) = self.preview.take_frame() {
                    self.retire_image(Some(image));
                    cx.notify();
                }
            }
            PreviewEvent::Finished { generation, at_ms } if self.preview.is_current(generation) => {
                self.playing = false;
                self.preview_at_ms = at_ms.min(self.last_ms());
                cx.notify();
            }
            PreviewEvent::Error {
                generation,
                message,
            } if self.preview.is_current(generation) => {
                self.stop_playback();
                self.status = message.into();
                cx.notify();
            }
            _ => {}
        }
    }

    /// Replaces the shown frame, queueing the old one for GPU release.
    fn retire_image(&mut self, image: Option<Arc<RenderImage>>) {
        if let Some(old) = std::mem::replace(&mut self.preview_image, image) {
            self.retired_images.push(old);
        }
    }

    fn check_files(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        let project_modified = modified(self.store.path());
        if project_modified != self.project_modified {
            self.project_modified = project_modified;
            if let Ok(project) = self.store.load() {
                self.set_project(project);
                changed = true;
            }
        }
        let config_modified = AppConfig::path().ok().and_then(|path| modified(&path));
        if config_modified != self.config_modified {
            self.config_modified = config_modified;
            if let Ok(config) = AppConfig::load() {
                let word_timestamps_changed =
                    config.transcript_word_timestamps != self.config.transcript_word_timestamps;
                self.palette = Palette::from_config(&config);
                self.theme_modified = theme_modified(&config);
                self.config = config;
                if word_timestamps_changed {
                    self.rebuild_transcript();
                }
                changed = true;
            }
        }
        let theme_modified = theme_modified(&self.config);
        if theme_modified != self.theme_modified {
            self.theme_modified = theme_modified;
            if let Ok(theme) = self.config.resolved_theme() {
                self.palette = Palette::new(&theme);
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    fn reload_project(&mut self) {
        match self.store.load() {
            Ok(project) => self.set_project(project),
            Err(error) => self.status = error.to_string().into(),
        }
    }

    /// Applies a newly loaded project, touching playback and the transcript only if needed.
    fn set_project(&mut self, project: Project) {
        self.project_modified = modified(self.store.path());
        let timeline_changed =
            project.segments != self.project.segments || project.assets != self.project.assets;
        let transcript_changed = project.transcript != self.project.transcript;
        self.project = project;
        if self
            .selected_asset
            .as_ref()
            .is_none_or(|id| self.project.asset(id).is_err())
        {
            self.selected_asset = self.project.assets.first().map(|asset| asset.id.clone());
        }
        if transcript_changed || timeline_changed {
            self.rebuild_transcript();
        }
        if timeline_changed {
            let resume = self.playing;
            self.stop_playback();
            self.preview_at_ms = self.preview_at_ms.min(self.last_ms());
            if resume {
                self.start_playback();
            } else {
                self.refresh_frame();
            }
        }
    }

    fn rebuild_transcript(&mut self) {
        let mut entries: Vec<_> = self
            .project
            .transcript
            .iter()
            .filter(|entry| self.selected_asset.as_deref() == Some(entry.asset_id.as_str()))
            .collect();
        entries.sort_by_key(|entry| entry.source_start_ms);
        let group_size = if self.config.transcript_word_timestamps {
            8
        } else {
            1
        };
        self.transcript_rows = entries
            .chunks(group_size)
            .map(|group| {
                let first = group[0];
                TranscriptRow {
                    id: first.id.clone().into(),
                    asset_id: first.asset_id.clone(),
                    start_ms: first.source_start_ms,
                    time: time_label(first.source_start_ms).into(),
                    text: group
                        .iter()
                        .map(|entry| entry.text.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                        .into(),
                }
            })
            .collect();
        self.transcript_list.reset(self.transcript_rows.len());
        self.active_row = None;
    }

    fn select_asset(&mut self, asset_id: String) {
        if self.selected_asset.as_ref() != Some(&asset_id) {
            self.selected_asset = Some(asset_id);
            self.rebuild_transcript();
        }
    }

    /// Highlights the transcript passage under the playhead and keeps it in view.
    fn follow_playhead(&mut self, playhead: u64) {
        let Ok((asset, source_ms)) = media::timeline_source(&self.project, playhead) else {
            self.active_row = None;
            return;
        };
        if self.playing && self.selected_asset.as_deref() != Some(asset.id.as_str()) {
            let id = asset.id.clone();
            self.select_asset(id);
        }
        let active = self
            .transcript_rows
            .partition_point(|row| row.start_ms <= source_ms)
            .checked_sub(1);
        if active != self.active_row {
            self.active_row = active;
            if self.playing
                && let Some(index) = active
            {
                self.transcript_list.scroll_to_reveal_item(index);
            }
        }
    }

    fn last_ms(&self) -> u64 {
        self.project.duration_ms().saturating_sub(1)
    }

    fn playhead_ms(&self) -> u64 {
        if self.playing {
            self.preview.position_ms().unwrap_or(self.preview_at_ms)
        } else {
            self.preview_at_ms
        }
    }

    fn refresh_frame(&mut self) {
        match preview::plan(&self.project, self.store.path(), self.preview_at_ms)
            .into_iter()
            .next()
        {
            Some(part) => self.preview.show_frame(part, &self.config),
            None => {
                self.preview.stop();
                self.retire_image(None);
            }
        }
    }

    fn start_playback(&mut self) {
        if self.project.duration_ms() == 0 {
            return;
        }
        if self.preview_at_ms + 100 >= self.project.duration_ms() {
            self.preview_at_ms = 0;
        }
        let parts = preview::plan(&self.project, self.store.path(), self.preview_at_ms);
        self.preview.play(parts, self.config.clone());
        self.playing = true;
    }

    fn stop_playback(&mut self) {
        let position = self.preview.stop();
        if self.playing {
            self.playing = false;
            if let Some(position) = position {
                self.preview_at_ms = position;
            }
        }
    }

    fn seek(&mut self, at_ms: u64, cx: &mut Context<Self>) {
        let resume = self.playing;
        self.stop_playback();
        self.scrubbing = false;
        self.preview_at_ms = at_ms.min(self.last_ms());
        if resume {
            self.start_playback();
        } else {
            self.refresh_frame();
        }
        cx.notify();
    }

    fn seek_by(&mut self, delta_ms: i64, cx: &mut Context<Self>) {
        let at = self.playhead_ms().saturating_add_signed(delta_ms);
        self.seek(at, cx);
    }

    fn begin_scrub(&mut self, position_x: f32, cx: &mut Context<Self>) {
        self.stop_playback();
        self.scrubbing = true;
        self.scrub_to(position_x, cx);
    }

    fn scrub_to(&mut self, position_x: f32, cx: &mut Context<Self>) {
        let (left, width) = self.timeline_bounds.get();
        if width <= 0.0 || self.project.duration_ms() == 0 {
            return;
        }
        let fraction = ((position_x - left) / width).clamp(0.0, 1.0);
        let at_ms =
            ((fraction * self.project.duration_ms() as f32).round() as u64).min(self.last_ms());
        if at_ms != self.preview_at_ms {
            self.preview_at_ms = at_ms;
            // The still decoder coalesces requests, so every move can ask for a frame.
            self.refresh_frame();
            cx.notify();
        }
    }

    fn finish_scrub(&mut self, cx: &mut Context<Self>) {
        if self.scrubbing {
            self.scrubbing = false;
            cx.notify();
        }
    }

    fn seek_source(&mut self, asset_id: &str, source_ms: u64, cx: &mut Context<Self>) {
        match self.project.source_to_timeline(asset_id, source_ms) {
            Some(at) => self.seek(at, cx),
            None => {
                self.status = "That transcript line was removed from the timeline".into();
                cx.notify();
            }
        }
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        if self.playing {
            self.stop_playback();
        } else {
            self.start_playback();
        }
        cx.notify();
    }

    fn set_mark(&mut self, out: bool, cx: &mut Context<Self>) {
        let at = Some(self.playhead_ms());
        if out {
            self.mark_out = at;
        } else {
            self.mark_in = at;
        }
        cx.notify();
    }

    fn cut_marks(&mut self, cx: &mut Context<Self>) {
        let (Some(start), Some(end)) = (self.mark_in, self.mark_out) else {
            self.status = "Set In and Out marks first".into();
            cx.notify();
            return;
        };
        let (start, end) = (start.min(end), start.max(end));
        match self
            .store
            .update(|project| project.remove_timeline_range(start, end))
        {
            Ok(()) => {
                self.mark_in = None;
                self.mark_out = None;
                self.status = "Range removed".into();
                self.preview_at_ms = start;
                self.reload_project();
                self.seek(start, cx);
            }
            Err(error) => {
                self.status = error.to_string().into();
                cx.notify();
            }
        }
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        match self.store.undo() {
            Ok(_) => {
                self.status = "Undid last edit".into();
                self.reload_project();
            }
            Err(error) => self.status = error.to_string().into(),
        }
        cx.notify();
    }

    fn import_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if paths.is_empty() {
            return;
        }
        self.status = "Importing…".into();
        let project_path = self.store.path().to_owned();
        let config = self.config.clone();
        let tx = self.job_tx.clone();
        std::thread::spawn(move || {
            let store = ProjectStore::new(project_path);
            let mut last = None;
            let mut messages = Vec::new();
            for path in &paths {
                match media::import(&store, path, &config) {
                    Ok(id) => {
                        messages.push(format!("Imported {}", file_label(path)));
                        last = Some(id);
                    }
                    Err(error) => messages.push(format!("{}: {error}", file_label(path))),
                }
            }
            let _ = tx.unbounded_send(JobEvent::Imported(last, messages.join(" · ")));
        });
        cx.notify();
    }

    fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        let theme = if self.config.theme == "light" {
            "dark"
        } else {
            "light"
        };
        match AppConfig::update(|config| {
            config.theme = theme.into();
            config.custom_theme = None;
            Ok(())
        }) {
            Ok((config, _)) => {
                self.palette = Palette::from_config(&config);
                self.theme_modified = theme_modified(&config);
                self.config = config;
                self.config_modified = AppConfig::path().ok().and_then(|path| modified(&path));
            }
            Err(error) => self.status = error.to_string().into(),
        }
        cx.notify();
    }

    fn persist_layout(&mut self) {
        let layout = self.config.layout.clone();
        match AppConfig::update(|config| {
            config.layout = layout;
            Ok(())
        }) {
            Ok((config, _)) => self.config = config,
            Err(error) => self.status = error.to_string().into(),
        }
        self.config_modified = AppConfig::path().ok().and_then(|path| modified(&path));
    }

    fn download_model(&mut self, cx: &mut Context<Self>) {
        self.status = "Downloading local English model…".into();
        let tx = self.job_tx.clone();
        std::thread::spawn(move || {
            let result = media::default_model_path().and_then(|path| {
                if !path.exists() {
                    media::download_model(&path)?;
                }
                Ok(path)
            });
            let _ = tx.unbounded_send(match result {
                Ok(path) => JobEvent::Model(path),
                Err(error) => JobEvent::Error(error.to_string()),
            });
        });
        cx.notify();
    }

    fn transcribe_selected(&mut self, cx: &mut Context<Self>) {
        let Some(asset_id) = self.selected_asset.clone() else {
            return;
        };
        let Some(model) = self.config.model_path.as_ref() else {
            self.status = "Download the local model first".into();
            cx.notify();
            return;
        };
        if !model.exists() {
            self.status = "Configured model file is missing".into();
            cx.notify();
            return;
        }
        let Ok(asset) = self.project.asset(&asset_id) else {
            return;
        };
        if !asset.has_audio {
            self.status = "This asset has no audio to transcribe".into();
            cx.notify();
            return;
        }
        let input = media::asset_path(self.store.path(), asset);
        let cache = self
            .store
            .path()
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".clipperino-cache");
        let config = self.config.clone();
        let tx = self.job_tx.clone();
        self.status = format!("Transcribing {asset_id}…").into();
        std::thread::spawn(move || {
            let result = media::transcribe(&input, &asset_id, &cache, &config);
            let _ = tx.unbounded_send(match result {
                Ok(entries) => JobEvent::Transcript(asset_id, entries),
                Err(error) => JobEvent::Error(error.to_string()),
            });
        });
        cx.notify();
    }

    fn auto_cut_selected(&mut self, cx: &mut Context<Self>) {
        let Some(asset_id) = self.selected_asset.clone() else {
            return;
        };
        let Ok(asset) = self.project.asset(&asset_id) else {
            return;
        };
        if !asset.has_audio {
            self.status = "This asset has no audio to detect pauses in".into();
            cx.notify();
            return;
        }
        let entries: Vec<_> = self
            .project
            .transcript
            .iter()
            .filter(|entry| entry.asset_id == asset_id)
            .cloned()
            .collect();
        if entries.is_empty() {
            self.status = "Transcribe this asset before auto-cut".into();
            cx.notify();
            return;
        }
        let input = media::asset_path(self.store.path(), asset);
        let settings = self.project.settings.clone();
        let duration = asset.duration_ms;
        let config = self.config.clone();
        let tx = self.job_tx.clone();
        self.status = format!("Finding pauses in {asset_id}…").into();
        std::thread::spawn(move || {
            let result = media::detect_silence(
                &input,
                settings.silence_min_ms,
                settings.silence_threshold_db,
                &config,
            )
            .map(|silence| {
                media::kept_after_silence(duration, &silence, &entries, settings.silence_padding_ms)
            });
            let _ = tx.unbounded_send(match result {
                Ok(ranges) => JobEvent::Kept(asset_id, ranges),
                Err(error) => JobEvent::Error(error.to_string()),
            });
        });
        cx.notify();
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

fn theme_modified(config: &AppConfig) -> Option<SystemTime> {
    config
        .theme_file_path()
        .ok()
        .flatten()
        .and_then(|path| modified(&path))
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn color(hex: &str) -> Hsla {
    rgb(u32::from_str_radix(hex.trim_start_matches('#'), 16).unwrap_or(0)).into()
}
fn panel_title(panel: PanelId) -> &'static str {
    match panel {
        PanelId::Assets => "Assets",
        PanelId::Preview => "Preview",
        PanelId::Transcript => "Transcript",
        PanelId::Timeline => "Timeline",
    }
}
fn control(label: &'static str, palette: &Palette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(label)
        .h(px(30.0))
        .flex()
        .items_center()
        .justify_center()
        .px_3()
        .text_sm()
        .font_weight(gpui::FontWeight::MEDIUM)
        .bg(palette.panel)
        .text_color(palette.text)
        .border_1()
        .border_color(palette.border)
        .rounded_md()
        .cursor_pointer()
        .hover(|this| this.bg(palette.selected))
        .child(label)
}

fn primary_control(label: &'static str, palette: &Palette) -> gpui::Stateful<gpui::Div> {
    div()
        .id(label)
        .h(px(32.0))
        .flex()
        .items_center()
        .justify_center()
        .px_3()
        .text_sm()
        .font_weight(gpui::FontWeight::MEDIUM)
        .bg(palette.text)
        .text_color(palette.panel)
        .rounded_md()
        .cursor_pointer()
        .hover(|this| this.opacity(0.85))
        .child(label)
}
fn time_label(ms: u64) -> String {
    format!(
        "{:02}:{:02}.{:03}",
        ms / 60_000,
        (ms / 1000) % 60,
        ms % 1000
    )
}
fn node_at_mut<'a>(node: &'a mut LayoutNode, path: &[bool]) -> Option<&'a mut LayoutNode> {
    if path.is_empty() {
        return Some(node);
    }
    match node {
        LayoutNode::Split { first, second, .. } => {
            node_at_mut(if path[0] { second } else { first }, &path[1..])
        }
        LayoutNode::Panel { .. } => None,
    }
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("project.json"));
    let store = ProjectStore::new(&path);
    if !path.exists()
        && let Err(error) = store.create()
    {
        eprintln!("Could not create project: {error}");
        std::process::exit(1);
    }
    let project = store.load().unwrap_or_else(|error| {
        eprintln!("Could not open project: {error}");
        std::process::exit(1)
    });
    let config = AppConfig::load().unwrap_or_else(|error| {
        eprintln!("Could not load config: {error}");
        std::process::exit(1)
    });
    Application::new().run(move |cx: &mut App| {
        let context = Some("Editor");
        cx.bind_keys([
            KeyBinding::new("space", TogglePlay, context),
            KeyBinding::new("k", TogglePlay, context),
            KeyBinding::new("left", StepBack, context),
            KeyBinding::new("right", StepForward, context),
            KeyBinding::new("shift-left", JumpBack, context),
            KeyBinding::new("shift-right", JumpForward, context),
            KeyBinding::new("home", GoToStart, context),
            KeyBinding::new("end", GoToEnd, context),
            KeyBinding::new("i", MarkIn, context),
            KeyBinding::new("o", MarkOut, context),
            KeyBinding::new("escape", ClearMarks, context),
            KeyBinding::new("delete", RemoveRange, context),
            KeyBinding::new("backspace", RemoveRange, context),
            KeyBinding::new("ctrl-z", Undo, context),
        ]);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |window, cx| {
                let editor = cx.new(|cx| Editor::new(store, project, config, cx));
                window.focus(&editor.read(cx).focus_handle);
                editor
            },
        )
        .expect("could not open GPUI window");
        cx.activate(true);
    });
}

mod icons;
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
    AnyView, App, Application, Bounds, Context, DragMoveEvent, ExternalPaths, FocusHandle, Hsla,
    IntoElement, KeyBinding, ListAlignment, ListState, Modifiers, MouseButton, MouseDownEvent,
    MouseMoveEvent, ObjectFit, PathPromptOptions, Render, RenderImage, ScrollWheelEvent,
    SharedString, Timer, Window, WindowBounds, WindowOptions, actions, canvas, div, img, list,
    prelude::*, px, relative, rgb, size,
};
use icons::{Assets, Icon};
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
        PrevFrame,
        NextFrame,
        PrevEdit,
        NextEdit,
        MarkIn,
        MarkOut,
        ClearSelection,
        Split,
        TrimLeft,
        TrimRight,
        Delete,
        Undo,
        Redo,
        ZoomIn,
        ZoomOut,
        ZoomFit,
        Import,
        Export,
    ]
);

/// Step used for frame-by-frame movement; preview frame rates vary per source.
const FRAME_MS: i64 = 33;
/// Heights of the ruler and clip lanes, which mouse handling relies on.
const RULER_HEIGHT: f32 = 24.0;
const LANE_HEIGHT: f32 = 52.0;

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
    Exported(PathBuf),
    Error(String),
}

/// Viewport of the timeline track, in window coordinates.
#[derive(Clone, Copy, Default)]
struct TrackBounds {
    left: f32,
    top: f32,
    width: f32,
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

struct Tooltip(SharedString, Palette);

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(self.1.text)
            .text_color(self.1.panel)
            .text_xs()
            .child(self.0.clone())
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
    timeline_bounds: Rc<Cell<TrackBounds>>,
    /// Timeline magnification; 1.0 fits the whole edit in the track.
    zoom: f32,
    /// Horizontal scroll of the zoomed timeline, in pixels.
    scroll_px: f32,
    selected_segments: Vec<String>,
    mark_in: Option<u64>,
    mark_out: Option<u64>,
    exporting: bool,
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
            timeline_bounds: Rc::new(Cell::new(TrackBounds::default())),
            zoom: 1.0,
            scroll_px: 0.0,
            selected_segments: Vec::new(),
            mark_in: None,
            mark_out: None,
            exporting: false,
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
            JobEvent::Exported(path) => {
                self.exporting = false;
                self.status = format!("Exported {}", path.display()).into();
            }
            JobEvent::Error(error) => {
                self.exporting = false;
                self.status = error.into();
            }
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
        let segments = &self.project.segments;
        self.selected_segments
            .retain(|id| segments.iter().any(|segment| &segment.id == id));
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

    fn content_width(&self) -> f32 {
        self.timeline_bounds.get().width * self.zoom
    }

    fn ms_to_x(&self, ms: u64) -> f32 {
        let total = self.project.duration_ms().max(1) as f32;
        ms as f32 / total * self.content_width() - self.scroll_px
    }

    fn x_to_ms(&self, position_x: f32) -> u64 {
        let bounds = self.timeline_bounds.get();
        let content = self.content_width();
        if content <= 0.0 {
            return 0;
        }
        let fraction = ((position_x - bounds.left + self.scroll_px) / content).clamp(0.0, 1.0);
        ((fraction * self.project.duration_ms() as f32).round() as u64).min(self.last_ms())
    }

    fn begin_scrub(
        &mut self,
        position: gpui::Point<gpui::Pixels>,
        modifiers: Modifiers,
        cx: &mut Context<Self>,
    ) {
        self.stop_playback();
        let bounds = self.timeline_bounds.get();
        let lane_y = f32::from(position.y) - bounds.top - RULER_HEIGHT;
        if lane_y >= 0.0 {
            // A click in the clip lanes selects the clip under the pointer.
            let at_ms = self.x_to_ms(f32::from(position.x));
            let clicked = self
                .project
                .segment_at(at_ms)
                .filter(|_| {
                    (f32::from(position.x) - bounds.left) < self.ms_to_x(self.project.duration_ms())
                })
                .map(|(index, _)| self.project.segments[index].id.clone());
            let additive = modifiers.shift || modifiers.control || modifiers.platform;
            match clicked {
                Some(id) if additive => {
                    if let Some(index) = self.selected_segments.iter().position(|s| *s == id) {
                        self.selected_segments.remove(index);
                    } else {
                        self.selected_segments.push(id);
                    }
                }
                Some(id) => self.selected_segments = vec![id],
                None if !additive => self.selected_segments.clear(),
                None => {}
            }
        }
        self.scrubbing = true;
        self.scrub_to(f32::from(position.x), cx);
        cx.notify();
    }

    fn scrub_to(&mut self, position_x: f32, cx: &mut Context<Self>) {
        if self.timeline_bounds.get().width <= 0.0 || self.project.duration_ms() == 0 {
            return;
        }
        let at_ms = self.x_to_ms(position_x);
        if at_ms != self.preview_at_ms {
            self.preview_at_ms = at_ms;
            // The still decoder coalesces requests, so every move can ask for a frame.
            self.refresh_frame();
            cx.notify();
        }
    }

    fn max_zoom(&self) -> f32 {
        // Allow zooming in until a pixel is about 5 ms.
        let width = self.timeline_bounds.get().width.max(1.0);
        (self.project.duration_ms() as f32 / (width * 5.0)).max(1.0)
    }

    fn clamp_scroll(&mut self) {
        let max = (self.content_width() - self.timeline_bounds.get().width).max(0.0);
        self.scroll_px = self.scroll_px.clamp(0.0, max);
    }

    /// Zooms by `factor`, keeping the moment at `anchor_ms` under the same screen position.
    fn zoom_by(&mut self, factor: f32, anchor_ms: u64, cx: &mut Context<Self>) {
        let anchor_x = self.ms_to_x(anchor_ms);
        self.zoom = (self.zoom * factor).clamp(1.0, self.max_zoom());
        let total = self.project.duration_ms().max(1) as f32;
        self.scroll_px = anchor_ms as f32 / total * self.content_width() - anchor_x;
        self.clamp_scroll();
        cx.notify();
    }

    fn zoom_to_fit(&mut self, cx: &mut Context<Self>) {
        self.zoom = 1.0;
        self.scroll_px = 0.0;
        cx.notify();
    }

    fn scroll_timeline(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(20.0));
        let (dx, dy) = (f32::from(delta.x), f32::from(delta.y));
        if event.modifiers.control || event.modifiers.platform {
            let anchor = self.x_to_ms(f32::from(event.position.x));
            self.zoom_by((dy * 0.005).exp(), anchor, cx);
        } else {
            // Vertical wheels scroll the timeline sideways, as in most editors.
            self.scroll_px -= if dx.abs() > dy.abs() { dx } else { dy };
            self.clamp_scroll();
            cx.notify();
        }
    }

    /// Scrolls a zoomed timeline so the playhead stays in view during playback.
    fn keep_playhead_visible(&mut self, playhead: u64) {
        let width = self.timeline_bounds.get().width;
        if self.zoom <= 1.0 || width <= 0.0 {
            return;
        }
        let x = self.ms_to_x(playhead);
        if x < 0.0 || x > width * 0.95 {
            self.scroll_px += x - width * 0.1;
            self.clamp_scroll();
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

    /// Runs one undoable project edit and applies the result.
    fn apply_edit<T>(
        &mut self,
        message: impl Into<SharedString>,
        edit: impl FnOnce(&mut Project) -> clipperino_core::Result<T>,
    ) -> Option<T> {
        match self.store.update(edit) {
            Ok(value) => {
                self.status = message.into();
                self.reload_project();
                Some(value)
            }
            Err(error) => {
                self.status = sentence(&error.to_string()).into();
                None
            }
        }
    }

    fn split(&mut self, cx: &mut Context<Self>) {
        let at = self.playhead_ms();
        let message = format!("Split at {}", time_label(at));
        if self
            .apply_edit(message, |project| project.split_at(at))
            .is_some()
        {
            self.selected_segments.clear();
        }
        cx.notify();
    }

    /// Removes the current clip's material before (`left`) or after the playhead.
    fn trim(&mut self, left: bool, cx: &mut Context<Self>) {
        let at = self.playhead_ms();
        let clip_start = self.project.segment_at(at).map_or(at, |(_, start)| start);
        let message = if left {
            "Removed the clip before the playhead"
        } else {
            "Removed the clip after the playhead"
        };
        if self
            .apply_edit(message, |project| project.trim_at(at, left))
            .is_some()
        {
            self.seek(if left { clip_start } else { at }, cx);
        }
        cx.notify();
    }

    /// Deletes the selected clips, or the In/Out range when no clip is selected.
    fn delete(&mut self, cx: &mut Context<Self>) {
        if !self.selected_segments.is_empty() {
            let ids = std::mem::take(&mut self.selected_segments);
            let mut position = 0;
            let mut first_start = None;
            for segment in &self.project.segments {
                if first_start.is_none() && ids.contains(&segment.id) {
                    first_start = Some(position);
                }
                position += segment.duration_ms();
            }
            let message = if ids.len() == 1 {
                "Deleted clip".to_owned()
            } else {
                format!("Deleted {} clips", ids.len())
            };
            if self
                .apply_edit(message, |project| project.remove_segments(&ids))
                .is_some()
            {
                self.seek(first_start.unwrap_or(0), cx);
            }
        } else if let (Some(a), Some(b)) = (self.mark_in, self.mark_out) {
            let (start, end) = (a.min(b), a.max(b));
            if self
                .apply_edit("Removed the marked range", |project| {
                    project.remove_timeline_range(start, end)
                })
                .is_some()
            {
                self.mark_in = None;
                self.mark_out = None;
                self.seek(start, cx);
            }
        } else {
            self.status =
                "Click a clip to select it, or press S to split at the playhead first".into();
        }
        cx.notify();
    }

    fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.selected_segments.clear();
        self.mark_in = None;
        self.mark_out = None;
        cx.notify();
    }

    /// Moves the playhead to the previous or next cut.
    fn jump_to_edit(&mut self, forward: bool, cx: &mut Context<Self>) {
        let at = self.playhead_ms();
        let mut points = vec![0];
        points.extend(self.project.cut_points());
        let target = if forward {
            points.into_iter().find(|point| *point > at)
        } else {
            points.into_iter().rev().find(|point| *point + 1 < at)
        };
        if let Some(target) = target {
            self.seek(target, cx);
        }
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        match self.store.undo() {
            Ok(_) => {
                self.status = "Undid last edit".into();
                self.reload_project();
            }
            Err(error) => self.status = sentence(&error.to_string()).into(),
        }
        cx.notify();
    }

    fn redo(&mut self, cx: &mut Context<Self>) {
        match self.store.redo() {
            Ok(_) => {
                self.status = "Redid edit".into();
                self.reload_project();
            }
            Err(error) => self.status = sentence(&error.to_string()).into(),
        }
        cx.notify();
    }

    fn prompt_import(&mut self, cx: &mut Context<Self>) {
        let answer = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: true,
            prompt: Some("Import video".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = answer.await {
                let _ = this.update(cx, |this, cx| this.import_paths(paths, cx));
            }
        })
        .detach();
    }

    fn prompt_export(&mut self, cx: &mut Context<Self>) {
        if self.exporting {
            self.status = "An export is already running".into();
            cx.notify();
            return;
        }
        if self.project.segments.is_empty() {
            self.status = "Add a video before exporting".into();
            cx.notify();
            return;
        }
        let directory = fs::canonicalize(self.store.path())
            .ok()
            .and_then(|path| path.parent().map(Path::to_owned))
            .unwrap_or_else(|| PathBuf::from("."));
        let answer = cx.prompt_for_new_path(&directory, Some("export.mp4"));
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(path))) = answer.await {
                let _ = this.update(cx, |this, cx| this.export(path, cx));
            }
        })
        .detach();
    }

    fn export(&mut self, output: PathBuf, cx: &mut Context<Self>) {
        self.exporting = true;
        self.status = format!("Exporting {}…", file_label(&output)).into();
        let project = self.project.clone();
        let project_path = self.store.path().to_owned();
        let config = self.config.clone();
        let tx = self.job_tx.clone();
        std::thread::spawn(move || {
            let result = media::render(&project, &project_path, &output, &config);
            let _ = tx.unbounded_send(match result {
                Ok(()) => JobEvent::Exported(output),
                Err(error) => JobEvent::Error(format!("Export failed: {error}")),
            });
        });
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

/// Capitalizes core error messages, which are lowercase for CLI output.
fn sentence(message: &str) -> String {
    let mut chars = message.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
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
fn tooltip(text: &'static str, palette: Palette) -> impl Fn(&mut Window, &mut App) -> AnyView {
    move |_, cx| cx.new(|_| Tooltip(text.into(), palette)).into()
}

/// A bordered button with an optional icon and label.
fn button(
    id: &'static str,
    icon: Option<Icon>,
    label: &'static str,
    palette: &Palette,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(30.0))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap_2()
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
        .active(|this| this.opacity(0.8))
        .when_some(icon, |this, icon| {
            this.child(icon.element().text_color(palette.text))
        })
        .when(!label.is_empty(), |this| {
            this.min_w_0()
                .child(div().min_w_0().truncate().child(label))
        })
}

/// A square, borderless icon button with a hover tooltip.
fn icon_button(
    id: &'static str,
    icon: Icon,
    hint: &'static str,
    palette: &Palette,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(30.0))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .rounded_md()
        .cursor_pointer()
        .text_color(palette.text)
        .hover(|this| this.bg(palette.selected))
        .active(|this| this.opacity(0.7))
        .child(icon.element().text_color(palette.text))
        .tooltip(tooltip(hint, *palette))
}

fn primary_button(
    id: &'static str,
    icon: Option<Icon>,
    label: &'static str,
    palette: &Palette,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(32.0))
        .flex()
        .flex_none()
        .items_center()
        .justify_center()
        .gap_2()
        .px_3()
        .text_sm()
        .font_weight(gpui::FontWeight::MEDIUM)
        .bg(palette.text)
        .text_color(palette.panel)
        .rounded_md()
        .cursor_pointer()
        .hover(|this| this.opacity(0.85))
        .active(|this| this.opacity(0.7))
        .when_some(icon, |this, icon| {
            this.child(icon.element().text_color(palette.panel))
        })
        .when(!label.is_empty(), |this| this.child(label))
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

fn key_bindings() -> Vec<KeyBinding> {
    let context = Some("Editor");
    vec![
        KeyBinding::new("space", TogglePlay, context),
        KeyBinding::new("k", TogglePlay, context),
        KeyBinding::new("left", StepBack, context),
        KeyBinding::new("right", StepForward, context),
        KeyBinding::new("shift-left", JumpBack, context),
        KeyBinding::new("shift-right", JumpForward, context),
        KeyBinding::new("home", GoToStart, context),
        KeyBinding::new("end", GoToEnd, context),
        KeyBinding::new(",", PrevFrame, context),
        KeyBinding::new(".", NextFrame, context),
        KeyBinding::new("up", PrevEdit, context),
        KeyBinding::new("down", NextEdit, context),
        KeyBinding::new("i", MarkIn, context),
        KeyBinding::new("o", MarkOut, context),
        KeyBinding::new("escape", ClearSelection, context),
        KeyBinding::new("s", Split, context),
        KeyBinding::new("ctrl-k", Split, context),
        KeyBinding::new("q", TrimLeft, context),
        KeyBinding::new("w", TrimRight, context),
        KeyBinding::new("delete", Delete, context),
        KeyBinding::new("backspace", Delete, context),
        KeyBinding::new("ctrl-z", Undo, context),
        KeyBinding::new("ctrl-shift-z", Redo, context),
        KeyBinding::new("ctrl-y", Redo, context),
        KeyBinding::new("=", ZoomIn, context),
        KeyBinding::new("+", ZoomIn, context),
        KeyBinding::new("-", ZoomOut, context),
        KeyBinding::new("shift-z", ZoomFit, context),
        KeyBinding::new("ctrl-i", Import, context),
        KeyBinding::new("ctrl-e", Export, context),
    ]
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
    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            cx.bind_keys(key_bindings());
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

#[cfg(test)]
mod tests {
    #[test]
    fn key_bindings_parse() {
        assert!(super::key_bindings().len() > 20);
    }
}

mod preview;
mod ui;

use clipperino_core::{
    config::{AppConfig, Axis, LayoutNode, PanelId, Theme},
    media,
    project::{Project, ProjectStore, TranscriptEntry},
};
use gpui::{
    App, Application, Bounds, Context, DragMoveEvent, Hsla, Image, ImageFormat, IntoElement,
    ObjectFit, PathPromptOptions, Render, Timer, Window, WindowBounds, WindowOptions, div, img,
    prelude::*, px, relative, rgb, size,
};
use preview::{PreviewEvent, PreviewPlayer};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        mpsc::{self, Receiver, Sender},
    },
    time::{Duration, SystemTime},
};

#[derive(Clone, Copy)]
struct PanelDrag(PanelId);
#[derive(Clone)]
struct SplitDrag(Vec<bool>);
struct DragGhost(&'static str);

enum JobEvent {
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

struct Editor {
    store: ProjectStore,
    project: Project,
    config: AppConfig,
    theme: Theme,
    selected_asset: Option<String>,
    preview_at_ms: u64,
    preview_image: Option<Arc<Image>>,
    preview: PreviewPlayer,
    job_tx: Sender<JobEvent>,
    job_rx: Receiver<JobEvent>,
    playing: bool,
    mark_in: Option<u64>,
    mark_out: Option<u64>,
    status: String,
    project_modified: Option<SystemTime>,
    config_modified: Option<SystemTime>,
    theme_modified: Option<SystemTime>,
}

impl Editor {
    fn new(store: ProjectStore, project: Project, config: AppConfig) -> Self {
        let theme = config.resolved_theme().unwrap_or_else(|_| Theme::light());
        let selected_asset = project.assets.first().map(|asset| asset.id.clone());
        let project_modified = modified(store.path());
        let config_modified = AppConfig::path().ok().and_then(|path| modified(&path));
        let theme_modified = config
            .theme_file_path()
            .ok()
            .flatten()
            .and_then(|path| modified(&path));
        let (job_tx, job_rx) = mpsc::channel();
        Self {
            store,
            project,
            config,
            theme,
            selected_asset,
            preview_at_ms: 0,
            preview_image: None,
            preview: PreviewPlayer::new(),
            job_tx,
            job_rx,
            playing: false,
            mark_in: None,
            mark_out: None,
            status: "Ready".into(),
            project_modified,
            config_modified,
            theme_modified,
        }
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        while let Ok(event) = self.job_rx.try_recv() {
            match event {
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
                        Ok(()) => format!("Transcribed {count} passages"),
                        Err(error) => error.to_string(),
                    };
                }
                JobEvent::Kept(asset_id, ranges) => {
                    let result = self.store.update(|project| {
                        project.replace_asset_with_kept_ranges(&asset_id, &ranges)
                    });
                    self.status = match result {
                        Ok(()) => format!("Kept {} ranges from {asset_id}", ranges.len()),
                        Err(error) => error.to_string(),
                    };
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
                        Err(error) => error.to_string(),
                    };
                    self.config_modified = AppConfig::path().ok().and_then(|path| modified(&path));
                }
                JobEvent::Error(error) => self.status = error,
            }
            changed = true;
        }
        while let Some(event) = self.preview.try_recv() {
            match event {
                PreviewEvent::Frame {
                    generation,
                    at_ms,
                    bytes,
                } if self.preview.is_current(generation) => {
                    let format = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
                        ImageFormat::Png
                    } else {
                        ImageFormat::Jpeg
                    };
                    self.preview_image = Some(Arc::new(Image::from_bytes(format, bytes)));
                    self.preview_at_ms = at_ms;
                    changed = true;
                }
                PreviewEvent::Finished { generation } if self.preview.is_current(generation) => {
                    self.playing = false;
                    changed = true;
                }
                PreviewEvent::Error {
                    generation,
                    message,
                } if self.preview.is_current(generation) => {
                    self.playing = false;
                    self.status = message;
                    changed = true;
                }
                _ => {}
            }
        }
        let project_modified = modified(self.store.path());
        if project_modified != self.project_modified
            && let Ok(project) = self.store.load()
        {
            self.preview.stop();
            self.playing = false;
            self.project = project;
            self.project_modified = project_modified;
            self.preview_at_ms = self
                .preview_at_ms
                .min(self.project.duration_ms().saturating_sub(1));
            self.refresh_frame();
            changed = true;
        }
        let config_modified = AppConfig::path().ok().and_then(|path| modified(&path));
        if config_modified != self.config_modified
            && let Ok(config) = AppConfig::load()
        {
            self.theme = config.resolved_theme().unwrap_or_else(|_| Theme::light());
            self.config = config;
            self.config_modified = config_modified;
            self.theme_modified = self
                .config
                .theme_file_path()
                .ok()
                .flatten()
                .and_then(|path| modified(&path));
            changed = true;
        }
        let theme_modified = self
            .config
            .theme_file_path()
            .ok()
            .flatten()
            .and_then(|path| modified(&path));
        if theme_modified != self.theme_modified
            && let Ok(theme) = self.config.resolved_theme()
        {
            self.theme = theme;
            self.theme_modified = theme_modified;
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    fn refresh_frame(&mut self) {
        if self.project.duration_ms() > 0 {
            self.preview.show_frame(
                self.project.clone(),
                self.store.path(),
                self.preview_at_ms,
                self.config.clone(),
            );
        } else {
            self.preview_image = None;
        }
    }

    fn seek(&mut self, at_ms: u64, cx: &mut Context<Self>) {
        self.playing = false;
        self.preview_at_ms = at_ms.min(self.project.duration_ms().saturating_sub(1));
        self.refresh_frame();
        cx.notify();
    }

    fn seek_source(&mut self, asset_id: &str, source_ms: u64, cx: &mut Context<Self>) {
        let mut at = 0;
        for segment in &self.project.segments {
            if segment.asset_id == asset_id
                && (segment.source_start_ms..segment.source_end_ms).contains(&source_ms)
            {
                self.seek(at + source_ms - segment.source_start_ms, cx);
                return;
            }
            at += segment.duration_ms();
        }
        self.status = "That transcript line was removed from the timeline".into();
        cx.notify();
    }

    fn toggle_play(&mut self, cx: &mut Context<Self>) {
        if self.playing {
            self.preview.stop();
            self.playing = false;
        } else if self.project.duration_ms() > 0 {
            self.preview.play(
                self.project.clone(),
                self.store.path(),
                self.preview_at_ms,
                self.config.clone(),
            );
            self.playing = true;
        }
        cx.notify();
    }

    fn cut_marks(&mut self, cx: &mut Context<Self>) {
        let (Some(start), Some(end)) = (self.mark_in, self.mark_out) else {
            self.status = "Set In and Out marks first".into();
            cx.notify();
            return;
        };
        match self
            .store
            .update(|project| project.remove_timeline_range(start.min(end), start.max(end)))
        {
            Ok(()) => {
                self.project = self.store.load().unwrap_or_else(|_| self.project.clone());
                self.project_modified = modified(self.store.path());
                self.mark_in = None;
                self.mark_out = None;
                self.status = "Range removed".into();
                self.seek(start.min(end), cx);
            }
            Err(error) => {
                self.status = error.to_string();
                cx.notify();
            }
        }
    }

    fn undo(&mut self, cx: &mut Context<Self>) {
        self.status = match self.store.undo() {
            Ok(_) => {
                self.project = self.store.load().unwrap_or_else(|_| self.project.clone());
                self.project_modified = modified(self.store.path());
                self.seek(self.preview_at_ms, cx);
                "Undid last edit".into()
            }
            Err(error) => error.to_string(),
        };
        cx.notify();
    }

    fn import_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        for path in paths {
            match fs::canonicalize(&path)
                .map_err(Into::into)
                .and_then(|path| media::probe(&path, &self.config))
                .and_then(|asset| self.store.update(|project| project.add_asset(asset)))
            {
                Ok(id) => {
                    self.selected_asset = Some(id);
                    self.status = format!("Imported {}", path.display());
                }
                Err(error) => self.status = error.to_string(),
            }
        }
        if let Ok(project) = self.store.load() {
            self.project = project;
            self.project_modified = modified(self.store.path());
            self.refresh_frame();
        }
        cx.notify();
    }

    fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        let theme = if self.config.theme == "light" {
            "dark".into()
        } else {
            "light".into()
        };
        match AppConfig::update(|config| {
            config.theme = theme;
            config.custom_theme = None;
            Ok(())
        }) {
            Ok((config, _)) => {
                self.config = config;
                self.theme = self
                    .config
                    .resolved_theme()
                    .unwrap_or_else(|_| Theme::light());
                self.config_modified = AppConfig::path().ok().and_then(|path| modified(&path));
            }
            Err(error) => self.status = error.to_string(),
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
            Err(error) => self.status = error.to_string(),
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
            let _ = tx.send(match result {
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
        let input = media::asset_path(self.store.path(), asset);
        let cache = self
            .store
            .path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(".clipperino-cache");
        let config = self.config.clone();
        let tx = self.job_tx.clone();
        self.status = format!("Transcribing {asset_id}…");
        std::thread::spawn(move || {
            let result = media::transcribe(&input, &asset_id, &cache, &config);
            let _ = tx.send(match result {
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
        self.status = format!("Finding pauses in {asset_id}…");
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
            let _ = tx.send(match result {
                Ok(ranges) => JobEvent::Kept(asset_id, ranges),
                Err(error) => JobEvent::Error(error.to_string()),
            });
        });
        cx.notify();
    }
}

fn modified(path: &std::path::Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
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
fn control(label: &'static str, theme: &Theme) -> gpui::Stateful<gpui::Div> {
    div()
        .id(label)
        .px_2()
        .py_1()
        .border_1()
        .border_color(color(&theme.border))
        .rounded_sm()
        .cursor_pointer()
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
        let bounds = Bounds::centered(None, size(px(1280.0), px(800.0)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |_, cx| {
                cx.new(|cx| {
                    let mut editor: Editor = Editor::new(store, project, config);
                    editor.refresh_frame();
                    cx.spawn(async move |this, cx| {
                        loop {
                            Timer::after(Duration::from_millis(80)).await;
                            if this
                                .update(cx, |editor: &mut Editor, cx| editor.poll(cx))
                                .is_err()
                            {
                                break;
                            }
                        }
                    })
                    .detach();
                    editor
                })
            },
        )
        .expect("could not open GPUI window");
        cx.activate(true);
    });
}

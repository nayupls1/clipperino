use std::{fs, path::PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use clipperino_core::{
    Result,
    config::{AppConfig, PanelId, Theme},
    error, media,
    project::ProjectStore,
};
use serde_json::{Value, json};

#[derive(Parser)]
#[command(version, about = "Local, transcript-led video editing")]
struct Args {
    #[arg(long, default_value = "project.json", global = true)]
    project: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a project.json file.
    New,
    /// Add a source video and append it to the timeline.
    Import { path: PathBuf },
    /// Inspect the full project, including source timestamps.
    Inspect,
    /// Read or update a project field with a JSON Pointer.
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
    /// Inspect the assembled timeline with computed positions.
    Timeline,
    /// Transcribe one asset locally with whisper-cli.
    Transcribe { asset: String },
    /// Remove detected silences from one untouched asset.
    AutoCut {
        asset: String,
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove a timeline range and ripple later material left.
    Cut { start_ms: u64, end_ms: u64 },
    /// Restore the project state before the latest edit.
    Undo,
    /// Extract a PNG frame from a source asset or the assembled timeline.
    Frame {
        #[arg(long)]
        asset: Option<String>,
        #[arg(long)]
        at_ms: u64,
        #[arg(long)]
        output: PathBuf,
    },
    /// Make a grid of source frames for quick visual inspection.
    ContactSheet {
        asset: String,
        #[arg(long, default_value_t = 5000)]
        every_ms: u64,
        #[arg(long, default_value_t = 4)]
        columns: u32,
        #[arg(long)]
        output: PathBuf,
    },
    /// Render the current timeline to a video file.
    Render { output: PathBuf },
    /// Download the default English transcription model locally.
    ModelDownload,
    /// Read or update JSON user settings.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Export a simple editable JSON theme.
    ThemeExport {
        #[arg(value_enum)]
        theme: ThemeName,
        output: PathBuf,
    },
    /// Swap two docked panels.
    LayoutSwap {
        #[arg(value_enum)]
        first: PanelName,
        #[arg(value_enum)]
        second: PanelName,
    },
    /// Print example project, settings, theme, and layout structures.
    Schema,
}

#[derive(Subcommand)]
enum ConfigCommand {
    Show,
    Get {
        pointer: String,
    },
    /// Set a field using a JSON Pointer and a JSON value.
    Set {
        pointer: String,
        value: String,
    },
}

#[derive(Subcommand)]
enum ProjectCommand {
    Get { pointer: String },
    Set { pointer: String, value: String },
}

#[derive(Clone, Copy, ValueEnum)]
enum ThemeName {
    Light,
    Dark,
}

#[derive(Clone, Copy, ValueEnum)]
enum PanelName {
    Assets,
    Preview,
    Transcript,
    Timeline,
}

impl From<PanelName> for PanelId {
    fn from(value: PanelName) -> Self {
        match value {
            PanelName::Assets => Self::Assets,
            PanelName::Preview => Self::Preview,
            PanelName::Transcript => Self::Transcript,
            PanelName::Timeline => Self::Timeline,
        }
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn print(value: &impl serde::Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn run() -> Result<()> {
    let args = Args::parse();
    let store = ProjectStore::new(&args.project);
    let config = AppConfig::load()?;
    match args.command {
        Command::New => {
            store.create()?;
            print(&json!({"project": args.project, "revision": 0}))?;
        }
        Command::Import { path } => {
            let id = media::import(&store, &path, &config)?;
            print(&json!({"asset_id": id}))?;
        }
        Command::Inspect => print(&store.load()?)?,
        Command::Project { command } => match command {
            ProjectCommand::Get { pointer } => {
                let raw = serde_json::to_value(store.load()?)?;
                let value = raw
                    .pointer(&pointer)
                    .ok_or_else(|| error(format!("unknown project pointer: {pointer}")))?;
                print(value)?;
            }
            ProjectCommand::Set { pointer, value } => {
                if !pointer.starts_with('/')
                    || pointer == "/revision"
                    || pointer == "/schema_version"
                {
                    return Err(error(
                        "use a project field pointer other than revision or schema_version",
                    ));
                }
                let value: Value = serde_json::from_str(&value)?;
                store.update(|project| {
                    let mut raw = serde_json::to_value(&*project)?;
                    let slot = raw
                        .pointer_mut(&pointer)
                        .ok_or_else(|| error(format!("unknown project pointer: {pointer}")))?;
                    *slot = value;
                    *project = serde_json::from_value(raw)?;
                    Ok(())
                })?;
                print(&store.load()?)?;
            }
        },
        Command::Timeline => {
            let project = store.load()?;
            let mut start = 0;
            let mut rows = Vec::new();
            for segment in &project.segments {
                let end = start + segment.duration_ms();
                rows.push(json!({
                    "id": segment.id,
                    "asset_id": segment.asset_id,
                    "timeline_start_ms": start,
                    "timeline_end_ms": end,
                    "source_start_ms": segment.source_start_ms,
                    "source_end_ms": segment.source_end_ms,
                }));
                start = end;
            }
            print(&json!({"revision": project.revision, "duration_ms": start, "segments": rows}))?;
        }
        Command::Transcribe { asset } => {
            let project = store.load()?;
            let input = media::asset_path(store.path(), project.asset(&asset)?);
            let cache = args
                .project
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join(".clipperino-cache");
            let entries = media::transcribe(&input, &asset, &cache, &config)?;
            let count = entries.len();
            store.update(|project| {
                project.transcript.retain(|entry| entry.asset_id != asset);
                project.transcript.extend(entries);
                Ok(())
            })?;
            print(&json!({"asset_id": asset, "entries": count}))?;
        }
        Command::AutoCut { asset, dry_run } => {
            let project = store.load()?;
            let source = project.asset(&asset)?;
            if !source.has_audio {
                return Err(error("asset has no audio to detect silence"));
            }
            let entries: Vec<_> = project
                .transcript
                .iter()
                .filter(|entry| entry.asset_id == asset)
                .cloned()
                .collect();
            if entries.is_empty() {
                return Err(error("transcribe this asset before auto-cut"));
            }
            let input = media::asset_path(store.path(), source);
            let silence = media::detect_silence(
                &input,
                project.settings.silence_min_ms,
                project.settings.silence_threshold_db,
                &config,
            )?;
            let kept = media::kept_after_silence(
                source.duration_ms,
                &silence,
                &entries,
                project.settings.silence_padding_ms,
            );
            if !dry_run {
                store.update(|project| project.replace_asset_with_kept_ranges(&asset, &kept))?;
            }
            print(&json!({"asset_id": asset, "dry_run": dry_run, "kept_ranges_ms": kept}))?;
        }
        Command::Cut { start_ms, end_ms } => {
            store.update(|project| project.remove_timeline_range(start_ms, end_ms))?;
            print(&json!({"removed": [start_ms, end_ms]}))?;
        }
        Command::Undo => {
            let revision = store.undo()?;
            print(&json!({"revision": revision, "duration_ms": store.load()?.duration_ms()}))?;
        }
        Command::Frame {
            asset,
            at_ms,
            output,
        } => {
            let project = store.load()?;
            let (source, source_at) = if let Some(asset) = asset {
                (project.asset(&asset)?, at_ms)
            } else {
                media::timeline_source(&project, at_ms)?
            };
            if source_at >= source.duration_ms {
                return Err(error("frame time is outside the source asset"));
            }
            media::frame(
                &media::asset_path(store.path(), source),
                source_at,
                &output,
                &config,
            )?;
            print(&json!({"asset_id": source.id, "source_at_ms": source_at, "output": output}))?;
        }
        Command::ContactSheet {
            asset,
            every_ms,
            columns,
            output,
        } => {
            let project = store.load()?;
            let source = project.asset(&asset)?;
            media::contact_sheet(
                &media::asset_path(store.path(), source),
                source.duration_ms,
                every_ms,
                columns,
                &output,
                &config,
            )?;
            print(&json!({"asset_id": asset, "every_ms": every_ms, "output": output}))?;
        }
        Command::Render { output } => {
            let project = store.load()?;
            media::render(&project, store.path(), &output, &config)?;
            print(&json!({"output": output, "duration_ms": project.duration_ms()}))?;
        }
        Command::ModelDownload => {
            let path = media::default_model_path()?;
            if !path.exists() {
                media::download_model(&path)?;
            }
            AppConfig::update(|updated| {
                updated.model_path = Some(path.clone());
                Ok(())
            })?;
            print(&json!({"model_path": path}))?;
        }
        Command::Config { command } => match command {
            ConfigCommand::Show => print(&config)?,
            ConfigCommand::Get { pointer } => {
                let raw = serde_json::to_value(&config)?;
                let value = raw
                    .pointer(&pointer)
                    .ok_or_else(|| error(format!("unknown config pointer: {pointer}")))?;
                print(value)?;
            }
            ConfigCommand::Set { pointer, value } => {
                if !pointer.starts_with('/') {
                    return Err(error("config pointer must start with /"));
                }
                let value: Value = serde_json::from_str(&value)?;
                let (updated, _) = AppConfig::update(|config| {
                    let mut raw = serde_json::to_value(&*config)?;
                    let slot = raw
                        .pointer_mut(&pointer)
                        .ok_or_else(|| error(format!("unknown config pointer: {pointer}")))?;
                    *slot = value;
                    *config = serde_json::from_value(raw)?;
                    Ok(())
                })?;
                print(&updated)?;
            }
        },
        Command::ThemeExport { theme, output } => {
            let theme = match theme {
                ThemeName::Light => Theme::light(),
                ThemeName::Dark => Theme::dark(),
            };
            fs::write(&output, serde_json::to_vec_pretty(&theme)?)?;
            print(&json!({"theme_file": output}))?;
        }
        Command::LayoutSwap { first, second } => {
            let (updated, _) = AppConfig::update(|config| {
                config.layout.swap(first.into(), second.into());
                Ok(())
            })?;
            print(&updated.layout)?;
        }
        Command::Schema => {
            let project = clipperino_core::project::Project::default();
            let sample = json!({
                "project": project,
                "config": AppConfig::default(),
                "theme": Theme::light(),
            });
            print(&sample)?;
        }
    }
    Ok(())
}

use std::{
    env,
    fs::{self, OpenOptions},
    path::PathBuf,
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::{Result, error, project::write_atomic};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub schema_version: u32,
    pub theme: String,
    pub custom_theme: Option<PathBuf>,
    pub whisper_cli: String,
    pub model_path: Option<PathBuf>,
    pub ffmpeg: String,
    pub ffprobe: String,
    pub preview_width: u32,
    pub transcript_word_timestamps: bool,
    pub layout: LayoutNode,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            schema_version: 1,
            theme: "light".into(),
            custom_theme: None,
            whisper_cli: "whisper-cli".into(),
            model_path: None,
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
            preview_width: 960,
            transcript_word_timestamps: true,
            layout: LayoutNode::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Axis {
    Horizontal,
    Vertical,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelId {
    Assets,
    Preview,
    Transcript,
    Timeline,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LayoutNode {
    Split {
        axis: Axis,
        ratio: f32,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
    Panel {
        panel: PanelId,
    },
}

impl Default for LayoutNode {
    fn default() -> Self {
        Self::Split {
            axis: Axis::Vertical,
            ratio: 0.72,
            first: Box::new(Self::Split {
                axis: Axis::Horizontal,
                ratio: 0.20,
                first: Box::new(Self::Panel {
                    panel: PanelId::Assets,
                }),
                second: Box::new(Self::Split {
                    axis: Axis::Horizontal,
                    ratio: 0.72,
                    first: Box::new(Self::Panel {
                        panel: PanelId::Preview,
                    }),
                    second: Box::new(Self::Panel {
                        panel: PanelId::Transcript,
                    }),
                }),
            }),
            second: Box::new(Self::Panel {
                panel: PanelId::Timeline,
            }),
        }
    }
}

impl LayoutNode {
    pub fn validate(&self) -> Result<()> {
        let mut panels = Vec::new();
        self.collect(&mut panels)?;
        panels.sort_by_key(|panel| *panel as u8);
        if panels
            != [
                PanelId::Assets,
                PanelId::Preview,
                PanelId::Transcript,
                PanelId::Timeline,
            ]
        {
            return Err(error("layout must contain each panel exactly once"));
        }
        Ok(())
    }

    fn collect(&self, panels: &mut Vec<PanelId>) -> Result<()> {
        match self {
            Self::Panel { panel } => panels.push(*panel),
            Self::Split {
                ratio,
                first,
                second,
                ..
            } => {
                if !(0.10..=0.90).contains(ratio) {
                    return Err(error("layout split ratio must be between 0.10 and 0.90"));
                }
                first.collect(panels)?;
                second.collect(panels)?;
            }
        }
        Ok(())
    }

    pub fn swap(&mut self, a: PanelId, b: PanelId) {
        match self {
            Self::Panel { panel } if *panel == a => *panel = b,
            Self::Panel { panel } if *panel == b => *panel = a,
            Self::Split { first, second, .. } => {
                first.swap(a, b);
                second.swap(a, b);
            }
            _ => {}
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Theme {
    pub background: String,
    pub panel: String,
    pub text: String,
    pub muted_text: String,
    pub border: String,
    pub accent: String,
    pub selected: String,
}

impl Theme {
    pub fn light() -> Self {
        Self {
            background: "#f7f7f8".into(),
            panel: "#ffffff".into(),
            text: "#18181b".into(),
            muted_text: "#71717a".into(),
            border: "#e4e4e7".into(),
            accent: "#2563eb".into(),
            selected: "#eff6ff".into(),
        }
    }

    pub fn dark() -> Self {
        Self {
            background: "#09090b".into(),
            panel: "#18181b".into(),
            text: "#fafafa".into(),
            muted_text: "#a1a1aa".into(),
            border: "#27272a".into(),
            accent: "#60a5fa".into(),
            selected: "#1e3a5f".into(),
        }
    }

    pub fn validate(&self) -> Result<()> {
        for value in [
            &self.background,
            &self.panel,
            &self.text,
            &self.muted_text,
            &self.border,
            &self.accent,
            &self.selected,
        ] {
            if value.len() != 7
                || !value.starts_with('#')
                || !value[1..].chars().all(|c| c.is_ascii_hexdigit())
            {
                return Err(error(format!("invalid theme color: {value}")));
            }
        }
        Ok(())
    }
}

impl AppConfig {
    pub fn path() -> Result<PathBuf> {
        if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
            return Ok(PathBuf::from(path).join("clipperino/config.json"));
        }
        let home = env::var_os("HOME").ok_or_else(|| error("HOME is not set"))?;
        Ok(PathBuf::from(home).join(".config/clipperino/config.json"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        let config = if path.exists() {
            serde_json::from_slice(&fs::read(path)?)?
        } else {
            Self::default()
        };
        let config: Self = config;
        config.validate()?;
        Ok(config)
    }

    pub fn update<T>(operation: impl FnOnce(&mut Self) -> Result<T>) -> Result<(Self, T)> {
        let path = Self::path()?;
        fs::create_dir_all(path.parent().ok_or_else(|| error("invalid config path"))?)?;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))?;
        lock.lock_exclusive()?;
        let mut config = Self::load()?;
        let result = operation(&mut config)?;
        config.validate()?;
        write_atomic(&path, &serde_json::to_vec_pretty(&config)?)?;
        Ok((config, result))
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err(error("unsupported config schema version"));
        }
        if self.theme != "light" && self.theme != "dark" {
            return Err(error("theme must be light or dark"));
        }
        if !(320..=3840).contains(&self.preview_width) {
            return Err(error("preview_width must be between 320 and 3840"));
        }
        self.layout.validate()?;
        self.resolved_theme()?.validate()
    }

    pub fn theme_file_path(&self) -> Result<Option<PathBuf>> {
        self.custom_theme
            .as_ref()
            .map(|path| {
                if path.is_absolute() {
                    Ok(path.clone())
                } else {
                    Ok(Self::path()?
                        .parent()
                        .ok_or_else(|| error("invalid config directory"))?
                        .join(path))
                }
            })
            .transpose()
    }

    pub fn resolved_theme(&self) -> Result<Theme> {
        if let Some(path) = self.theme_file_path()? {
            Ok(serde_json::from_slice(&fs::read(path)?)?)
        } else if self.theme == "dark" {
            Ok(Theme::dark())
        } else {
            Ok(Theme::light())
        }
    }
}

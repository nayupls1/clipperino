//! Interface icons, embedded in the binary. Shapes follow Lucide (ISC license).

use std::borrow::Cow;

use gpui::{AssetSource, SharedString, Svg, prelude::*, svg};

#[derive(Clone, Copy)]
pub enum Icon {
    AudioLines,
    ChevronLeft,
    ChevronRight,
    Download,
    Export,
    Film,
    Flag,
    FlagEnd,
    Grip,
    Mic,
    Moon,
    Pause,
    Play,
    Plus,
    Redo,
    Scissors,
    SkipBack,
    SkipForward,
    Sparkles,
    Sun,
    Text,
    Trash,
    TrimLeft,
    TrimRight,
    Undo,
    ZoomFit,
    ZoomIn,
    ZoomOut,
}

impl Icon {
    const ALL: [Icon; 28] = [
        Icon::AudioLines,
        Icon::ChevronLeft,
        Icon::ChevronRight,
        Icon::Download,
        Icon::Export,
        Icon::Film,
        Icon::Flag,
        Icon::FlagEnd,
        Icon::Grip,
        Icon::Mic,
        Icon::Moon,
        Icon::Pause,
        Icon::Play,
        Icon::Plus,
        Icon::Redo,
        Icon::Scissors,
        Icon::SkipBack,
        Icon::SkipForward,
        Icon::Sparkles,
        Icon::Sun,
        Icon::Text,
        Icon::Trash,
        Icon::TrimLeft,
        Icon::TrimRight,
        Icon::Undo,
        Icon::ZoomFit,
        Icon::ZoomIn,
        Icon::ZoomOut,
    ];

    fn name(self) -> &'static str {
        match self {
            Icon::AudioLines => "audio-lines",
            Icon::ChevronLeft => "chevron-left",
            Icon::ChevronRight => "chevron-right",
            Icon::Download => "download",
            Icon::Export => "export",
            Icon::Film => "film",
            Icon::Flag => "flag",
            Icon::FlagEnd => "flag-end",
            Icon::Grip => "grip",
            Icon::Mic => "mic",
            Icon::Moon => "moon",
            Icon::Pause => "pause",
            Icon::Play => "play",
            Icon::Plus => "plus",
            Icon::Redo => "redo",
            Icon::Scissors => "scissors",
            Icon::SkipBack => "skip-back",
            Icon::SkipForward => "skip-forward",
            Icon::Sparkles => "sparkles",
            Icon::Sun => "sun",
            Icon::Text => "text",
            Icon::Trash => "trash",
            Icon::TrimLeft => "trim-left",
            Icon::TrimRight => "trim-right",
            Icon::Undo => "undo",
            Icon::ZoomFit => "zoom-fit",
            Icon::ZoomIn => "zoom-in",
            Icon::ZoomOut => "zoom-out",
        }
    }

    fn body(self) -> &'static str {
        match self {
            Icon::AudioLines => {
                r#"<path d="M2 10v3"/><path d="M6 6v11"/><path d="M10 3v18"/><path d="M14 8v7"/><path d="M18 5v13"/><path d="M22 10v3"/>"#
            }
            Icon::ChevronLeft => r#"<path d="m15 18-6-6 6-6"/>"#,
            Icon::ChevronRight => r#"<path d="m9 18 6-6-6-6"/>"#,
            Icon::Download => {
                r#"<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="m7 10 5 5 5-5"/><path d="M12 15V3"/>"#
            }
            Icon::Export => {
                r#"<path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4"/><path d="m17 8-5-5-5 5"/><path d="M12 3v12"/>"#
            }
            Icon::Film => {
                r#"<rect width="18" height="18" x="3" y="3" rx="2"/><path d="M7 3v18"/><path d="M3 7.5h4"/><path d="M3 12h18"/><path d="M3 16.5h4"/><path d="M17 3v18"/><path d="M17 7.5h4"/><path d="M17 16.5h4"/>"#
            }
            Icon::Flag => r#"<path d="M8 4v16"/><path d="M8 4h8"/><path d="M8 20h8"/>"#,
            Icon::FlagEnd => r#"<path d="M16 4v16"/><path d="M16 4H8"/><path d="M16 20H8"/>"#,
            Icon::Grip => {
                r#"<circle cx="9" cy="12" r="1"/><circle cx="9" cy="5" r="1"/><circle cx="9" cy="19" r="1"/><circle cx="15" cy="12" r="1"/><circle cx="15" cy="5" r="1"/><circle cx="15" cy="19" r="1"/>"#
            }
            Icon::Mic => {
                r#"<path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z"/><path d="M19 10v2a7 7 0 0 1-14 0v-2"/><path d="M12 19v3"/>"#
            }
            Icon::Moon => r#"<path d="M12 3a6 6 0 0 0 9 9 9 9 0 1 1-9-9Z"/>"#,
            Icon::Pause => {
                r#"<rect x="14" y="4" width="4" height="16" rx="1"/><rect x="6" y="4" width="4" height="16" rx="1"/>"#
            }
            Icon::Play => r#"<path d="M6 3 20 12 6 21Z"/>"#,
            Icon::Plus => r#"<path d="M5 12h14"/><path d="M12 5v14"/>"#,
            Icon::Redo => {
                r#"<path d="m15 14 5-5-5-5"/><path d="M20 9H9.5A5.5 5.5 0 0 0 4 14.5 5.5 5.5 0 0 0 9.5 20H13"/>"#
            }
            Icon::Scissors => {
                r#"<circle cx="6" cy="6" r="3"/><path d="M8.12 8.12 12 12"/><path d="M20 4 8.12 15.88"/><circle cx="6" cy="18" r="3"/><path d="M14.8 14.8 20 20"/>"#
            }
            Icon::SkipBack => r#"<path d="M19 20 9 12l10-8Z"/><path d="M5 19V5"/>"#,
            Icon::SkipForward => r#"<path d="m5 4 10 8-10 8Z"/><path d="M19 5v14"/>"#,
            Icon::Sparkles => {
                r#"<path d="M9.94 15.5A2 2 0 0 0 8.5 14.06l-6.14-1.58a.5.5 0 0 1 0-.96L8.5 9.94A2 2 0 0 0 9.94 8.5l1.58-6.14a.5.5 0 0 1 .96 0l1.58 6.14a2 2 0 0 0 1.44 1.44l6.14 1.58a.5.5 0 0 1 0 .96l-6.14 1.58a2 2 0 0 0-1.44 1.44l-1.58 6.14a.5.5 0 0 1-.96 0Z"/>"#
            }
            Icon::Sun => {
                r#"<circle cx="12" cy="12" r="4"/><path d="M12 2v2"/><path d="M12 20v2"/><path d="m4.93 4.93 1.41 1.41"/><path d="m17.66 17.66 1.41 1.41"/><path d="M2 12h2"/><path d="M20 12h2"/><path d="m6.34 17.66-1.41 1.41"/><path d="m19.07 4.93-1.41 1.41"/>"#
            }
            Icon::Text => r#"<path d="M17 6H3"/><path d="M21 12H3"/><path d="M15 18H3"/>"#,
            Icon::Trash => {
                r#"<path d="M3 6h18"/><path d="M19 6v14c0 1-1 2-2 2H7c-1 0-2-1-2-2V6"/><path d="M8 6V4c0-1 1-2 2-2h4c1 0 2 1 2 2v2"/>"#
            }
            Icon::TrimLeft => r#"<path d="M3 19V5"/><path d="m13 6-6 6 6 6"/><path d="M7 12h14"/>"#,
            Icon::TrimRight => {
                r#"<path d="M17 12H3"/><path d="m11 18 6-6-6-6"/><path d="M21 5v14"/>"#
            }
            Icon::Undo => {
                r#"<path d="M9 14 4 9l5-5"/><path d="M4 9h10.5A5.5 5.5 0 0 1 20 14.5 5.5 5.5 0 0 1 14.5 20H11"/>"#
            }
            Icon::ZoomFit => {
                r#"<path d="M8 3H5a2 2 0 0 0-2 2v3"/><path d="M21 8V5a2 2 0 0 0-2-2h-3"/><path d="M3 16v3a2 2 0 0 0 2 2h3"/><path d="M16 21h3a2 2 0 0 0 2-2v-3"/>"#
            }
            Icon::ZoomIn => {
                r#"<circle cx="11" cy="11" r="8"/><path d="m21 21-4.35-4.35"/><path d="M11 8v6"/><path d="M8 11h6"/>"#
            }
            Icon::ZoomOut => {
                r#"<circle cx="11" cy="11" r="8"/><path d="m21 21-4.35-4.35"/><path d="M8 11h6"/>"#
            }
        }
    }

    fn path(self) -> SharedString {
        format!("icons/{}.svg", self.name()).into()
    }

    pub fn element(self) -> Svg {
        svg().path(self.path()).flex_none().size_4()
    }
}

/// Serves the embedded icons to GPUI.
pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> gpui::Result<Option<Cow<'static, [u8]>>> {
        let Some(name) = path
            .strip_prefix("icons/")
            .and_then(|name| name.strip_suffix(".svg"))
        else {
            return Ok(None);
        };
        Ok(Icon::ALL
            .into_iter()
            .find(|icon| icon.name() == name)
            .map(|icon| {
                Cow::Owned(
                    format!(
                        r#"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="black" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">{}</svg>"#,
                        icon.body()
                    )
                    .into_bytes(),
                )
            }))
    }

    fn list(&self, path: &str) -> gpui::Result<Vec<SharedString>> {
        Ok(Icon::ALL
            .into_iter()
            .map(Icon::path)
            .filter(|icon| icon.starts_with(path))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_icon_loads_as_svg() {
        for icon in Icon::ALL {
            let bytes = Assets.load(&icon.path()).unwrap().unwrap();
            assert!(bytes.starts_with(b"<svg"));
        }
        assert!(Assets.load("icons/missing.svg").unwrap().is_none());
    }
}

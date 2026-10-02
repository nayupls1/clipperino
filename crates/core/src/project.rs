use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::{Result, error};

pub const PROJECT_VERSION: u32 = 1;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Project {
    pub schema_version: u32,
    pub revision: u64,
    pub assets: Vec<Asset>,
    pub transcript: Vec<TranscriptEntry>,
    pub segments: Vec<Segment>,
    #[serde(default)]
    pub settings: ProjectSettings,
}

impl Default for Project {
    fn default() -> Self {
        Self {
            schema_version: PROJECT_VERSION,
            revision: 0,
            assets: Vec::new(),
            transcript: Vec::new(),
            segments: Vec::new(),
            settings: ProjectSettings::default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub path: String,
    pub duration_ms: u64,
    pub width: u32,
    pub height: u32,
    pub has_audio: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptEntry {
    pub id: String,
    pub asset_id: String,
    pub source_start_ms: u64,
    pub source_end_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub id: String,
    pub asset_id: String,
    pub source_start_ms: u64,
    pub source_end_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectSettings {
    pub silence_min_ms: u64,
    pub silence_padding_ms: u64,
    pub silence_threshold_db: i32,
}

impl Default for ProjectSettings {
    fn default() -> Self {
        Self {
            silence_min_ms: 750,
            silence_padding_ms: 150,
            silence_threshold_db: -35,
        }
    }
}

impl Segment {
    pub fn duration_ms(&self) -> u64 {
        self.source_end_ms - self.source_start_ms
    }
}

impl Project {
    fn fresh_segment_id(&self, added: &[Segment], counter: &mut u64) -> String {
        loop {
            let id = format!("segment-{}-{}", self.revision + 1, *counter);
            *counter += 1;
            if !self.segments.iter().any(|segment| segment.id == id)
                && !added.iter().any(|segment| segment.id == id)
            {
                return id;
            }
        }
    }

    pub fn duration_ms(&self) -> u64 {
        self.segments.iter().map(Segment::duration_ms).sum()
    }

    /// Maps a source position to the assembled timeline, if that source moment is still kept.
    pub fn source_to_timeline(&self, asset_id: &str, source_ms: u64) -> Option<u64> {
        let mut position = 0;
        for segment in &self.segments {
            if segment.asset_id == asset_id
                && (segment.source_start_ms..segment.source_end_ms).contains(&source_ms)
            {
                return Some(position + source_ms - segment.source_start_ms);
            }
            position += segment.duration_ms();
        }
        None
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != PROJECT_VERSION {
            return Err(error(format!(
                "unsupported project schema version {}",
                self.schema_version
            )));
        }
        let mut ids = std::collections::HashSet::new();
        for asset in &self.assets {
            if !ids.insert(asset.id.as_str()) || asset.id.is_empty() {
                return Err(error(format!("duplicate or empty asset id: {}", asset.id)));
            }
            if asset.duration_ms == 0 || asset.width == 0 || asset.height == 0 {
                return Err(error(format!("invalid media metadata for {}", asset.id)));
            }
        }
        ids.clear();
        for segment in &self.segments {
            if !ids.insert(segment.id.as_str()) || segment.id.is_empty() {
                return Err(error(format!(
                    "duplicate or empty segment id: {}",
                    segment.id
                )));
            }
            let asset = self
                .assets
                .iter()
                .find(|asset| asset.id == segment.asset_id)
                .ok_or_else(|| error(format!("unknown asset: {}", segment.asset_id)))?;
            if segment.source_start_ms >= segment.source_end_ms
                || segment.source_end_ms > asset.duration_ms
            {
                return Err(error(format!("invalid source range in {}", segment.id)));
            }
        }
        ids.clear();
        for entry in &self.transcript {
            if !ids.insert(entry.id.as_str()) || entry.id.is_empty() {
                return Err(error(format!(
                    "duplicate or empty transcript id: {}",
                    entry.id
                )));
            }
            let asset = self
                .assets
                .iter()
                .find(|asset| asset.id == entry.asset_id)
                .ok_or_else(|| error(format!("unknown asset: {}", entry.asset_id)))?;
            if entry.source_start_ms >= entry.source_end_ms
                || entry.source_end_ms > asset.duration_ms + 1000
            {
                return Err(error(format!("invalid transcript time in {}", entry.id)));
            }
        }
        Ok(())
    }

    pub fn asset(&self, id: &str) -> Result<&Asset> {
        self.assets
            .iter()
            .find(|asset| asset.id == id)
            .ok_or_else(|| error(format!("unknown asset: {id}")))
    }

    pub fn add_asset(&mut self, mut asset: Asset) -> Result<String> {
        if self
            .assets
            .iter()
            .any(|existing| existing.path == asset.path)
        {
            return Err(error("asset path is already imported"));
        }
        let mut next = 1;
        while self
            .assets
            .iter()
            .any(|asset| asset.id == format!("asset-{next}"))
        {
            next += 1;
        }
        asset.id = format!("asset-{next}");
        let id = asset.id.clone();
        let segment_id = self.fresh_segment_id(&[], &mut 1);
        self.segments.push(Segment {
            id: segment_id,
            asset_id: id.clone(),
            source_start_ms: 0,
            source_end_ms: asset.duration_ms,
        });
        self.assets.push(asset);
        Ok(id)
    }

    /// Removes a half-open range on the assembled timeline and ripples later segments left.
    pub fn remove_timeline_range(&mut self, start_ms: u64, end_ms: u64) -> Result<()> {
        if start_ms >= end_ms || end_ms > self.duration_ms() {
            return Err(error("cut range must be inside the timeline"));
        }
        let mut cursor = 0;
        let mut next = Vec::new();
        let mut new_id = 1;
        for segment in &self.segments {
            let segment_end = cursor + segment.duration_ms();
            let mut kept_left = false;
            if cursor < start_ms {
                let end = start_ms.min(segment_end);
                if end > cursor {
                    next.push(Segment {
                        id: segment.id.clone(),
                        asset_id: segment.asset_id.clone(),
                        source_start_ms: segment.source_start_ms,
                        source_end_ms: segment.source_start_ms + end - cursor,
                    });
                    kept_left = true;
                }
            }
            if segment_end > end_ms {
                let start = end_ms.max(cursor);
                if segment_end > start {
                    let id = if kept_left {
                        self.fresh_segment_id(&next, &mut new_id)
                    } else {
                        segment.id.clone()
                    };
                    next.push(Segment {
                        id,
                        asset_id: segment.asset_id.clone(),
                        source_start_ms: segment.source_start_ms + start - cursor,
                        source_end_ms: segment.source_end_ms,
                    });
                }
            }
            cursor = segment_end;
        }
        self.segments = coalesce(next);
        Ok(())
    }

    pub fn replace_asset_with_kept_ranges(
        &mut self,
        asset_id: &str,
        kept: &[(u64, u64)],
    ) -> Result<()> {
        let duration = self.asset(asset_id)?.duration_ms;
        if kept
            .iter()
            .any(|&(start, end)| start >= end || end > duration)
            || kept.windows(2).any(|pair| pair[0].1 > pair[1].0)
        {
            return Err(error("invalid or overlapping kept ranges"));
        }
        let positions: Vec<_> = self
            .segments
            .iter()
            .enumerate()
            .filter(|(_, segment)| segment.asset_id == asset_id)
            .map(|(index, _)| index)
            .collect();
        if positions.len() != 1 {
            return Err(error("auto-cut requires one uncut segment for this asset"));
        }
        let index = positions[0];
        let original = &self.segments[index];
        if original.source_start_ms != 0 || original.source_end_ms != duration {
            return Err(error("auto-cut requires the original full asset segment"));
        }
        let original_id = original.id.clone();
        let mut replacements = Vec::new();
        let mut next_id = 1;
        for (index, &(start, end)) in kept.iter().enumerate() {
            let id = if index == 0 {
                original_id.clone()
            } else {
                self.fresh_segment_id(&replacements, &mut next_id)
            };
            replacements.push(Segment {
                id,
                asset_id: asset_id.to_owned(),
                source_start_ms: start,
                source_end_ms: end,
            });
        }
        self.segments.splice(index..=index, replacements);
        self.segments = coalesce(std::mem::take(&mut self.segments));
        Ok(())
    }
}

fn coalesce(segments: Vec<Segment>) -> Vec<Segment> {
    let mut result: Vec<Segment> = Vec::new();
    for segment in segments {
        if let Some(last) = result.last_mut()
            && last.asset_id == segment.asset_id
            && last.source_end_ms == segment.source_start_ms
        {
            last.source_end_ms = segment.source_end_ms;
            continue;
        }
        result.push(segment);
    }
    result
}

pub struct ProjectStore {
    path: PathBuf,
}

impl ProjectStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn create(&self) -> Result<()> {
        if self.path.exists() {
            return Err(error("project already exists"));
        }
        if let Some(parent) = self
            .path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        write_atomic(&self.path, &serde_json::to_vec_pretty(&Project::default())?)
    }

    pub fn load(&self) -> Result<Project> {
        let project: Project = serde_json::from_slice(&fs::read(&self.path)?)?;
        project.validate()?;
        Ok(project)
    }

    pub fn update<T>(&self, operation: impl FnOnce(&mut Project) -> Result<T>) -> Result<T> {
        let _lock = self.lock()?;
        let mut project = self.load()?;
        let before = serde_json::to_vec_pretty(&project)?;
        let result = operation(&mut project)?;
        project.validate()?;
        self.push_history(project.revision, &before)?;
        project.revision += 1;
        write_atomic(&self.path, &serde_json::to_vec_pretty(&project)?)?;
        Ok(result)
    }

    pub fn undo(&self) -> Result<u64> {
        let _lock = self.lock()?;
        let current = self.load()?;
        let history_dir = self.history_dir();
        let index_path = history_dir.join("index.json");
        let mut index: Vec<u64> =
            serde_json::from_slice(&fs::read(&index_path).map_err(|_| error("nothing to undo"))?)?;
        let revision = index.pop().ok_or_else(|| error("nothing to undo"))?;
        let mut previous: Project =
            serde_json::from_slice(&fs::read(history_dir.join(format!("{revision}.json")))?)?;
        previous.revision = current.revision + 1;
        previous.validate()?;
        write_atomic(&self.path, &serde_json::to_vec_pretty(&previous)?)?;
        write_atomic(&index_path, &serde_json::to_vec_pretty(&index)?)?;
        Ok(previous.revision)
    }

    fn lock(&self) -> Result<File> {
        let lock_path = self.path.with_extension("lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        lock.lock_exclusive()?;
        Ok(lock)
    }

    fn history_dir(&self) -> PathBuf {
        self.path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".clipperino-cache/history")
    }

    fn push_history(&self, revision: u64, before: &[u8]) -> Result<()> {
        let directory = self.history_dir();
        fs::create_dir_all(&directory)?;
        let index_path = directory.join("index.json");
        let mut index: Vec<u64> = if index_path.exists() {
            serde_json::from_slice(&fs::read(&index_path)?)?
        } else {
            Vec::new()
        };
        write_atomic(&directory.join(format!("{revision}.json")), before)?;
        index.push(revision);
        write_atomic(&index_path, &serde_json::to_vec_pretty(&index)?)
    }
}

pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let temporary = path.with_extension("tmp");
    let mut file = File::create(&temporary)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    fs::rename(temporary, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Project {
        let mut project = Project::default();
        project
            .add_asset(Asset {
                id: String::new(),
                path: "a.mp4".into(),
                duration_ms: 10_000,
                width: 1920,
                height: 1080,
                has_audio: true,
            })
            .unwrap();
        project
    }

    #[test]
    fn timeline_removal_splits_and_ripples() {
        let mut project = sample();
        let original_id = project.segments[0].id.clone();
        project.remove_timeline_range(2_000, 4_000).unwrap();
        assert_eq!(project.duration_ms(), 8_000);
        assert_eq!(project.segments.len(), 2);
        assert_eq!(project.segments[0].id, original_id);
        assert_ne!(project.segments[1].id, original_id);
        assert_eq!(project.segments[0].source_end_ms, 2_000);
        assert_eq!(project.segments[1].source_start_ms, 4_000);
        project.validate().unwrap();
    }

    #[test]
    fn undo_restores_the_previous_timeline() {
        let directory = std::env::temp_dir().join(format!(
            "clipperino-core-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let store = ProjectStore::new(directory.join("project.json"));
        store.create().unwrap();
        store
            .update(|project| {
                project.add_asset(Asset {
                    id: String::new(),
                    path: "a.mp4".into(),
                    duration_ms: 10_000,
                    width: 1920,
                    height: 1080,
                    has_audio: true,
                })?;
                Ok(())
            })
            .unwrap();
        store
            .update(|project| project.remove_timeline_range(2_000, 4_000))
            .unwrap();
        assert_eq!(store.load().unwrap().duration_ms(), 8_000);
        store.undo().unwrap();
        assert_eq!(store.load().unwrap().duration_ms(), 10_000);
        fs::remove_dir_all(directory).unwrap();
    }
}

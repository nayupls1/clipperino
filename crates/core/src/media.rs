use std::{
    collections::HashMap,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use serde_json::Value;

use crate::{
    Result,
    config::AppConfig,
    error,
    project::{Asset, Project, TranscriptEntry},
};

fn command_output(command: &mut Command) -> Result<std::process::Output> {
    let program = command.get_program().to_string_lossy().into_owned();
    let output = command
        .output()
        .map_err(|error| crate::error(format!("could not run {program}: {error}")))?;
    if !output.status.success() {
        return Err(error(format!(
            "command failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(output)
}

pub fn probe(path: &Path, config: &AppConfig) -> Result<Asset> {
    let output = command_output(
        Command::new(&config.ffprobe)
            .args([
                "-v",
                "error",
                "-show_format",
                "-show_streams",
                "-of",
                "json",
            ])
            .arg(path),
    )?;
    let data: Value = serde_json::from_slice(&output.stdout)?;
    let streams = data["streams"]
        .as_array()
        .ok_or_else(|| error("ffprobe did not return streams"))?;
    let video = streams
        .iter()
        .find(|stream| stream["codec_type"] == "video")
        .ok_or_else(|| error("asset has no video stream"))?;
    let has_audio = streams.iter().any(|stream| stream["codec_type"] == "audio");
    let duration = data["format"]["duration"]
        .as_str()
        .or_else(|| video["duration"].as_str())
        .ok_or_else(|| error("cannot determine media duration"))?
        .parse::<f64>()?;
    let duration_ms = (duration * 1000.0).round() as u64;
    Ok(Asset {
        id: String::new(),
        path: path.to_string_lossy().into_owned(),
        duration_ms,
        width: video["width"].as_u64().unwrap_or(0) as u32,
        height: video["height"].as_u64().unwrap_or(0) as u32,
        has_audio,
    })
}

pub fn asset_path(project_path: &Path, asset: &Asset) -> PathBuf {
    let path = Path::new(&asset.path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        project_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path)
    }
}

pub fn frame(input: &Path, at_ms: u64, output: &Path, config: &AppConfig) -> Result<()> {
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    command_output(
        Command::new(&config.ffmpeg)
            .args(["-v", "error", "-y", "-ss"])
            .arg(format!("{:.3}", at_ms as f64 / 1000.0))
            .arg("-i")
            .arg(input)
            .args(["-frames:v", "1", "-vf"])
            .arg(format!("scale={}: -2", config.preview_width).replace(" ", ""))
            .arg(output),
    )?;
    Ok(())
}

pub fn contact_sheet(
    input: &Path,
    duration_ms: u64,
    every_ms: u64,
    columns: u32,
    output: &Path,
    config: &AppConfig,
) -> Result<()> {
    if every_ms < 100 || !(1..=10).contains(&columns) {
        return Err(error(
            "every_ms must be at least 100 and columns must be 1 to 10",
        ));
    }
    let count = duration_ms.div_ceil(every_ms).clamp(1, 100) as u32;
    let rows = count.div_ceil(columns);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    command_output(
        Command::new(&config.ffmpeg)
            .args(["-v", "error", "-y", "-i"])
            .arg(input)
            .arg("-vf")
            .arg(format!(
                "fps=1000/{every_ms},scale=320:-2,tile={}x{}:padding=4:margin=4",
                columns, rows
            ))
            .args(["-frames:v", "1"])
            .arg(output),
    )?;
    Ok(())
}

pub fn timeline_source(project: &Project, timeline_ms: u64) -> Result<(&Asset, u64)> {
    let mut position = 0;
    for segment in &project.segments {
        let end = position + segment.duration_ms();
        if timeline_ms < end {
            return Ok((
                project.asset(&segment.asset_id)?,
                segment.source_start_ms + timeline_ms - position,
            ));
        }
        position = end;
    }
    Err(error("time is outside the timeline"))
}

pub fn transcribe(
    input: &Path,
    asset_id: &str,
    cache_dir: &Path,
    config: &AppConfig,
) -> Result<Vec<TranscriptEntry>> {
    let model = config
        .model_path
        .as_ref()
        .ok_or_else(|| error("no model configured; run `clipperino model download`"))?;
    if !model.exists() {
        return Err(error(format!(
            "model file does not exist: {}",
            model.display()
        )));
    }
    fs::create_dir_all(cache_dir)?;
    let wav = cache_dir.join(format!("{asset_id}.wav"));
    let output_base = cache_dir.join(format!("{asset_id}-transcript"));
    command_output(
        Command::new(&config.ffmpeg)
            .args(["-v", "error", "-y", "-i"])
            .arg(input)
            .args(["-vn", "-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le"])
            .arg(&wav),
    )?;
    let mut whisper = Command::new(&config.whisper_cli);
    whisper
        .arg("-m")
        .arg(model)
        .arg("-f")
        .arg(&wav)
        .args(["-l", "en", "-oj", "-of"])
        .arg(&output_base);
    if config.transcript_word_timestamps {
        whisper.args(["-ml", "1", "-sow"]);
    }
    command_output(&mut whisper)?;
    let data: Value = serde_json::from_slice(&fs::read(output_base.with_extension("json"))?)?;
    let entries = data["transcription"]
        .as_array()
        .ok_or_else(|| error("whisper-cli returned no transcription array"))?;
    let parsed: Vec<TranscriptEntry> = entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let start = entry["offsets"]["from"]
                .as_u64()
                .ok_or_else(|| error("transcript entry has no start offset"))?;
            let end = entry["offsets"]["to"]
                .as_u64()
                .ok_or_else(|| error("transcript entry has no end offset"))?;
            let text = entry["text"]
                .as_str()
                .ok_or_else(|| error("transcript entry has no text"))?
                .trim()
                .to_owned();
            Ok(TranscriptEntry {
                id: format!("{asset_id}-line-{}", index + 1),
                asset_id: asset_id.to_owned(),
                source_start_ms: start,
                source_end_ms: end,
                text,
            })
        })
        .collect::<Result<_>>()?;
    Ok(parsed
        .into_iter()
        .filter(|entry| !entry.text.is_empty() && entry.source_start_ms < entry.source_end_ms)
        .collect())
}

pub fn detect_silence(
    input: &Path,
    min_ms: u64,
    threshold_db: i32,
    config: &AppConfig,
) -> Result<Vec<(u64, u64)>> {
    let output = command_output(
        Command::new(&config.ffmpeg)
            .args(["-hide_banner", "-i"])
            .arg(input)
            .arg("-af")
            .arg(format!(
                "silencedetect=noise={}dB:d={:.3}",
                threshold_db,
                min_ms as f64 / 1000.0
            ))
            .args(["-f", "null", "-"]),
    )?;
    let log = String::from_utf8_lossy(&output.stderr);
    let mut starts = Vec::new();
    let mut silence = Vec::new();
    for line in log.lines() {
        if let Some(value) = line.split("silence_start:").nth(1)
            && let Ok(start) = value.trim().parse::<f64>()
        {
            starts.push((start * 1000.0).round() as u64);
        }
        if let Some(value) = line.split("silence_end:").nth(1)
            && let Some(end) = value.split('|').next()
            && let (Some(start), Ok(end)) = (starts.pop(), end.trim().parse::<f64>())
        {
            silence.push((start, (end * 1000.0).round() as u64));
        }
    }
    Ok(silence)
}

pub fn kept_after_silence(
    duration_ms: u64,
    silence: &[(u64, u64)],
    transcript: &[TranscriptEntry],
    padding_ms: u64,
) -> Vec<(u64, u64)> {
    let mut removed = Vec::new();
    let mut transcript = transcript.to_vec();
    transcript.sort_by_key(|entry| entry.source_start_ms);
    for &(raw_start, raw_end) in silence {
        let start = raw_start.saturating_add(padding_ms);
        let end = raw_end.min(duration_ms).saturating_sub(padding_ms);
        if start >= end {
            continue;
        }
        let mut cursor = start;
        for entry in &transcript {
            if entry.source_end_ms <= cursor || entry.source_start_ms >= end {
                continue;
            }
            if cursor < entry.source_start_ms {
                removed.push((cursor, entry.source_start_ms));
            }
            cursor = cursor.max(entry.source_end_ms);
        }
        if cursor < end {
            removed.push((cursor, end));
        }
    }
    removed.sort_unstable();
    let mut kept = Vec::new();
    let mut cursor = 0;
    for (start, end) in removed {
        if start > cursor {
            kept.push((cursor, start));
        }
        cursor = cursor.max(end);
    }
    if cursor < duration_ms {
        kept.push((cursor, duration_ms));
    }
    kept
}

pub fn render(
    project: &Project,
    project_path: &Path,
    output: &Path,
    config: &AppConfig,
) -> Result<()> {
    if project.segments.is_empty() {
        return Err(error("timeline is empty"));
    }
    let mut command = Command::new(&config.ffmpeg);
    command.args(["-v", "error", "-y"]);
    let first_asset = project.asset(&project.segments[0].asset_id)?;
    let width = first_asset.width & !1;
    let height = first_asset.height & !1;
    let mut indices = HashMap::new();
    for asset in &project.assets {
        if project
            .segments
            .iter()
            .any(|segment| segment.asset_id == asset.id)
        {
            indices.insert(asset.id.as_str(), indices.len());
            command.arg("-i").arg(asset_path(project_path, asset));
        }
    }
    let mut filters = String::new();
    for (index, segment) in project.segments.iter().enumerate() {
        let input = indices[segment.asset_id.as_str()];
        let start = segment.source_start_ms as f64 / 1000.0;
        let end = segment.source_end_ms as f64 / 1000.0;
        filters.push_str(&format!(
            "[{input}:v]trim=start={start:.3}:end={end:.3},setpts=PTS-STARTPTS,fps=30,scale={width}:{height}:force_original_aspect_ratio=decrease,pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,setsar=1,format=yuv420p[v{index}];"
        ));
        if project.asset(&segment.asset_id)?.has_audio {
            filters.push_str(&format!(
                "[{input}:a]atrim=start={start:.3}:end={end:.3},asetpts=PTS-STARTPTS,aresample=48000,aformat=sample_fmts=fltp:sample_rates=48000:channel_layouts=stereo[a{index}];"
            ));
        } else {
            filters.push_str(&format!(
                "anullsrc=channel_layout=stereo:sample_rate=48000,atrim=duration={:.3},asetpts=PTS-STARTPTS[a{index}];",
                end - start
            ));
        }
    }
    for index in 0..project.segments.len() {
        filters.push_str(&format!("[v{index}][a{index}]"));
    }
    filters.push_str(&format!(
        "concat=n={}:v=1:a=1[v][a]",
        project.segments.len()
    ));
    command
        .arg("-filter_complex")
        .arg(filters)
        .args([
            "-map", "[v]", "-map", "[a]", "-c:v", "libx264", "-c:a", "aac",
        ])
        .arg(output);
    command_output(&mut command)?;
    Ok(())
}

pub fn default_model_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(path).join("clipperino/models/ggml-base.en.bin"));
    }
    let home = env::var_os("HOME").ok_or_else(|| error("HOME is not set"))?;
    Ok(PathBuf::from(home).join(".local/share/clipperino/models/ggml-base.en.bin"))
}

pub fn download_model(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let partial = path.with_extension("part");
    command_output(
        Command::new("curl")
            .args(["--fail", "--location", "--retry", "3", "--output"])
            .arg(&partial)
            .arg("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin"),
    )?;
    if fs::metadata(&partial)?.len() < 100_000_000 {
        return Err(error("downloaded model is unexpectedly small"));
    }
    fs::rename(partial, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_cut_preserves_transcribed_speech() {
        let speech = vec![TranscriptEntry {
            id: "line-1".into(),
            asset_id: "asset-1".into(),
            source_start_ms: 1_400,
            source_end_ms: 1_600,
            text: "quiet word".into(),
        }];
        assert_eq!(
            kept_after_silence(3_000, &[(1_000, 2_000)], &speech, 100),
            vec![(0, 1_100), (1_400, 1_600), (1_900, 3_000)]
        );
    }
}

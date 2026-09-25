use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread,
};

use clipperino_core::{config::AppConfig, error, media, project::Project};
use gpui::{ImageFormat, RenderImage};

const PREVIEW_FPS: u64 = 24;

pub enum PreviewEvent {
    Frame {
        generation: u64,
        at_ms: u64,
        image: Arc<RenderImage>,
        advances_playhead: bool,
    },
    Finished {
        generation: u64,
    },
    Error {
        generation: u64,
        message: String,
    },
}

pub struct PreviewPlayer {
    tx: SyncSender<PreviewEvent>,
    rx: Receiver<PreviewEvent>,
    generation: Arc<AtomicU64>,
    snapshots: Arc<(Mutex<SnapshotState>, Condvar)>,
}

struct SnapshotRequest {
    project: Project,
    project_path: PathBuf,
    at_ms: u64,
    config: AppConfig,
    generation: u64,
}

#[derive(Default)]
struct SnapshotState {
    pending: Option<SnapshotRequest>,
    closed: bool,
}

impl PreviewPlayer {
    pub fn new() -> Self {
        let (tx, rx) = sync_channel(2);
        let generation = Arc::new(AtomicU64::new(0));
        let snapshots = Arc::new((Mutex::new(SnapshotState::default()), Condvar::new()));
        thread::spawn({
            let tx = tx.clone();
            let current = generation.clone();
            let snapshots = snapshots.clone();
            move || snapshot_worker(&snapshots, &current, &tx)
        });
        Self {
            tx,
            rx,
            generation,
            snapshots,
        }
    }

    pub fn stop(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    pub fn try_recv(&self) -> Option<PreviewEvent> {
        self.rx.try_recv().ok()
    }

    pub fn show_frame(&self, project: Project, project_path: &Path, at_ms: u64, config: AppConfig) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let (state, wake) = &*self.snapshots;
        state.lock().unwrap().pending = Some(SnapshotRequest {
            project,
            project_path: project_path.to_owned(),
            at_ms,
            config,
            generation,
        });
        wake.notify_one();
    }

    pub fn play(&self, project: Project, project_path: &Path, from_ms: u64, config: AppConfig) {
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let current = self.generation.clone();
        let tx = self.tx.clone();
        let project_path = project_path.to_owned();
        thread::spawn(move || {
            if let Err(error) = stream(
                &project,
                &project_path,
                from_ms,
                &config,
                generation,
                &current,
                &tx,
            ) {
                let _ = tx.try_send(PreviewEvent::Error {
                    generation,
                    message: error.to_string(),
                });
            } else {
                let _ = tx.send(PreviewEvent::Finished { generation });
            }
        });
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }
}

impl Drop for PreviewPlayer {
    fn drop(&mut self) {
        let (state, wake) = &*self.snapshots;
        state.lock().unwrap().closed = true;
        wake.notify_one();
        self.stop();
    }
}

fn snapshot_worker(
    snapshots: &Arc<(Mutex<SnapshotState>, Condvar)>,
    current: &AtomicU64,
    tx: &SyncSender<PreviewEvent>,
) {
    loop {
        let request = {
            let (state, wake) = &**snapshots;
            let mut state = state.lock().unwrap();
            while state.pending.is_none() && !state.closed {
                state = wake.wait(state).unwrap();
            }
            if state.closed {
                return;
            }
            state.pending.take().unwrap()
        };
        if current.load(Ordering::SeqCst) != request.generation {
            continue;
        }
        let result: clipperino_core::Result<Arc<RenderImage>> = (|| {
            let (asset, source_at) = media::timeline_source(&request.project, request.at_ms)?;
            let path = std::env::temp_dir().join(format!(
                "clipperino-preview-{}-{}.png",
                std::process::id(),
                request.generation
            ));
            let result = (|| {
                media::frame(
                    &media::asset_path(&request.project_path, asset),
                    source_at,
                    &path,
                    &request.config,
                )?;
                render_frame(fs::read(&path)?, ImageFormat::Png)
            })();
            let _ = fs::remove_file(path);
            result
        })();
        if current.load(Ordering::SeqCst) != request.generation {
            continue;
        }
        match result {
            Ok(image) => {
                let _ = tx.send(PreviewEvent::Frame {
                    generation: request.generation,
                    at_ms: request.at_ms,
                    image,
                    advances_playhead: false,
                });
            }
            Err(err) => {
                let _ = tx.send(PreviewEvent::Error {
                    generation: request.generation,
                    message: err.to_string(),
                });
            }
        }
    }
}

fn render_frame(bytes: Vec<u8>, format: ImageFormat) -> clipperino_core::Result<Arc<RenderImage>> {
    let format = match format {
        ImageFormat::Png => image::ImageFormat::Png,
        ImageFormat::Jpeg => image::ImageFormat::Jpeg,
        _ => return Err(error("unsupported preview image format")),
    };
    let mut pixels = image::load_from_memory_with_format(&bytes, format)?.into_rgba8();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        pixel.swap(0, 2);
    }
    Ok(Arc::new(RenderImage::new([image::Frame::new(pixels)])))
}

fn stream(
    project: &Project,
    project_path: &Path,
    from_ms: u64,
    config: &AppConfig,
    generation: u64,
    current: &AtomicU64,
    tx: &SyncSender<PreviewEvent>,
) -> clipperino_core::Result<()> {
    let mut timeline_start = 0;
    for segment in &project.segments {
        let timeline_end = timeline_start + segment.duration_ms();
        if timeline_end <= from_ms {
            timeline_start = timeline_end;
            continue;
        }
        if current.load(Ordering::SeqCst) != generation {
            break;
        }
        let source = project.asset(&segment.asset_id)?;
        let source_path = media::asset_path(project_path, source);
        let skipped = from_ms.saturating_sub(timeline_start);
        let source_start = segment.source_start_ms + skipped;
        let duration = segment.source_end_ms - source_start;
        let start_seconds = format!("{:.3}", source_start as f64 / 1000.0);
        let duration_seconds = format!("{:.3}", duration as f64 / 1000.0);
        let mut audio = if source.has_audio {
            Some(
                Command::new("mpv")
                    .args(["--no-config", "--no-video", "--msg-level=all=error"])
                    .arg(format!("--start={start_seconds}"))
                    .arg(format!("--length={duration_seconds}"))
                    .arg(&source_path)
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped())
                    .spawn()
                    .map_err(|err| error(format!("Could not start preview audio (mpv): {err}")))?,
            )
        } else {
            None
        };
        let mut decoder = Command::new(&config.ffmpeg)
            .args(["-v", "error", "-re", "-ss"])
            .arg(&start_seconds)
            .arg("-i")
            .arg(&source_path)
            .arg("-t")
            .arg(&duration_seconds)
            .arg("-vf")
            .arg(format!(
                "fps={PREVIEW_FPS},scale={}:-2",
                config.preview_width
            ))
            .args([
                "-an",
                "-q:v",
                "5",
                "-f",
                "image2pipe",
                "-vcodec",
                "mjpeg",
                "-",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdout = decoder.stdout.take().ok_or("ffmpeg did not open stdout")?;
        let mut pending = Vec::new();
        let mut chunk = [0_u8; 16 * 1024];
        let mut index = 0_u64;
        let mut audio_failure = None;
        while current.load(Ordering::SeqCst) == generation {
            let size = stdout.read(&mut chunk)?;
            if size == 0 {
                break;
            }
            if let Some(player) = audio.as_mut()
                && let Some(status) = player.try_wait()?
            {
                if !status.success() {
                    audio_failure = Some(audio_failure_message(player));
                    break;
                }
                audio = None;
            }
            pending.extend_from_slice(&chunk[..size]);
            while let Some(end) = pending.windows(2).position(|bytes| bytes == [0xff, 0xd9]) {
                let start = pending.windows(2).position(|bytes| bytes == [0xff, 0xd8]);
                let frame = start.map(|start| pending[start..end + 2].to_vec());
                pending.drain(..end + 2);
                if let Some(bytes) = frame {
                    let at_ms = timeline_start + skipped + index * 1000 / PREVIEW_FPS;
                    if let Ok(image) = render_frame(bytes, ImageFormat::Jpeg) {
                        let _ = tx.try_send(PreviewEvent::Frame {
                            generation,
                            at_ms,
                            image,
                            advances_playhead: true,
                        });
                    }
                    index += 1;
                }
            }
        }
        let _ = decoder.kill();
        let _ = decoder.wait();
        if let Some(mut audio) = audio.take() {
            if audio_failure.is_none()
                && let Some(status) = audio.try_wait()?
                && !status.success()
            {
                audio_failure = Some(audio_failure_message(&mut audio));
            }
            let _ = audio.kill();
            let _ = audio.wait();
        }
        if let Some(message) = audio_failure {
            return Err(error(message));
        }
        timeline_start = timeline_end;
    }
    Ok(())
}

fn audio_failure_message(player: &mut Child) -> String {
    let mut details = String::new();
    if let Some(stderr) = player.stderr.as_mut() {
        let _ = stderr.read_to_string(&mut details);
    }
    format!(
        "Preview audio failed (mpv): {}",
        details
            .trim()
            .lines()
            .last()
            .unwrap_or("unknown audio error")
    )
}

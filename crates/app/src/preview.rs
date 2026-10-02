//! Preview decoding and playback.
//!
//! FFmpeg writes raw BGRA frames, which is the layout GPUI uploads to the GPU, so frames go
//! from the pipe to a texture with no image decoding in between. During playback one mpv
//! process plays the rest of the timeline as an EDL, which keeps audio gapless across cuts
//! and acts as the clock that video frames are presented against.

use std::{
    collections::HashMap,
    env, fs,
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc::{RecvTimeoutError, SyncSender, TryRecvError, sync_channel},
    },
    thread,
    time::{Duration, Instant},
};

use clipperino_core::{Result, config::AppConfig, error, media, project::Project};
use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui::RenderImage;
use serde_json::{Value, json};

/// Higher source rates are decimated by a whole factor so motion stays even.
const MAX_PREVIEW_FPS: u32 = 60;
/// Frames decoded ahead of the clock. This also hides FFmpeg start-up at cuts.
const FRAME_BUFFER: usize = 12;
const AUDIO_SYNC_INTERVAL: Duration = Duration::from_millis(500);
const AUDIO_SYNC_TOLERANCE_MS: u64 = 30;
const AUDIO_START_TIMEOUT: Duration = Duration::from_secs(5);

/// One stretch of source media, in timeline order.
#[derive(Clone)]
pub struct Part {
    path: PathBuf,
    has_audio: bool,
    width: u32,
    height: u32,
    source_start_ms: u64,
    source_end_ms: u64,
    timeline_start_ms: u64,
}

impl Part {
    fn duration_ms(&self) -> u64 {
        self.source_end_ms - self.source_start_ms
    }

    fn timeline_end_ms(&self) -> u64 {
        self.timeline_start_ms + self.duration_ms()
    }

    /// Output size for preview frames: the source aspect ratio, never upscaled.
    fn frame_size(&self, max_width: u32) -> (u32, u32) {
        let width = (max_width.min(self.width).max(2)) & !1;
        let height = (u64::from(self.height) * u64::from(width) / u64::from(self.width.max(1)))
            .clamp(2, 8192) as u32
            & !1;
        (width, height)
    }
}

/// Returns the parts that play from `from_ms` to the end of the timeline.
pub fn plan(project: &Project, project_path: &Path, from_ms: u64) -> Vec<Part> {
    let mut parts = Vec::new();
    let mut timeline_start = 0;
    for segment in &project.segments {
        let timeline_end = timeline_start + segment.duration_ms();
        if timeline_end > from_ms
            && let Ok(asset) = project.asset(&segment.asset_id)
        {
            let skipped = from_ms.saturating_sub(timeline_start);
            parts.push(Part {
                path: media::asset_path(project_path, asset),
                has_audio: asset.has_audio,
                width: asset.width,
                height: asset.height,
                source_start_ms: segment.source_start_ms + skipped,
                source_end_ms: segment.source_end_ms,
                timeline_start_ms: timeline_start + skipped,
            });
        }
        timeline_start = timeline_end;
    }
    parts
}

pub enum PreviewEvent {
    /// A new frame is waiting in [`PreviewPlayer::take_frame`].
    FrameReady,
    Finished {
        generation: u64,
        at_ms: u64,
    },
    Error {
        generation: u64,
        message: String,
    },
}

#[derive(Clone, Copy)]
struct Clock {
    generation: u64,
    origin: Instant,
    origin_ms: u64,
    end_ms: u64,
}

impl Clock {
    fn now_ms(&self) -> u64 {
        (self.origin_ms + self.origin.elapsed().as_millis() as u64).min(self.end_ms)
    }
}

struct Shared {
    generation: AtomicU64,
    /// Only the newest undisplayed frame is kept, so a busy UI never falls behind.
    frame: Mutex<Option<(u64, Arc<RenderImage>)>>,
    clock: Mutex<Option<Clock>>,
    frame_rates: Mutex<HashMap<PathBuf, (u32, u32)>>,
    events: UnboundedSender<PreviewEvent>,
}

impl Shared {
    fn is_current(&self, generation: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == generation
    }

    fn publish(&self, generation: u64, image: Arc<RenderImage>) {
        if !self.is_current(generation) {
            return;
        }
        let was_empty = self
            .frame
            .lock()
            .unwrap()
            .replace((generation, image))
            .is_none();
        if was_empty {
            let _ = self.events.unbounded_send(PreviewEvent::FrameReady);
        }
    }

    fn send(&self, event: PreviewEvent) {
        let _ = self.events.unbounded_send(event);
    }

    fn frame_rate(&self, path: &Path, config: &AppConfig) -> (u32, u32) {
        if let Some(rate) = self.frame_rates.lock().unwrap().get(path) {
            return *rate;
        }
        let (num, den) = media::video_frame_rate(path, config).unwrap_or((30, 1));
        let factor = num.div_ceil(den.saturating_mul(MAX_PREVIEW_FPS)).max(1);
        let rate = (num, den * factor);
        self.frame_rates
            .lock()
            .unwrap()
            .insert(path.to_owned(), rate);
        rate
    }
}

struct StillRequest {
    part: Part,
    max_width: u32,
    ffmpeg: String,
    generation: u64,
}

#[derive(Default)]
struct StillQueue {
    pending: Option<StillRequest>,
    closed: bool,
}

pub struct PreviewPlayer {
    shared: Arc<Shared>,
    stills: Arc<(Mutex<StillQueue>, Condvar)>,
}

impl PreviewPlayer {
    pub fn new() -> (Self, UnboundedReceiver<PreviewEvent>) {
        let (events, receiver) = unbounded();
        let shared = Arc::new(Shared {
            generation: AtomicU64::new(0),
            frame: Mutex::default(),
            clock: Mutex::default(),
            frame_rates: Mutex::default(),
            events,
        });
        let stills = Arc::new((Mutex::new(StillQueue::default()), Condvar::new()));
        thread::Builder::new()
            .name("preview-stills".into())
            .spawn({
                let shared = shared.clone();
                let stills = stills.clone();
                move || still_worker(&shared, &stills)
            })
            .expect("could not start preview thread");
        (Self { shared, stills }, receiver)
    }

    pub fn is_current(&self, generation: u64) -> bool {
        self.shared.is_current(generation)
    }

    /// Takes the newest decoded frame, if it belongs to the current request.
    pub fn take_frame(&self) -> Option<Arc<RenderImage>> {
        let (generation, image) = self.shared.frame.lock().unwrap().take()?;
        self.is_current(generation).then_some(image)
    }

    /// The playback position, once audio and video have started.
    pub fn position_ms(&self) -> Option<u64> {
        let clock = (*self.shared.clock.lock().unwrap())?;
        self.is_current(clock.generation).then(|| clock.now_ms())
    }

    /// Cancels playback and pending stills, returning where playback was.
    pub fn stop(&self) -> Option<u64> {
        let position = self.position_ms();
        self.shared.generation.fetch_add(1, Ordering::SeqCst);
        *self.shared.clock.lock().unwrap() = None;
        position
    }

    fn next_generation(&self) -> u64 {
        self.shared.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Shows the frame at the start of `part`. Rapid requests coalesce to the latest one.
    pub fn show_frame(&self, part: Part, config: &AppConfig) {
        let generation = self.next_generation();
        *self.shared.clock.lock().unwrap() = None;
        let (queue, wake) = &*self.stills;
        queue.lock().unwrap().pending = Some(StillRequest {
            part,
            max_width: config.preview_width,
            ffmpeg: config.ffmpeg.clone(),
            generation,
        });
        wake.notify_one();
    }

    pub fn play(&self, parts: Vec<Part>, config: AppConfig) {
        let generation = self.next_generation();
        *self.shared.clock.lock().unwrap() = None;
        let shared = self.shared.clone();
        thread::Builder::new()
            .name("preview-playback".into())
            .spawn(move || {
                let end_ms = parts.last().map_or(0, Part::timeline_end_ms);
                let result = playback(&shared, generation, parts, &config);
                if !shared.is_current(generation) {
                    return;
                }
                shared.send(match result {
                    Ok(()) => PreviewEvent::Finished {
                        generation,
                        at_ms: end_ms,
                    },
                    Err(err) => PreviewEvent::Error {
                        generation,
                        message: err.to_string(),
                    },
                });
            })
            .expect("could not start playback thread");
    }
}

impl Drop for PreviewPlayer {
    fn drop(&mut self) {
        let (queue, wake) = &*self.stills;
        queue.lock().unwrap().closed = true;
        wake.notify_one();
        self.stop();
    }
}

fn still_worker(shared: &Shared, stills: &(Mutex<StillQueue>, Condvar)) {
    loop {
        let request = {
            let (queue, wake) = stills;
            let mut queue = queue.lock().unwrap();
            while queue.pending.is_none() && !queue.closed {
                queue = wake.wait(queue).unwrap();
            }
            if queue.closed {
                return;
            }
            queue.pending.take().unwrap()
        };
        if !shared.is_current(request.generation) {
            continue;
        }
        match decode_still(&request) {
            Ok(image) => shared.publish(request.generation, image),
            Err(err) if shared.is_current(request.generation) => shared.send(PreviewEvent::Error {
                generation: request.generation,
                message: err.to_string(),
            }),
            Err(_) => {}
        }
    }
}

fn decode_still(request: &StillRequest) -> Result<Arc<RenderImage>> {
    let part = &request.part;
    let (width, height) = part.frame_size(request.max_width);
    let scale = scale_filter(width, height);
    let first = decode_frames(&request.ffmpeg, part, part.source_start_ms, &scale, true)?;
    let bytes = if first.len() >= frame_len(width, height) {
        first
    } else {
        // Seeking at the very end of a file can land past its last frame; show the last one.
        let start = part.source_start_ms.saturating_sub(500);
        decode_frames(
            &request.ffmpeg,
            part,
            start,
            &format!("fps=8,{scale}"),
            false,
        )?
    };
    let len = frame_len(width, height);
    let Some(last) = bytes.len().checked_div(len).filter(|count| *count > 0) else {
        return Err(error("no preview frame at this position"));
    };
    render_image(width, height, bytes[(last - 1) * len..last * len].to_vec())
}

fn decode_frames(
    ffmpeg: &str,
    part: &Part,
    start_ms: u64,
    filter: &str,
    single: bool,
) -> Result<Vec<u8>> {
    let mut command = decoder_command(ffmpeg, &part.path, start_ms, None, filter);
    if single {
        command.args(["-frames:v", "1"]);
    } else {
        command.args(["-t", "1"]);
    }
    let output = command
        .arg("-")
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| error(format!("could not run {ffmpeg}: {err}")))?;
    if !output.status.success() {
        return Err(error(format!(
            "preview frame failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(output.stdout)
}

fn scale_filter(width: u32, height: u32) -> String {
    // Padding keeps the byte size exact even for rotated or anamorphic sources.
    format!(
        "scale={width}:{height}:force_original_aspect_ratio=decrease,\
         pad={width}:{height}:(ow-iw)/2:(oh-ih)/2,format=bgra"
    )
}

fn frame_len(width: u32, height: u32) -> usize {
    width as usize * height as usize * 4
}

/// Builds an FFmpeg command that writes raw BGRA frames. The caller adds the output.
fn decoder_command(
    ffmpeg: &str,
    path: &Path,
    start_ms: u64,
    duration_ms: Option<u64>,
    filter: &str,
) -> Command {
    let mut command = Command::new(ffmpeg);
    command
        .args(["-v", "error", "-nostdin", "-ss"])
        .arg(seconds(start_ms))
        .arg("-i")
        .arg(path);
    if let Some(duration) = duration_ms {
        command.arg("-t").arg(seconds(duration));
    }
    command
        .args(["-an", "-sn", "-dn", "-vf", filter])
        .args(["-f", "rawvideo", "-pix_fmt", "bgra"])
        .stdin(Stdio::null());
    die_with_parent(&mut command);
    command
}

fn seconds(ms: u64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

fn render_image(width: u32, height: u32, bgra: Vec<u8>) -> Result<Arc<RenderImage>> {
    // GPUI expects BGRA bytes in an RGBA-typed buffer, so no channel swizzle is needed.
    let buffer = image::RgbaImage::from_raw(width, height, bgra)
        .ok_or_else(|| error("preview frame has the wrong size"))?;
    Ok(Arc::new(RenderImage::new([image::Frame::new(buffer)])))
}

/// Kills the child process when playback ends or is interrupted.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(target_os = "linux")]
fn die_with_parent(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: prctl is async-signal-safe and touches no memory shared with the parent.
    unsafe {
        command.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn die_with_parent(_: &mut Command) {}

struct TimedFrame {
    at_ms: u64,
    image: Arc<RenderImage>,
}

fn playback(
    shared: &Arc<Shared>,
    generation: u64,
    parts: Vec<Part>,
    config: &AppConfig,
) -> Result<()> {
    let (Some(first), Some(last)) = (parts.first(), parts.last()) else {
        return Ok(());
    };
    let (from_ms, end_ms) = (first.timeline_start_ms, last.timeline_end_ms());
    let mut audio = if parts.iter().any(|part| part.has_audio) {
        Some(Audio::start(&parts, generation)?)
    } else {
        None
    };
    let (frames, frame_rx) = sync_channel(FRAME_BUFFER);
    let mut decoder = Some(
        thread::Builder::new()
            .name("preview-decoder".into())
            .spawn({
                let shared = shared.clone();
                let config = config.clone();
                move || decode_parts(&shared, &parts, &config, &frames)
            })?,
    );
    // Hold the clock until the first frame is ready so audio and video start together.
    let mut next = loop {
        if !shared.is_current(generation) {
            return Ok(());
        }
        match frame_rx.recv_timeout(Duration::from_millis(20)) {
            Ok(frame) => break Some(frame),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                finish_decoder(&mut decoder)?;
                break None;
            }
        }
    };
    if let Some(player) = audio.as_mut() {
        player.wait_until_loaded(shared, generation)?;
        player.resume()?;
    }
    let mut clock = Clock {
        generation,
        origin: Instant::now(),
        origin_ms: from_ms,
        end_ms,
    };
    *shared.clock.lock().unwrap() = Some(clock);
    // Audio output takes a moment to settle, so check sync early once.
    let mut next_sync = Instant::now() + Duration::from_millis(200);

    while shared.is_current(generation) {
        let now = clock.now_ms();
        // Present the newest due frame, skipping any the decoder fell behind on.
        let mut due = None;
        while let Some(frame) = next.take_if(|frame| frame.at_ms <= now) {
            due = Some(frame.image);
            next = match frame_rx.try_recv() {
                Ok(frame) => Some(frame),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    finish_decoder(&mut decoder)?;
                    None
                }
            };
        }
        if let Some(image) = due {
            shared.publish(generation, image);
        }

        if let Some(player) = audio.as_mut() {
            if !player.is_running()? {
                audio = None;
            } else if Instant::now() >= next_sync {
                next_sync = Instant::now() + AUDIO_SYNC_INTERVAL;
                if let Some(played) = player.position_ms() {
                    let heard = (from_ms + played).min(end_ms);
                    if heard.abs_diff(clock.now_ms()) > AUDIO_SYNC_TOLERANCE_MS {
                        clock.origin = Instant::now();
                        clock.origin_ms = heard;
                        *shared.clock.lock().unwrap() = Some(clock);
                    }
                }
            }
        }

        let decoding = decoder.is_some();
        match &next {
            None if !decoding && now >= end_ms => break,
            Some(frame) => thread::sleep(Duration::from_millis((frame.at_ms - now).clamp(1, 10))),
            None if decoding => match frame_rx.recv_timeout(Duration::from_millis(10)) {
                Ok(frame) => next = Some(frame),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => finish_decoder(&mut decoder)?,
            },
            None => thread::sleep(Duration::from_millis(10)),
        }
    }
    drop(frame_rx);
    let decoded = finish_decoder(&mut decoder);
    if shared.is_current(generation) {
        decoded?;
    }
    Ok(())
}

fn finish_decoder(decoder: &mut Option<thread::JoinHandle<Result<()>>>) -> Result<()> {
    match decoder.take().map(thread::JoinHandle::join) {
        Some(Ok(result)) => result,
        Some(Err(_)) => Err(error("preview decoder crashed")),
        None => Ok(()),
    }
}

fn decode_parts(
    shared: &Shared,
    parts: &[Part],
    config: &AppConfig,
    frames: &SyncSender<TimedFrame>,
) -> Result<()> {
    for part in parts {
        let (num, den) = shared.frame_rate(&part.path, config);
        let (width, height) = part.frame_size(config.preview_width);
        let filter = format!("fps={num}/{den},{}", scale_filter(width, height));
        let mut command = decoder_command(
            &config.ffmpeg,
            &part.path,
            part.source_start_ms,
            Some(part.duration_ms()),
            &filter,
        );
        let mut child = ChildGuard(
            command
                .arg("-")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|err| error(format!("could not run {}: {err}", config.ffmpeg)))?,
        );
        let mut stdout = child.0.stdout.take().ok_or("ffmpeg did not open stdout")?;
        for index in 0_u64.. {
            let mut bytes = vec![0; frame_len(width, height)];
            if !read_frame(&mut stdout, &mut bytes)? {
                break;
            }
            let at_ms = part.timeline_start_ms + index * 1000 * u64::from(den) / u64::from(num);
            let frame = TimedFrame {
                at_ms,
                image: render_image(width, height, bytes)?,
            };
            if frames.send(frame).is_err() {
                // Playback stopped; the guard kills FFmpeg.
                return Ok(());
            }
        }
        let status = child.0.wait()?;
        if !status.success() {
            let mut details = String::new();
            if let Some(stderr) = child.0.stderr.as_mut() {
                let _ = stderr.read_to_string(&mut details);
            }
            return Err(error(format!(
                "preview decoding failed: {}",
                details.trim()
            )));
        }
    }
    Ok(())
}

/// Fills `buffer` with one frame. Returns false at the end of the stream.
fn read_frame(reader: &mut impl Read, buffer: &mut [u8]) -> Result<bool> {
    let mut filled = 0;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => return Ok(false),
            Ok(size) => filled += size,
            Err(err) if err.kind() == ErrorKind::Interrupted => {}
            Err(err) => return Err(err.into()),
        }
    }
    Ok(true)
}

/// Preview audio for the whole remaining timeline, controlled over mpv's JSON IPC.
struct Audio {
    child: ChildGuard,
    socket: PathBuf,
    ipc: Option<Ipc>,
}

impl Audio {
    fn start(parts: &[Part], generation: u64) -> Result<Self> {
        let socket = env::temp_dir().join(format!(
            "clipperino-audio-{}-{generation}.sock",
            std::process::id()
        ));
        let _ = fs::remove_file(&socket);
        let mut command = Command::new("mpv");
        command
            .args([
                "--no-config",
                "--no-video",
                "--pause",
                "--idle=no",
                "--audio-display=no",
                "--msg-level=all=error",
            ])
            .arg(format!("--input-ipc-server={}", socket.display()))
            .arg(edl(parts))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        die_with_parent(&mut command);
        let child = command
            .spawn()
            .map_err(|err| error(format!("could not start preview audio (mpv): {err}")))?;
        Ok(Self {
            child: ChildGuard(child),
            socket,
            ipc: None,
        })
    }

    fn wait_until_loaded(&mut self, shared: &Shared, generation: u64) -> Result<()> {
        let deadline = Instant::now() + AUDIO_START_TIMEOUT;
        while shared.is_current(generation) && Instant::now() < deadline {
            if !self.is_running()? {
                return Ok(());
            }
            if self.ipc.is_none() {
                self.ipc = Ipc::connect(&self.socket);
            }
            if let Some(ipc) = self.ipc.as_mut()
                && ipc.request(json!(["get_property", "duration"])).is_ok()
            {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(5));
        }
        if shared.is_current(generation) {
            return Err(error("preview audio (mpv) did not start in time"));
        }
        Ok(())
    }

    fn resume(&mut self) -> Result<()> {
        if let Some(ipc) = self.ipc.as_mut() {
            ipc.request(json!(["set_property", "pause", false]))?;
        }
        Ok(())
    }

    fn position_ms(&mut self) -> Option<u64> {
        let seconds = self
            .ipc
            .as_mut()?
            .request(json!(["get_property", "playback-time"]))
            .ok()?
            .as_f64()?;
        Some((seconds.max(0.0) * 1000.0).round() as u64)
    }

    /// Returns false once mpv has finished, and an error if it failed.
    fn is_running(&mut self) -> Result<bool> {
        match self.child.0.try_wait()? {
            None => Ok(true),
            Some(status) if status.success() => Ok(false),
            Some(_) => {
                let mut details = String::new();
                if let Some(stderr) = self.child.0.stderr.as_mut() {
                    let _ = stderr.read_to_string(&mut details);
                }
                Err(error(format!(
                    "Preview audio failed (mpv): {}",
                    details
                        .trim()
                        .lines()
                        .last()
                        .unwrap_or("unknown audio error")
                )))
            }
        }
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket);
    }
}

/// An mpv EDL that plays every part back to back without gaps.
fn edl(parts: &[Part]) -> String {
    let entries: Vec<String> = parts
        .iter()
        .map(|part| {
            let path = part.path.to_string_lossy();
            // `%length%` quotes the path so commas and semicolons in it are safe.
            format!(
                "%{}%{path},start={},length={}",
                path.len(),
                seconds(part.source_start_ms),
                seconds(part.duration_ms())
            )
        })
        .collect();
    format!("edl://{}", entries.join(";"))
}

struct Ipc {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
    next_id: u64,
}

impl Ipc {
    fn connect(socket: &Path) -> Option<Self> {
        let stream = UnixStream::connect(socket).ok()?;
        stream
            .set_read_timeout(Some(Duration::from_millis(250)))
            .ok()?;
        Some(Self {
            writer: stream.try_clone().ok()?,
            reader: BufReader::new(stream),
            next_id: 1,
        })
    }

    fn request(&mut self, command: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let mut line = json!({ "command": command, "request_id": id }).to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes())?;
        loop {
            let mut reply = String::new();
            if self.reader.read_line(&mut reply)? == 0 {
                return Err(error("mpv closed its control socket"));
            }
            let reply: Value = serde_json::from_str(&reply)?;
            if reply["request_id"] != id {
                continue; // An unrelated event.
            }
            return if reply["error"] == "success" {
                Ok(reply["data"].clone())
            } else {
                Err(error(format!("mpv: {}", reply["error"])))
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_size_keeps_aspect_and_never_upscales() {
        let part = Part {
            path: PathBuf::new(),
            has_audio: false,
            width: 1920,
            height: 1080,
            source_start_ms: 0,
            source_end_ms: 1,
            timeline_start_ms: 0,
        };
        assert_eq!(part.frame_size(960), (960, 540));
        assert_eq!(part.frame_size(3840), (1920, 1080));
    }

    #[test]
    fn edl_quotes_paths() {
        let part = Part {
            path: PathBuf::from("/a,b.mp4"),
            has_audio: true,
            width: 2,
            height: 2,
            source_start_ms: 2_000,
            source_end_ms: 3_500,
            timeline_start_ms: 0,
        };
        assert_eq!(edl(&[part]), "edl://%8%/a,b.mp4,start=2.000,length=1.500");
    }
}

/// Plays a real clip through FFmpeg and mpv. Run with
/// `CLIPPERINO_TEST_VIDEO=/path/to/clip.mp4 cargo test -p clipperino-app -- --ignored`.
#[cfg(test)]
mod playback_tests {
    use super::*;
    use clipperino_core::project::Asset;
    use futures::StreamExt;

    #[test]
    #[ignore = "needs ffmpeg, mpv, an audio device, and CLIPPERINO_TEST_VIDEO"]
    fn plays_across_a_cut_in_real_time() {
        let path = PathBuf::from(env::var("CLIPPERINO_TEST_VIDEO").unwrap());
        let config = AppConfig::default();
        let mut asset = media::probe(&path, &config).unwrap();
        asset.duration_ms = asset.duration_ms.min(20_000);
        let mut project = Project::default();
        project.add_asset(Asset { ..asset }).unwrap();
        project.remove_timeline_range(1_500, 6_000).unwrap();
        let parts = plan(&project, Path::new("/"), 500);
        let expected_ms = parts.last().unwrap().timeline_end_ms() - 500;
        let (player, mut events) = PreviewPlayer::new();
        let started = Instant::now();
        player.play(parts, config);
        let (mut frames, mut first_frame) = (0, None);
        let finished_at = futures::executor::block_on(async {
            while let Some(event) = events.next().await {
                match event {
                    PreviewEvent::FrameReady => {
                        if player.take_frame().is_some() {
                            frames += 1;
                            first_frame.get_or_insert(started.elapsed());
                        }
                    }
                    PreviewEvent::Finished { at_ms, .. } => return at_ms,
                    PreviewEvent::Error { message, .. } => panic!("{message}"),
                }
            }
            unreachable!()
        });
        let elapsed = started.elapsed().as_millis() as u64;
        eprintln!(
            "frames={frames} first_frame={first_frame:?} elapsed={elapsed}ms expected={expected_ms}ms"
        );
        assert_eq!(finished_at, expected_ms + 500);
        assert!(elapsed >= expected_ms && elapsed < expected_ms + 1_000);
        assert!(
            frames as u64 >= expected_ms * 25 / 1000,
            "too few frames shown"
        );
    }

    #[test]
    #[ignore = "needs ffmpeg and CLIPPERINO_TEST_VIDEO"]
    fn decodes_stills_including_the_last_frame() {
        let path = PathBuf::from(env::var("CLIPPERINO_TEST_VIDEO").unwrap());
        let config = AppConfig::default();
        let asset = media::probe(&path, &config).unwrap();
        let mut project = Project::default();
        project.add_asset(asset.clone()).unwrap();
        for at in [0, asset.duration_ms / 2, asset.duration_ms - 1] {
            let part = plan(&project, Path::new("/"), at).remove(0);
            let started = Instant::now();
            let image = decode_still(&StillRequest {
                part,
                max_width: config.preview_width,
                ffmpeg: config.ffmpeg.clone(),
                generation: 0,
            })
            .unwrap();
            eprintln!(
                "still at {at}ms: {:?} in {:?}",
                image.size(0),
                started.elapsed()
            );
        }
    }
}

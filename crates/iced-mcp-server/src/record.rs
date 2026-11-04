//! Screen recording: screenshots of a window on a fixed clock, written as
//! numbered PNGs and, when `ffmpeg` is installed, encoded to an mp4.
//!
//! The recorder holds its own connection to the bridge, so the agent keeps
//! acting while it runs. A frame identical to the previous one is not
//! written (Argent's `trimStatic`): stills cost nothing and the video plays
//! the changes back to back.

use crate::conn::Conn;
use base64::Engine as _;
use iced_mcp_protocol::{Request, Response, ScreenshotParams};

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub(crate) struct Recording {
    pub(crate) dir: PathBuf,
    pub(crate) app: String,
    pub(crate) started: Instant,
    pub(crate) limit: Duration,
    pub(crate) fps: u32,
    stop: Arc<AtomicBool>,
    frames: Arc<AtomicUsize>,
    task: tokio::task::JoinHandle<Option<String>>,
}

pub(crate) struct Options {
    pub(crate) fps: u32,
    pub(crate) limit: Duration,
    pub(crate) max_width: u32,
    pub(crate) window: Option<String>,
}

pub(crate) async fn start(
    port: u16,
    token: Option<String>,
    app: String,
    dir: PathBuf,
    options: Options,
) -> Result<Recording, String> {
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let mut conn = Conn::open(port, token).await?;
    let stop = Arc::new(AtomicBool::new(false));
    let frames = Arc::new(AtomicUsize::new(0));
    let fps = options.fps.clamp(1, 30);
    let task = {
        let (stop, frames, dir) = (stop.clone(), frames.clone(), dir.clone());
        let period = Duration::from_secs_f64(1.0 / f64::from(fps));
        let limit = options.limit;
        tokio::spawn(async move {
            let started = Instant::now();
            let mut last: Option<Vec<u8>> = None;
            let mut ticker = tokio::time::interval(period);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            while !stop.load(Ordering::Relaxed) && started.elapsed() < limit {
                ticker.tick().await;
                let shot = conn
                    .request(Request::Screenshot(ScreenshotParams {
                        window: options.window.clone(),
                        region: None,
                        max_width: Some(options.max_width),
                    }))
                    .await;
                let png = match shot {
                    Ok(Response::Screenshot(shot)) => base64::engine::general_purpose::STANDARD
                        .decode(shot.png_base64)
                        .ok(),
                    Ok(_) => None,
                    Err(error) => return Some(format!("recording stopped early: {error}")),
                };
                let Some(png) = png else { continue };
                if last.as_ref() == Some(&png) {
                    continue;
                }
                let n = frames.fetch_add(1, Ordering::Relaxed) + 1;
                let _ = std::fs::write(dir.join(format!("frame_{n:05}.png")), &png);
                last = Some(png);
            }
            None
        })
    };
    Ok(Recording {
        dir,
        app,
        started: Instant::now(),
        limit: options.limit,
        fps,
        stop,
        frames,
        task,
    })
}

impl Recording {
    pub(crate) fn frames(&self) -> usize {
        self.frames.load(Ordering::Relaxed)
    }

    /// The note every tool reply carries while a recording runs.
    pub(crate) fn reminder(&self) -> String {
        format!(
            "NOTE: screen recording of {} running for {:.0}s ({} frames; auto-stops at {}s) — call record_stop to get the video.",
            self.app,
            self.started.elapsed().as_secs_f32(),
            self.frames(),
            self.limit.as_secs()
        )
    }

    /// Stops, then encodes `recording.mp4` if `ffmpeg` is on PATH.
    pub(crate) async fn stop(self) -> String {
        self.stop.store(true, Ordering::Relaxed);
        let early = self.task.await.ok().flatten();
        let frames = self.frames.load(Ordering::Relaxed);
        let seconds = self.started.elapsed().as_secs_f32();
        let mut text = format!(
            "Recorded {frames} distinct frame(s) over {seconds:.1}s into {}",
            self.dir.display()
        );
        if let Some(early) = early {
            text = format!("{early}\n{text}");
        }
        if frames == 0 {
            return text;
        }
        match encode(&self.dir, self.fps).await {
            Ok(video) => format!("{text}\nVideo: {}", video.display()),
            Err(error) => format!("{text}\n{error}"),
        }
    }
}

async fn encode(dir: &Path, fps: u32) -> Result<PathBuf, String> {
    let video = dir.join("recording.mp4");
    let output = tokio::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-framerate"])
        .arg(fps.to_string())
        .arg("-i")
        .arg(dir.join("frame_%05d.png"))
        .args([
            "-vf",
            "scale=trunc(iw/2)*2:trunc(ih/2)*2",
            "-c:v",
            "libx264",
            "-crf",
            "20",
            "-preset",
            "veryfast",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ])
        .arg(&video)
        .output()
        .await
        .map_err(|_| {
            "ffmpeg was not found on PATH, so the frames stay as PNGs. Install it (`brew install ffmpeg`) to get an mp4.".to_owned()
        })?;
    if output.status.success() {
        Ok(video)
    } else {
        Err(format!(
            "ffmpeg failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

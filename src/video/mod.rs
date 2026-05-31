//! A minimal ffmpeg-backed video player widget for iced.
//!
//! Design: a background thread decodes frames into a bounded queue (see
//! [`decoder`]); the UI advances a playback clock each frame and presents the
//! decoded frame nearest the clock. This mirrors the approach in ~/code/finn,
//! pared down to a single file. Intended to be extracted into a standalone
//! `iced_ffmpeg_video_player` crate once the design settles.
//!
//! Phase A (current): video only, wall-clock master, presented via
//! `image::Handle::from_rgba`. Audio (cpal) and a custom wgpu primitive follow.

mod audio;
mod decoder;
mod render;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use audio::AudioOutput;
pub use decoder::RgbaFrame;
use decoder::Shared;
pub use render::{VideoPipeline, VideoProgram};

/// Distinct id per player, so the shared GPU pipeline keys each video's texture.
static NEXT_PLAYER_ID: AtomicU64 = AtomicU64::new(0);

/// A playing video. Owns the decoder thread (stopped on drop) and the temp file
/// backing ffmpeg's input (deleted on drop).
pub struct Player {
    id: u64,
    shared: Arc<Shared>,
    _file: tempfile::TempPath,
    /// cpal output; `None` if there's no device. When present and the input has
    /// audio, its playback position is the master clock.
    audio: Option<AudioOutput>,
    /// Frame currently presented to the UI.
    current: Option<Arc<RgbaFrame>>,
    /// Bumped whenever `current` changes (so the GPU renderer skips re-uploads).
    version: u64,
    /// Wall-clock origin (fallback clock for audioless inputs).
    start: Option<Instant>,
}

impl Player {
    /// Write `bytes` to a temp file and start decoding it. We buffer to a file
    /// (rather than stream a URL) because booru CDNs reject ffmpeg's default HTTP
    /// headers — the bytes are fetched with our own client first.
    pub fn from_bytes(bytes: &[u8]) -> std::io::Result<Self> {
        use std::io::Write;
        let mut file = tempfile::Builder::new().prefix("ribb-video-").tempfile()?;
        file.write_all(bytes)?;
        Ok(Player::open(file.into_temp_path()))
    }

    /// Start decoding `file`. The temp file is kept alive for the player's life.
    fn open(file: tempfile::TempPath) -> Self {
        let audio = AudioOutput::new();
        let ring = audio.as_ref().map(|a| a.ring());
        let shared = decoder::spawn(file.to_path_buf(), ring);
        Player {
            id: NEXT_PLAYER_ID.fetch_add(1, Ordering::Relaxed),
            shared,
            _file: file,
            audio,
            current: None,
            version: 0,
            start: None,
        }
    }

    /// Advance presentation toward `now`, promoting any frames whose time has
    /// come. Returns true if the displayed frame changed.
    pub fn tick(&mut self, now: Instant) -> bool {
        // Show the first decoded frame as soon as it exists, regardless of clock.
        if self.current.is_none() {
            if let Some(frame) = self.shared.frames.lock().unwrap().pop_front() {
                self.current = Some(Arc::new(frame));
                self.version += 1;
                return true;
            }
            return false;
        }
        // Wait for the decoder to resolve the stream info (audio vs not) so we
        // pick the right clock and don't briefly run the wall clock then jump.
        if !self.shared.opened.load(Ordering::Acquire) {
            return false;
        }

        let clock_ms = self.clock_ms(now);
        let mut queue = self.shared.frames.lock().unwrap();
        let mut advanced = false;
        while let Some(front) = queue.front() {
            if front.pts_ms <= clock_ms {
                self.current = Some(Arc::new(queue.pop_front().unwrap()));
                self.version += 1;
                advanced = true;
            } else {
                break;
            }
        }
        advanced
    }

    /// Current playback position in ms: the audio clock when the input has
    /// audio, otherwise a wall clock anchored to the first post-open tick.
    fn clock_ms(&mut self, now: Instant) -> i64 {
        if self.shared.has_audio.load(Ordering::Acquire) {
            if let Some(audio) = &self.audio {
                return audio.clock_ms();
            }
        }
        let start = *self.start.get_or_insert(now);
        now.saturating_duration_since(start).as_millis() as i64
    }

    /// Whether any frame has decoded yet (for a loading placeholder).
    pub fn has_frame(&self) -> bool {
        self.current.is_some()
    }

    /// The `shader` program to render this player's current frame.
    pub fn program(&self) -> VideoProgram {
        VideoProgram {
            id: self.id,
            version: self.version,
            frame: self.current.clone(),
        }
    }
}

impl Drop for Player {
    fn drop(&mut self) {
        // Signal the decoder thread to exit; it drops its `Shared` ref and the
        // temp file is removed once `_file` drops.
        self.shared.quit.store(true, Ordering::Release);
    }
}

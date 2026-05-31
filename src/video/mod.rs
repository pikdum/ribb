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

    // --- playback clock / controls -------------------------------------
    playing: bool,
    muted: bool,
    /// Playback position (ms, monotonic timeline) at the last anchor event
    /// (open / seek / wall-clock resume).
    base_ms: i64,
    /// `audio.frames_played()` at the last anchor (audio-clock rebasing).
    frames_base: u64,
    /// Wall-clock anchor for audioless inputs; `None` while paused.
    wall_anchor: Option<Instant>,
    /// Position computed on the last `tick`, so the view can read it without
    /// needing an `Instant`.
    last_position_ms: i64,
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
            playing: true,
            muted: false,
            base_ms: 0,
            frames_base: 0,
            wall_anchor: None,
            last_position_ms: 0,
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
        // Anchor the wall clock on the first post-open tick (audioless inputs).
        if self.wall_anchor.is_none() && self.playing && !self.has_audio_clock() {
            self.wall_anchor = Some(now);
        }

        let clock_ms = self.position_ms(now);
        self.last_position_ms = clock_ms;

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

    /// Whether the audio sample count is the master clock (input has audio and
    /// an output device exists). Otherwise a wall clock is used.
    fn has_audio_clock(&self) -> bool {
        self.audio.is_some() && self.shared.has_audio.load(Ordering::Acquire)
    }

    /// Current playback position (ms, monotonic). Audio: rebased sample count
    /// (frozen while the cpal stream is paused). Wall: elapsed since the anchor.
    fn position_ms(&self, now: Instant) -> i64 {
        if self.has_audio_clock() {
            if let Some(audio) = &self.audio {
                let played = audio.frames_played().saturating_sub(self.frames_base);
                let rate = audio.sample_rate().max(1) as u64;
                return self.base_ms + (played * 1000 / rate) as i64;
            }
        }
        match self.wall_anchor {
            Some(anchor) if self.playing => {
                self.base_ms + now.saturating_duration_since(anchor).as_millis() as i64
            }
            _ => self.base_ms,
        }
    }

    /// Toggle play/pause: pause the cpal stream (freezing the audio clock) or
    /// freeze/resume the wall clock.
    pub fn toggle_playing(&mut self, now: Instant) {
        let pos = self.position_ms(now);
        self.playing = !self.playing;
        if let Some(audio) = &self.audio {
            audio.set_playing(self.playing);
        }
        if !self.has_audio_clock() {
            self.base_ms = pos;
            self.wall_anchor = if self.playing { Some(now) } else { None };
        }
    }

    pub fn toggle_muted(&mut self) {
        self.muted = !self.muted;
        if let Some(audio) = &self.audio {
            audio.set_muted(self.muted);
        }
    }

    /// Seek to `file_ms` within the clip (0..duration), staying in the current
    /// loop. Rebases the clock and tells the decoder to jump + drop buffers.
    pub fn seek(&mut self, file_ms: i64, now: Instant) {
        let dur = self.duration_ms();
        if dur <= 0 {
            return;
        }
        let file_t = file_ms.clamp(0, dur);
        let loop_base = (self.position_ms(now) / dur) * dur;
        let target = loop_base + file_t;

        self.shared.seek_target_ms.store(target, Ordering::Release);
        self.shared.seek_gen.fetch_add(1, Ordering::Release);

        self.base_ms = target;
        self.last_position_ms = target;
        if let Some(audio) = &self.audio {
            self.frames_base = audio.frames_played();
            audio.clear();
        }
        self.wall_anchor = if self.playing { Some(now) } else { None };
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn is_muted(&self) -> bool {
        self.muted
    }

    pub fn duration_ms(&self) -> i64 {
        self.shared.duration_ms.load(Ordering::Acquire)
    }

    /// Position within the current loop (0..duration), for the seek bar.
    pub fn position_ms_in_loop(&self) -> i64 {
        let dur = self.duration_ms();
        if dur <= 0 {
            return 0;
        }
        self.last_position_ms.rem_euclid(dur)
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

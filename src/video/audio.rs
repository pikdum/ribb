//! cpal audio output for the video player.
//!
//! A background cpal stream pulls interleaved f32 samples from a shared ring the
//! decode thread fills (after resampling to the device format). `samples_played`
//! — real, non-silence frames — is the master playback clock the video syncs to.
//! Mirrors iced_ruffle's cpal backend, minus ruffle's mixer.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Shared between the decode thread (producer) and the cpal callback (consumer).
pub struct AudioRing {
    buf: Mutex<VecDeque<f32>>,
    /// Per-channel sample frames actually played (silence excluded) — the clock.
    samples_played: AtomicU64,
    pub channels: u16,
    pub sample_rate: u32,
    /// Backpressure threshold (~1s of interleaved samples).
    cap: usize,
}

impl AudioRing {
    /// Append interleaved samples (called by the decoder).
    pub fn push(&self, samples: &[f32]) {
        self.buf.lock().unwrap().extend(samples.iter().copied());
    }

    /// Whether the ring is at capacity (decoder backpressure).
    pub fn is_full(&self) -> bool {
        self.buf.lock().unwrap().len() >= self.cap
    }

    /// Per-channel frames played so far (the playback clock numerator).
    pub fn frames_played(&self) -> u64 {
        self.samples_played.load(Ordering::Relaxed)
    }
}

/// Owns the cpal output stream (kept alive while playing) and the shared ring.
pub struct AudioOutput {
    _stream: cpal::Stream,
    ring: Arc<AudioRing>,
}

impl AudioOutput {
    /// Open the default output device, or `None` if there's no device / it can't
    /// be configured (the player then falls back to a wall clock, no sound).
    pub fn new() -> Option<Self> {
        let host = cpal::default_host();
        let device = host.default_output_device()?;
        let supported = device.default_output_config().ok()?;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let channels = config.channels;
        let sample_rate = config.sample_rate.0;
        let ring = Arc::new(AudioRing {
            buf: Mutex::new(VecDeque::new()),
            samples_played: AtomicU64::new(0),
            channels,
            sample_rate,
            cap: sample_rate as usize * channels as usize,
        });

        let on_err = |e| tracing::error!("audio stream error: {e}");
        let stream = {
            let ring = Arc::clone(&ring);
            match sample_format {
                cpal::SampleFormat::F32 => device.build_output_stream(
                    &config,
                    move |out: &mut [f32], _| fill(&ring, out, |s| s),
                    on_err,
                    None,
                ),
                cpal::SampleFormat::I16 => device.build_output_stream(
                    &config,
                    move |out: &mut [i16], _| fill(&ring, out, f32_to_i16),
                    on_err,
                    None,
                ),
                cpal::SampleFormat::U16 => device.build_output_stream(
                    &config,
                    move |out: &mut [u16], _| {
                        fill(&ring, out, |s| (f32_to_i16(s) as i32 + 32768) as u16)
                    },
                    on_err,
                    None,
                ),
                other => {
                    tracing::warn!("unsupported audio sample format: {other:?}");
                    return None;
                }
            }
            .ok()?
        };
        stream.play().ok()?;
        Some(AudioOutput {
            _stream: stream,
            ring,
        })
    }

    pub fn ring(&self) -> Arc<AudioRing> {
        Arc::clone(&self.ring)
    }

    /// Current playback position in ms (per-channel frames / sample rate).
    pub fn clock_ms(&self) -> i64 {
        if self.ring.sample_rate == 0 {
            return 0;
        }
        (self.ring.frames_played() * 1000 / self.ring.sample_rate as u64) as i64
    }
}

fn f32_to_i16(s: f32) -> i16 {
    (s.clamp(-1.0, 1.0) * 32767.0) as i16
}

/// Fill `out` from the ring, advancing the clock by the real frames written.
/// On underrun the remainder is silence and the clock does not advance (so the
/// video, which follows this clock, waits rather than drifting ahead).
fn fill<T: Copy>(ring: &AudioRing, out: &mut [T], conv: impl Fn(f32) -> T) {
    let mut real = 0usize;
    {
        let mut buf = ring.buf.lock().unwrap();
        for slot in out.iter_mut() {
            match buf.pop_front() {
                Some(s) => {
                    *slot = conv(s);
                    real += 1;
                }
                None => *slot = conv(0.0),
            }
        }
    }
    if ring.channels > 0 {
        ring.samples_played
            .fetch_add((real / ring.channels as usize) as u64, Ordering::Relaxed);
    }
}

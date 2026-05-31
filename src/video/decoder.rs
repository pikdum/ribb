//! Video + audio decode on a background thread, built on ffmpeg-next (libav).
//!
//! The thread demuxes one file: video packets decode to tightly-packed RGBA8
//! (via libswscale) and land on a bounded queue the UI drains; audio packets
//! decode + resample (via libswresample) to the device's interleaved-f32 format
//! and feed the cpal ring (see [`super::audio`]). The audio playback position is
//! the master clock the UI syncs video to.
//!
//! Adapted from ~/code/finn's per-angle video decoder (which is video-only),
//! minus the grid/segment machinery (single file, no seek yet).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use ffmpeg::channel_layout::ChannelLayout;
use ffmpeg::format::sample::Type as SampleType;
use ffmpeg::format::{Pixel, Sample};
use ffmpeg::media::Type;
use ffmpeg::software::resampling::Context as Resampler;
use ffmpeg::software::scaling::{Context as Scaler, Flags};
use ffmpeg::util::frame::audio::Audio as AudioFrame;
use ffmpeg::util::frame::video::Video as VideoFrame;
use ffmpeg_next as ffmpeg;

use super::audio::AudioRing;

/// A decoded frame, already converted to tightly-packed RGBA8.
pub struct RgbaFrame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    pub pts_ms: i64,
}

/// How many decoded frames to buffer ahead before the decoder thread backs off.
const VIDEO_QUEUE_CAP: usize = 8;

/// Shared state between the UI thread and the decoder thread.
pub struct Shared {
    pub quit: AtomicBool,
    /// Set once the decoder has opened the input and resolved its streams.
    pub opened: AtomicBool,
    /// Whether the input has a usable audio stream (decides the clock source).
    pub has_audio: AtomicBool,
    /// Set once the decoder has produced its last frame.
    pub eof: AtomicBool,
    /// Clip duration in ms; 0 until known / on open failure.
    pub duration_ms: AtomicI64,
    /// Bumped by the UI to request a seek to `seek_target_ms` (absolute, in the
    /// monotonic playback timeline the clock uses).
    pub seek_gen: AtomicU64,
    pub seek_target_ms: AtomicI64,
    /// Decoded frames in ascending pts order; UI pops, decoder pushes.
    pub frames: Mutex<VecDeque<RgbaFrame>>,
}

impl Shared {
    fn new() -> Arc<Self> {
        Arc::new(Shared {
            quit: AtomicBool::new(false),
            opened: AtomicBool::new(false),
            has_audio: AtomicBool::new(false),
            eof: AtomicBool::new(false),
            duration_ms: AtomicI64::new(0),
            seek_gen: AtomicU64::new(0),
            seek_target_ms: AtomicI64::new(0),
            frames: Mutex::new(VecDeque::new()),
        })
    }
}

static FFMPEG_INIT: std::sync::Once = std::sync::Once::new();

/// Spawn a decoder thread for `path`. If `audio_ring` is `Some`, the input's
/// audio (when present) is resampled into it. Returns immediately.
pub fn spawn(path: PathBuf, audio_ring: Option<Arc<AudioRing>>) -> Arc<Shared> {
    FFMPEG_INIT.call_once(|| {
        if let Err(e) = ffmpeg::init() {
            tracing::error!("ffmpeg init failed: {e}");
        }
    });
    let shared = Shared::new();
    let thread_shared = Arc::clone(&shared);
    thread::Builder::new()
        .name("ribb-video".into())
        .spawn(move || {
            if let Err(e) = run(&path, &thread_shared, audio_ring) {
                tracing::warn!("video decode failed: {e}");
            }
            thread_shared.opened.store(true, Ordering::Release);
            thread_shared.eof.store(true, Ordering::Release);
        })
        .expect("spawn video decoder thread");
    shared
}

/// libav audio decode + resample-to-device for one input.
struct AudioStream {
    index: usize,
    decoder: ffmpeg::decoder::Audio,
    resampler: Resampler,
    out_channels: u16,
    out_rate: u32,
}

fn open_audio(ictx: &ffmpeg::format::context::Input, ring: &AudioRing) -> Option<AudioStream> {
    let stream = ictx.streams().best(Type::Audio)?;
    let index = stream.index();
    let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters()).ok()?;
    let decoder = ctx.decoder().audio().ok()?;

    let in_layout = {
        let l = decoder.channel_layout();
        if l.is_empty() {
            ChannelLayout::default(decoder.channels() as i32)
        } else {
            l
        }
    };
    let resampler = Resampler::get(
        decoder.format(),
        in_layout,
        decoder.rate(),
        Sample::F32(SampleType::Packed),
        ChannelLayout::default(ring.channels as i32),
        ring.sample_rate,
    )
    .ok()?;

    Some(AudioStream {
        index,
        decoder,
        resampler,
        out_channels: ring.channels,
        out_rate: ring.sample_rate,
    })
}

fn drain_audio(audio: &mut AudioStream, ring: &AudioRing) {
    let mut frame = AudioFrame::empty();
    while audio.decoder.receive_frame(&mut frame).is_ok() {
        let in_rate = frame.rate().max(1);
        // Size the output generously so swresample doesn't buffer the excess
        // internally (which would grow unbounded when upsampling, e.g. 44.1→48k).
        let cap = (frame.samples() as i64 * audio.out_rate as i64 / in_rate as i64) as usize + 1024;
        let mut out = AudioFrame::new(
            Sample::F32(SampleType::Packed),
            cap,
            ChannelLayout::default(audio.out_channels as i32),
        );
        if audio.resampler.run(&frame, &mut out).is_err() {
            continue;
        }
        let produced = out.samples() * audio.out_channels as usize;
        let bytes = out.data(0);
        let floats = bytes.len() / 4;
        let raw: &[f32] = bytemuck::cast_slice(&bytes[..floats * 4]);
        ring.push(&raw[..produced.min(raw.len())]);
    }
}

fn make_rgba(scaler: &mut Option<Scaler>, decoded: VideoFrame, tb_secs: f64) -> RgbaFrame {
    let w = decoded.width();
    let h = decoded.height();
    let scaler = scaler.get_or_insert_with(|| {
        Scaler::get(decoded.format(), w, h, Pixel::RGBA, w, h, Flags::BILINEAR)
            .expect("create sws scaler")
    });

    let mut rgba = VideoFrame::empty();
    scaler.run(&decoded, &mut rgba).expect("scale frame");

    // sws output rows may be padded; copy into a tight w*4 buffer.
    let stride = rgba.stride(0);
    let row_bytes = (w * 4) as usize;
    let src = rgba.data(0);
    let mut data = vec![0u8; row_bytes * h as usize];
    for y in 0..h as usize {
        let s = y * stride;
        let d = y * row_bytes;
        data[d..d + row_bytes].copy_from_slice(&src[s..s + row_bytes]);
    }

    let pts = decoded.pts().or_else(|| decoded.timestamp()).unwrap_or(0);
    let pts_ms = (pts as f64 * tb_secs * 1000.0) as i64;

    RgbaFrame {
        width: w,
        height: h,
        data,
        pts_ms,
    }
}

fn drain_video(
    decoder: &mut ffmpeg::decoder::Video,
    scaler: &mut Option<Scaler>,
    tb_secs: f64,
    pts_offset: i64,
    shared: &Shared,
) {
    let mut frame = VideoFrame::empty();
    while decoder.receive_frame(&mut frame).is_ok() {
        let mut rgba = make_rgba(scaler, frame.clone(), tb_secs);
        // Each loop pass restarts pts at 0; offset it so frame pts stays
        // monotonic in lockstep with the continuous (audio/wall) clock.
        rgba.pts_ms += pts_offset;
        shared.frames.lock().unwrap().push_back(rgba);
    }
}

fn run(
    path: &Path,
    shared: &Shared,
    audio_ring: Option<Arc<AudioRing>>,
) -> Result<(), ffmpeg::Error> {
    let mut ictx = ffmpeg::format::input(&path)?;
    let duration_ms = ictx.duration() / 1000;
    shared.duration_ms.store(duration_ms, Ordering::Relaxed);

    // Video stream + decoder.
    let (v_index, v_tb_secs, mut v_decoder) = {
        let stream = ictx
            .streams()
            .best(Type::Video)
            .ok_or(ffmpeg::Error::StreamNotFound)?;
        let index = stream.index();
        let tb = stream.time_base();
        let tb_secs = tb.numerator() as f64 / tb.denominator() as f64;
        let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
        (index, tb_secs, ctx.decoder().video()?)
    };
    let mut v_scaler: Option<Scaler> = None;

    // Optional audio stream + resampler.
    let mut audio = audio_ring.as_ref().and_then(|ring| open_audio(&ictx, ring));
    shared.has_audio.store(audio.is_some(), Ordering::Release);
    shared.opened.store(true, Ordering::Release);

    // Accumulated pts offset for the current loop pass (see `drain_video`).
    let mut loop_offset: i64 = 0;
    let mut cur_seek_gen = shared.seek_gen.load(Ordering::Acquire);
    loop {
        if shared.quit.load(Ordering::Acquire) {
            return Ok(());
        }

        // Handle a requested seek: jump the container, flush decoders, and drop
        // buffered output so playback resumes at the target. `loop_offset` is set
        // so post-seek frame pts line up with the rebased clock (target = N*dur+T).
        let sg = shared.seek_gen.load(Ordering::Acquire);
        if sg != cur_seek_gen {
            cur_seek_gen = sg;
            if duration_ms > 0 {
                let target = shared.seek_target_ms.load(Ordering::Acquire).max(0);
                let file_t = target % duration_ms;
                loop_offset = target - file_t;
                let _ = ictx.seek(file_t * 1000, ..);
                v_decoder.flush();
                if let Some(a) = &mut audio {
                    a.decoder.flush();
                }
                shared.frames.lock().unwrap().clear();
                if let Some(ring) = &audio_ring {
                    ring.clear();
                }
            }
            continue;
        }

        // Backpressure: only read more once at least one buffer has room.
        let video_full = shared.frames.lock().unwrap().len() >= VIDEO_QUEUE_CAP;
        let audio_full = match (&audio, &audio_ring) {
            (Some(_), Some(ring)) => ring.is_full(),
            _ => true,
        };
        if video_full && audio_full {
            thread::sleep(Duration::from_millis(3));
            continue;
        }

        let mut packet = ffmpeg::Packet::empty();
        match packet.read(&mut ictx) {
            Ok(()) => {
                let stream = packet.stream();
                if stream == v_index {
                    let _ = v_decoder.send_packet(&packet);
                    drain_video(
                        &mut v_decoder,
                        &mut v_scaler,
                        v_tb_secs,
                        loop_offset,
                        shared,
                    );
                } else if let (Some(a), Some(ring)) = (&mut audio, &audio_ring) {
                    if stream == a.index {
                        let _ = a.decoder.send_packet(&packet);
                        drain_audio(a, ring);
                    }
                }
            }
            Err(_) => {
                // End of this pass: flush each decoder's remaining frames.
                let _ = v_decoder.send_eof();
                drain_video(
                    &mut v_decoder,
                    &mut v_scaler,
                    v_tb_secs,
                    loop_offset,
                    shared,
                );
                if let (Some(a), Some(ring)) = (&mut audio, &audio_ring) {
                    let _ = a.decoder.send_eof();
                    drain_audio(a, ring);
                }

                if duration_ms > 0 {
                    // Loop: rewind to the start and keep decoding. Audio sample
                    // count and (offset) video pts both stay monotonic, so the
                    // clock keeps advancing seamlessly across the boundary.
                    let _ = ictx.seek(0, ..);
                    v_decoder.flush();
                    if let Some(a) = &mut audio {
                        a.decoder.flush();
                    }
                    loop_offset += duration_ms;
                } else {
                    // Unknown duration: can't loop cleanly, hold and idle.
                    shared.eof.store(true, Ordering::Release);
                    while !shared.quit.load(Ordering::Acquire) {
                        thread::sleep(Duration::from_millis(20));
                    }
                    return Ok(());
                }
            }
        }
    }
}

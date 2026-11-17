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
const VIDEO_QUEUE_BYTES: usize = 64 * 1024 * 1024;

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
}

struct DecodeControl<'a> {
    shared: &'a Shared,
    stop: &'a AtomicBool,
    seek_gen: u64,
}

impl DecodeControl<'_> {
    fn stopped(&self) -> bool {
        self.shared.quit.load(Ordering::Acquire) || self.stop.load(Ordering::Acquire)
    }

    fn interrupted(&self) -> bool {
        self.stopped() || self.shared.seek_gen.load(Ordering::Acquire) != self.seek_gen
    }
}

struct AudioWorker {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Drop for AudioWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.handle.take() {
            if handle.join().is_err() {
                tracing::warn!("audio decoder thread panicked");
            }
        }
    }
}

fn open_audio(
    ictx: &ffmpeg::format::context::Input,
    ring: &AudioRing,
) -> Result<Option<AudioStream>, ffmpeg::Error> {
    let Some(stream) = ictx.streams().best(Type::Audio) else {
        return Ok(None);
    };
    let index = stream.index();
    let ctx = ffmpeg::codec::context::Context::from_parameters(stream.parameters())?;
    let decoder = ctx.decoder().audio()?;

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
    )?;

    Ok(Some(AudioStream {
        index,
        decoder,
        resampler,
    }))
}

fn resample_audio_frame(
    resampler: &mut Resampler,
    frame: &mut AudioFrame,
) -> Result<AudioFrame, ffmpeg::Error> {
    if frame.channel_layout().is_empty() {
        frame.set_channel_layout(resampler.input().channel_layout);
    }
    let output = *resampler.output();
    let cap =
        (frame.samples() as i64 * output.rate as i64 / frame.rate().max(1) as i64) as usize + 1024;
    let mut out = AudioFrame::new(output.format, cap, output.channel_layout);
    resampler.run(frame, &mut out)?;
    Ok(out)
}

fn drain_audio(
    audio: &mut AudioStream,
    ring: &AudioRing,
    control: &DecodeControl<'_>,
) -> Result<(), ffmpeg::Error> {
    let mut frame = AudioFrame::empty();
    loop {
        match audio.decoder.receive_frame(&mut frame) {
            Ok(()) => {}
            Err(ffmpeg::Error::Eof)
            | Err(ffmpeg::Error::Other {
                errno: ffmpeg::error::EAGAIN,
            }) => return Ok(()),
            Err(error) => return Err(error),
        }
        if control.interrupted() {
            return Ok(());
        }
        let out = resample_audio_frame(&mut audio.resampler, &mut frame)?;
        let produced = out.samples() * ring.channels as usize;
        let bytes = out.data(0);
        let floats = bytes.len() / 4;
        let raw: &[f32] = bytemuck::cast_slice(&bytes[..floats * 4]);
        let mut samples = &raw[..produced.min(raw.len())];
        while !samples.is_empty() {
            if control.interrupted() {
                return Ok(());
            }
            let written = ring.push(samples);
            samples = &samples[written..];
            if written == 0 {
                thread::sleep(Duration::from_millis(3));
            }
        }
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
    control: &DecodeControl<'_>,
) {
    let mut frame = VideoFrame::empty();
    while decoder.receive_frame(&mut frame).is_ok() {
        let bytes = frame.width() as usize * frame.height() as usize * 4;
        loop {
            if control.interrupted() {
                return;
            }
            let frames = control.shared.frames.lock().unwrap();
            let queued_bytes: usize = frames.iter().map(|frame| frame.data.len()).sum();
            if frames.len() < VIDEO_QUEUE_CAP
                && (frames.is_empty() || queued_bytes.saturating_add(bytes) <= VIDEO_QUEUE_BYTES)
            {
                break;
            }
            drop(frames);
            thread::sleep(Duration::from_millis(3));
        }
        let mut rgba = make_rgba(scaler, frame.clone(), tb_secs);
        // Each loop pass restarts pts at 0; offset it so frame pts stays
        // monotonic in lockstep with the continuous (audio/wall) clock.
        rgba.pts_ms += pts_offset;
        if control.interrupted() {
            return;
        }
        control.shared.frames.lock().unwrap().push_back(rgba);
    }
}

fn run(
    path: &Path,
    shared: &Arc<Shared>,
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

    let stop = Arc::new(AtomicBool::new(false));
    let mut audio_worker = AudioWorker {
        stop: Arc::clone(&stop),
        handle: None,
    };
    if let Some(ring) = audio_ring {
        let audio = ffmpeg::format::input(&path).and_then(|input| {
            open_audio(&input, &ring).map(|audio| audio.map(|audio| (input, audio)))
        });
        match audio {
            Ok(Some((input, audio))) => {
                shared.has_audio.store(true, Ordering::Release);
                let shared = Arc::clone(shared);
                let stop = Arc::clone(&stop);
                audio_worker.handle = Some(
                    thread::Builder::new()
                        .name("ribb-audio".into())
                        .spawn(move || {
                            if let Err(error) =
                                run_audio(input, audio, &ring, &shared, &stop, duration_ms)
                            {
                                tracing::warn!(
                                    "audio decode or resampling failed, using wall clock: {error}"
                                );
                                ring.clear();
                                shared.has_audio.store(false, Ordering::Release);
                            }
                        })
                        .expect("spawn audio decoder thread"),
                );
            }
            Ok(None) => tracing::debug!("input has no audio stream"),
            Err(error) => {
                tracing::warn!("audio stream could not be opened, using wall clock: {error}");
            }
        }
    }
    shared.opened.store(true, Ordering::Release);

    // Accumulated pts offset for the current loop pass (see `drain_video`).
    let mut loop_offset: i64 = 0;
    let mut control = DecodeControl {
        shared,
        stop: &stop,
        seek_gen: 0,
    };
    loop {
        if control.stopped() {
            return Ok(());
        }

        // Handle a requested seek: jump the container, flush decoders, and drop
        // buffered output so playback resumes at the target. `loop_offset` is set
        // so post-seek frame pts line up with the rebased clock (target = N*dur+T).
        let sg = shared.seek_gen.load(Ordering::Acquire);
        if sg != control.seek_gen {
            control.seek_gen = sg;
            if duration_ms > 0 {
                let target = shared.seek_target_ms.load(Ordering::Acquire).max(0);
                let file_t = target % duration_ms;
                loop_offset = target - file_t;
                let _ = ictx.seek(file_t * 1000, ..);
                v_decoder.flush();
                shared.frames.lock().unwrap().clear();
            }
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
                        &control,
                    );
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
                    &control,
                );
                if control.interrupted() {
                    continue;
                }

                if duration_ms > 0 {
                    // Loop: rewind to the start and keep decoding. Audio sample
                    // count and (offset) video pts both stay monotonic, so the
                    // clock keeps advancing seamlessly across the boundary.
                    let _ = ictx.seek(0, ..);
                    v_decoder.flush();
                    loop_offset += duration_ms;
                } else {
                    // Unknown duration: can't loop cleanly, hold and idle.
                    shared.eof.store(true, Ordering::Release);
                    while !control.interrupted() {
                        thread::sleep(Duration::from_millis(20));
                    }
                }
            }
        }
    }
}

fn run_audio(
    mut input: ffmpeg::format::context::Input,
    mut audio: AudioStream,
    ring: &AudioRing,
    shared: &Shared,
    stop: &AtomicBool,
    duration_ms: i64,
) -> Result<(), ffmpeg::Error> {
    let mut control = DecodeControl {
        shared,
        stop,
        seek_gen: 0,
    };
    loop {
        if control.stopped() {
            return Ok(());
        }
        let seek_gen = shared.seek_gen.load(Ordering::Acquire);
        if seek_gen != control.seek_gen {
            control.seek_gen = seek_gen;
            if duration_ms > 0 {
                let target = shared.seek_target_ms.load(Ordering::Acquire).max(0);
                input.seek((target % duration_ms) * 1000, ..)?;
                audio.decoder.flush();
                let source = *audio.resampler.input();
                let output = *audio.resampler.output();
                audio.resampler = Resampler::get(
                    source.format,
                    source.channel_layout,
                    source.rate,
                    output.format,
                    output.channel_layout,
                    output.rate,
                )?;
                ring.clear();
            }
            continue;
        }

        let mut packet = ffmpeg::Packet::empty();
        match packet.read(&mut input) {
            Ok(()) => {
                if packet.stream() == audio.index {
                    audio.decoder.send_packet(&packet)?;
                    drain_audio(&mut audio, ring, &control)?;
                }
            }
            Err(ffmpeg::Error::Eof) => {
                audio.decoder.send_eof()?;
                drain_audio(&mut audio, ring, &control)?;
                if control.interrupted() {
                    continue;
                }
                if duration_ms > 0 {
                    input.seek(0, ..)?;
                    audio.decoder.flush();
                } else {
                    while !control.interrupted() {
                        thread::sleep(Duration::from_millis(20));
                    }
                }
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{Delay, Frame, Rgba, RgbaImage};
    use std::time::Instant;

    fn stereo_resampler() -> Resampler {
        Resampler::get(
            Sample::F32(SampleType::Packed),
            ChannelLayout::STEREO,
            48_000,
            Sample::F32(SampleType::Packed),
            ChannelLayout::STEREO,
            48_000,
        )
        .unwrap()
    }

    #[test]
    fn audio_without_a_channel_layout_produces_samples() {
        let mut resampler = stereo_resampler();
        let mut frame = AudioFrame::new(Sample::F32(SampleType::Packed), 32, ChannelLayout::STEREO);
        frame.set_rate(48_000);
        let samples: &mut [f32] = bytemuck::cast_slice_mut(frame.data_mut(0));
        samples.fill(0.25);
        let mut layout = ChannelLayout::STEREO;
        layout.0.order = ffmpeg::ffi::AVChannelOrder::AV_CHANNEL_ORDER_UNSPEC;
        layout.0.u.mask = 0;
        frame.set_channel_layout(layout);

        let out = resample_audio_frame(&mut resampler, &mut frame)
            .expect("unspecified decoded layout should use the stream layout");
        assert_eq!(out.samples(), 32);
        let samples: &[f32] = bytemuck::cast_slice(out.data(0));
        assert_eq!(&samples[..64], &[0.25; 64]);
    }

    #[test]
    fn incompatible_audio_layout_returns_a_conversion_error() {
        let mut resampler = stereo_resampler();
        let mut frame = AudioFrame::new(Sample::F32(SampleType::Packed), 32, ChannelLayout::MONO);
        frame.set_rate(48_000);
        assert!(matches!(
            resample_audio_frame(&mut resampler, &mut frame),
            Err(ffmpeg::Error::InputChanged)
        ));
    }

    struct GifDecoder {
        shared: Arc<Shared>,
        _file: tempfile::NamedTempFile,
    }

    impl GifDecoder {
        fn new() -> Self {
            Self::with_size(4, 4)
        }

        fn with_size(width: u32, height: u32) -> Self {
            // No extension, just like Player's downloaded temp files. Distinct
            // delays and transparent backgrounds exercise GIF timing/disposal.
            let mut file = tempfile::NamedTempFile::new().unwrap();
            {
                let mut encoder = GifEncoder::new(&mut file);
                encoder.set_repeat(Repeat::Infinite).unwrap();
                for (i, (color, delay)) in [
                    ([255, 0, 0, 255], 80),
                    ([0, 255, 0, 255], 160),
                    ([0, 0, 255, 255], 240),
                ]
                .into_iter()
                .enumerate()
                {
                    let mut image = RgbaImage::new(width, height);
                    image.put_pixel(i as u32 + 1, 0, Rgba(color));
                    encoder
                        .encode_frame(Frame::from_parts(
                            image,
                            0,
                            0,
                            Delay::from_numer_denom_ms(delay, 1),
                        ))
                        .unwrap();
                }
            }
            let shared = spawn(file.path().to_path_buf(), None);
            Self {
                shared,
                _file: file,
            }
        }

        fn wait_for(&self, ready: impl Fn(&Shared) -> bool) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while !ready(&self.shared) {
                assert!(
                    Instant::now() < deadline,
                    "GIF decoder did not produce frames"
                );
                thread::sleep(Duration::from_millis(3));
            }
        }
    }

    impl Drop for GifDecoder {
        fn drop(&mut self) {
            self.shared.quit.store(true, Ordering::Release);
        }
    }

    #[test]
    fn gif_preserves_frame_timing_transparency_and_loops() {
        let decoder = GifDecoder::new();
        decoder.wait_for(|s| s.frames.lock().unwrap().len() >= 6);
        assert_eq!(decoder.shared.duration_ms.load(Ordering::Acquire), 480);
        assert!(!decoder.shared.has_audio.load(Ordering::Acquire));

        let frames = decoder.shared.frames.lock().unwrap();
        let pts: Vec<_> = frames.iter().take(6).map(|f| f.pts_ms).collect();
        assert_eq!(pts, [0, 80, 240, 480, 560, 720]);
        for (i, frame) in frames.iter().take(6).enumerate() {
            assert_eq!((frame.width, frame.height), (4, 4));
            assert_eq!(frame.data[3], 0, "transparent background");
            let color = match i % 3 {
                0 => [255, 0, 0, 255],
                1 => [0, 255, 0, 255],
                _ => [0, 0, 255, 255],
            };
            let offset = (i % 3 + 1) * 4;
            assert_eq!(frame.data[offset..offset + 4], color, "frame {i}");
            for x in 0..4 {
                if x * 4 != offset {
                    assert_eq!(frame.data[x * 4 + 3], 0, "disposed pixel {x}, frame {i}");
                }
            }
        }
    }

    #[test]
    fn gif_seek_rebases_frames_within_the_current_loop() {
        let decoder = GifDecoder::new();
        decoder.wait_for(|s| s.frames.lock().unwrap().len() >= VIDEO_QUEUE_CAP);
        // Third frame of the second loop: 480 + 240 ms.
        decoder.shared.seek_target_ms.store(720, Ordering::Release);
        decoder.shared.seek_gen.fetch_add(1, Ordering::Release);
        decoder.wait_for(|s| {
            let frames = s.frames.lock().unwrap();
            // Seeking may decode from an earlier keyframe to reconstruct the
            // GIF canvas. Player::tick presents the last frame at the target.
            frames.len() >= 4
                && frames
                    .front()
                    .is_some_and(|f| (480..=720).contains(&f.pts_ms))
        });
        let frames = decoder.shared.frames.lock().unwrap();
        let target = frames.iter().position(|f| f.pts_ms == 720).unwrap();
        assert_eq!(frames[target].data[12..16], [0, 0, 255, 255]);
        assert_eq!(frames[target + 1].pts_ms, 960);
        assert_eq!(frames[target + 1].data[4..8], [255, 0, 0, 255]);
    }

    #[test]
    fn undisplayed_video_stays_bounded_and_stops_while_full() {
        let decoder = GifDecoder::new();
        decoder.wait_for(|s| s.frames.lock().unwrap().len() == VIDEO_QUEUE_CAP);
        thread::sleep(Duration::from_millis(30));
        assert_eq!(decoder.shared.frames.lock().unwrap().len(), VIDEO_QUEUE_CAP);
        decoder.shared.quit.store(true, Ordering::Release);
        decoder.wait_for(|s| s.eof.load(Ordering::Acquire));
    }

    #[test]
    fn large_video_frames_respect_the_queue_byte_budget() {
        let decoder = GifDecoder::with_size(2048, 2048);
        decoder.wait_for(|s| {
            s.frames
                .lock()
                .unwrap()
                .iter()
                .map(|frame| frame.data.len())
                .sum::<usize>()
                >= VIDEO_QUEUE_BYTES
        });
        thread::sleep(Duration::from_millis(30));
        let frames = decoder.shared.frames.lock().unwrap();
        assert_eq!(frames.len(), 4);
        assert_eq!(
            frames.iter().map(|frame| frame.data.len()).sum::<usize>(),
            64 * 1024 * 1024
        );
        drop(frames);
        decoder.shared.seek_target_ms.store(720, Ordering::Release);
        decoder.shared.seek_gen.fetch_add(1, Ordering::Release);
        decoder.wait_for(|s| {
            s.frames
                .lock()
                .unwrap()
                .front()
                .is_some_and(|frame| frame.pts_ms >= 480)
        });
        decoder.shared.quit.store(true, Ordering::Release);
        decoder.wait_for(|s| s.eof.load(Ordering::Acquire));
    }
}

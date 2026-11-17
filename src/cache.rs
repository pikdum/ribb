//! In-memory image cache.
//!
//! iced re-runs `view()` constantly, so thumbnails and full images must be
//! fetched once and cached — never refetched per frame. Crucially, images are
//! **decoded and downscaled in worker tasks** (off the render thread) and
//! handed to iced as ready RGBA via [`Handle::from_rgba`]; using
//! `Handle::from_bytes` instead would defer decoding to the single render
//! thread, stalling the UI while ~100 thumbnails decode one at a time.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use iced::widget::image::Handle;
use tokio::sync::Semaphore;

/// A decoded, ready-to-upload RGBA image.
#[derive(Clone)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl std::fmt::Debug for DecodedImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DecodedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.rgba.len())
            .finish()
    }
}

/// Load state of a single image URL.
#[derive(Debug, Clone)]
pub enum ImageState {
    Loading,
    Loaded(Handle),
    Failed,
}

/// Which resolution of an image is wanted. The same URL can be both a grid
/// thumbnail and the expanded full image (e.g. a small Danbooru post whose
/// original file is also its preview), so they're cached separately — otherwise
/// the low-res thumbnail would be reused, upscaled, for the full view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageKind {
    Thumbnail,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageLoadId(u64);

#[derive(Debug)]
struct ImageEntry {
    state: ImageState,
    load_id: ImageLoadId,
    abort: Option<iced::task::Handle>,
}

/// URL-keyed cache of fetched images, split by [`ImageKind`].
#[derive(Debug, Default)]
pub struct ImageCache {
    thumbnails: HashMap<String, ImageEntry>,
    full: HashMap<String, ImageEntry>,
    next_load_id: u64,
}

impl ImageCache {
    fn map(&self, kind: ImageKind) -> &HashMap<String, ImageEntry> {
        match kind {
            ImageKind::Thumbnail => &self.thumbnails,
            ImageKind::Full => &self.full,
        }
    }

    fn map_mut(&mut self, kind: ImageKind) -> &mut HashMap<String, ImageEntry> {
        match kind {
            ImageKind::Thumbnail => &mut self.thumbnails,
            ImageKind::Full => &mut self.full,
        }
    }

    pub fn get(&self, url: &str, kind: ImageKind) -> Option<&ImageState> {
        self.map(kind).get(url).map(|entry| &entry.state)
    }

    /// Mark a URL as in-flight, returning a fresh identity for a new fetch.
    pub fn begin_load(&mut self, url: &str, kind: ImageKind) -> Option<ImageLoadId> {
        if self.map(kind).contains_key(url) {
            return None;
        }
        let load_id = ImageLoadId(self.next_load_id);
        self.next_load_id = self
            .next_load_id
            .checked_add(1)
            .expect("image load ID exhausted");
        self.map_mut(kind).insert(
            url.to_string(),
            ImageEntry {
                state: ImageState::Loading,
                load_id,
                abort: None,
            },
        );
        Some(load_id)
    }

    pub fn track_load(
        &mut self,
        url: &str,
        kind: ImageKind,
        load_id: ImageLoadId,
        abort: iced::task::Handle,
    ) {
        let abort = abort.abort_on_drop();
        if let Some(entry) = self.map_mut(kind).get_mut(url) {
            if entry.load_id == load_id && matches!(entry.state, ImageState::Loading) {
                entry.abort = Some(abort);
            }
        }
    }

    /// Drop images and pending requests no open tab owns.
    pub fn retain(&mut self, mut keep: impl FnMut(&str, ImageKind) -> bool) {
        self.thumbnails
            .retain(|url, _| keep(url, ImageKind::Thumbnail));
        self.full.retain(|url, _| keep(url, ImageKind::Full));
    }

    /// Record the result of a fetch+decode. `None` marks failure.
    pub fn finish_load(
        &mut self,
        url: &str,
        kind: ImageKind,
        load_id: ImageLoadId,
        decoded: Option<DecodedImage>,
    ) {
        let Some(entry) = self.map_mut(kind).get_mut(url) else {
            return;
        };
        if entry.load_id != load_id || !matches!(entry.state, ImageState::Loading) {
            return;
        }
        entry.state = match decoded {
            Some(img) => ImageState::Loaded(Handle::from_rgba(img.width, img.height, img.rgba)),
            None => ImageState::Failed,
        };
        entry.abort = None;
    }
}

/// Fetch `url`, then decode and (optionally) downscale it on a blocking worker
/// so neither the network wait nor the CPU decode touches the render thread.
/// `max_dim` bounds the longer edge (preserving aspect); `None` keeps full size.
/// `sem` caps how many fetch/decode jobs run at once.
pub async fn fetch_image(
    client: reqwest::Client,
    sem: Arc<Semaphore>,
    url: String,
    max_dim: Option<u32>,
    high_quality: bool,
) -> (String, Option<DecodedImage>) {
    let Ok(permit) = sem.acquire_owned().await else {
        return (url, None);
    };

    let t0 = Instant::now();
    let bytes = match async {
        client
            .get(&url)
            // Set Referer to the image's own URL to defeat hotlink protection
            // (e.g. Gelbooru's CDN serves a non-image page otherwise) — mirrors
            // ebb's onBeforeSendHeaders.
            .header(reqwest::header::REFERER, &url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await
    }
    .await
    {
        Ok(bytes) => bytes,
        Err(e) => {
            tracing::warn!("image fetch failed for {url}: {e}");
            return (url, None);
        }
    };
    let fetch_ms = t0.elapsed().as_millis();

    let t1 = Instant::now();
    let decoded = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        decode_and_resize(&bytes, max_dim, high_quality)
    })
    .await
    .unwrap_or(None);
    let decode_ms = t1.elapsed().as_millis();

    match &decoded {
        Some(img) => {
            tracing::debug!(
                fetch_ms,
                decode_ms,
                w = img.width,
                h = img.height,
                "image ready: {url}"
            )
        }
        None => tracing::warn!(fetch_ms, "image decode failed: {url}"),
    }
    (url, decoded)
}

/// Fetch raw bytes for `url` (no decoding) — used for SWF movies. Returns the
/// URL alongside the bytes so the caller can route the result.
pub async fn fetch_bytes(client: reqwest::Client, url: String) -> (String, Option<Vec<u8>>) {
    let result = async {
        client
            .get(&url)
            .header(reqwest::header::REFERER, &url)
            .send()
            .await?
            .error_for_status()?
            .bytes()
            .await
    }
    .await;
    match result {
        Ok(bytes) => (url, Some(bytes.to_vec())),
        Err(e) => {
            tracing::warn!("byte fetch failed for {url}: {e}");
            (url, None)
        }
    }
}

/// Decode encoded image bytes to RGBA, downscaling to fit `max_dim` if given.
/// `high_quality` selects a Lanczos3 filter (full images) over the fast
/// `thumbnail` box filter (grid thumbnails).
fn decode_and_resize(
    bytes: &[u8],
    max_dim: Option<u32>,
    high_quality: bool,
) -> Option<DecodedImage> {
    let img = image::load_from_memory(bytes).ok()?;
    let img = match max_dim {
        Some(m) if img.width() > m || img.height() > m => {
            if high_quality {
                img.resize(m, m, image::imageops::FilterType::Lanczos3)
            } else {
                img.thumbnail(m, m)
            }
        }
        _ => img,
    };
    let rgba = img.into_rgba8();
    Some(DecodedImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decoded() -> DecodedImage {
        DecodedImage {
            width: 1,
            height: 1,
            rgba: vec![255; 4],
        }
    }

    #[test]
    fn stale_completion_cannot_recreate_evicted_image() {
        let mut cache = ImageCache::default();
        let load = cache.begin_load("image", ImageKind::Full).unwrap();
        cache.retain(|_, _| false);
        cache.finish_load("image", ImageKind::Full, load, Some(decoded()));
        assert!(cache.get("image", ImageKind::Full).is_none());
    }

    #[test]
    fn stale_completion_cannot_replace_new_request_for_same_url() {
        let mut cache = ImageCache::default();
        let old = cache.begin_load("image", ImageKind::Full).unwrap();
        cache.retain(|_, _| false);
        let new = cache.begin_load("image", ImageKind::Full).unwrap();
        assert_ne!(old, new);

        cache.finish_load("image", ImageKind::Full, old, None);
        assert!(matches!(
            cache.get("image", ImageKind::Full),
            Some(ImageState::Loading)
        ));
        cache.finish_load("image", ImageKind::Full, new, Some(decoded()));
        assert!(matches!(
            cache.get("image", ImageKind::Full),
            Some(ImageState::Loaded(_))
        ));
        cache.finish_load("image", ImageKind::Full, old, None);
        assert!(matches!(
            cache.get("image", ImageKind::Full),
            Some(ImageState::Loaded(_))
        ));
    }

    #[test]
    fn thumbnail_and_full_requests_stay_distinct_for_same_url() {
        let mut cache = ImageCache::default();
        let thumb = cache.begin_load("image", ImageKind::Thumbnail).unwrap();
        let full = cache.begin_load("image", ImageKind::Full).unwrap();
        cache.finish_load("image", ImageKind::Thumbnail, full, None);
        assert!(matches!(
            cache.get("image", ImageKind::Thumbnail),
            Some(ImageState::Loading)
        ));
        cache.finish_load("image", ImageKind::Thumbnail, thumb, Some(decoded()));
        cache.finish_load("image", ImageKind::Full, full, None);
        assert!(matches!(
            cache.get("image", ImageKind::Thumbnail),
            Some(ImageState::Loaded(_))
        ));
        assert!(matches!(
            cache.get("image", ImageKind::Full),
            Some(ImageState::Failed)
        ));

        cache.retain(|_, kind| kind == ImageKind::Thumbnail);
        assert!(matches!(
            cache.get("image", ImageKind::Thumbnail),
            Some(ImageState::Loaded(_))
        ));
        assert!(cache.get("image", ImageKind::Full).is_none());
    }

    #[test]
    fn pruning_aborts_only_unowned_requests() {
        let mut cache = ImageCache::default();
        let kept = cache.begin_load("kept", ImageKind::Thumbnail).unwrap();
        let dropped = cache.begin_load("dropped", ImageKind::Thumbnail).unwrap();
        let (_kept_task, kept_abort) = iced::Task::done(()).abortable();
        let (_dropped_task, dropped_abort) = iced::Task::done(()).abortable();
        cache.track_load("kept", ImageKind::Thumbnail, kept, kept_abort.clone());
        cache.track_load(
            "dropped",
            ImageKind::Thumbnail,
            dropped,
            dropped_abort.clone(),
        );

        cache.retain(|url, _| url == "kept");
        assert!(!kept_abort.is_aborted());
        assert!(dropped_abort.is_aborted());
    }
}

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

/// URL-keyed cache of fetched images, split by [`ImageKind`].
#[derive(Debug, Default)]
pub struct ImageCache {
    thumbnails: HashMap<String, ImageState>,
    full: HashMap<String, ImageState>,
}

impl ImageCache {
    fn map(&self, kind: ImageKind) -> &HashMap<String, ImageState> {
        match kind {
            ImageKind::Thumbnail => &self.thumbnails,
            ImageKind::Full => &self.full,
        }
    }

    fn map_mut(&mut self, kind: ImageKind) -> &mut HashMap<String, ImageState> {
        match kind {
            ImageKind::Thumbnail => &mut self.thumbnails,
            ImageKind::Full => &mut self.full,
        }
    }

    pub fn get(&self, url: &str, kind: ImageKind) -> Option<&ImageState> {
        self.map(kind).get(url)
    }

    /// Mark a URL as in-flight for `kind`. Returns `true` if newly inserted
    /// (the caller should kick off a fetch), `false` if already known.
    pub fn begin_load(&mut self, url: &str, kind: ImageKind) -> bool {
        let map = self.map_mut(kind);
        if map.contains_key(url) {
            return false;
        }
        map.insert(url.to_string(), ImageState::Loading);
        true
    }

    /// Record the result of a fetch+decode. `None` marks failure.
    pub fn finish_load(&mut self, url: String, kind: ImageKind, decoded: Option<DecodedImage>) {
        let state = match decoded {
            Some(img) => ImageState::Loaded(Handle::from_rgba(img.width, img.height, img.rgba)),
            None => ImageState::Failed,
        };
        self.map_mut(kind).insert(url, state);
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
    let _permit = sem.acquire().await;

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
    let decoded =
        tokio::task::spawn_blocking(move || decode_and_resize(&bytes, max_dim, high_quality))
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
    let rgba = img.to_rgba8();
    Some(DecodedImage {
        width: rgba.width(),
        height: rgba.height(),
        rgba: rgba.into_raw(),
    })
}

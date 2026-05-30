//! In-memory image cache.
//!
//! iced re-runs `view()` constantly, so thumbnails and full images must be
//! fetched once and cached — never refetched per frame. Entries are keyed by
//! URL and shared across all tabs.

use std::collections::HashMap;

use iced::widget::image::Handle;

/// Load state of a single image URL.
#[derive(Debug, Clone)]
pub enum ImageState {
    Loading,
    Loaded(Handle),
    Failed,
}

/// URL-keyed cache of fetched images.
#[derive(Debug, Default)]
pub struct ImageCache {
    entries: HashMap<String, ImageState>,
}

impl ImageCache {
    pub fn get(&self, url: &str) -> Option<&ImageState> {
        self.entries.get(url)
    }

    pub fn contains(&self, url: &str) -> bool {
        self.entries.contains_key(url)
    }

    /// Mark a URL as in-flight. Returns `true` if it was newly inserted (i.e.
    /// the caller should kick off a fetch), `false` if already known.
    pub fn begin_load(&mut self, url: &str) -> bool {
        if self.entries.contains_key(url) {
            return false;
        }
        self.entries.insert(url.to_string(), ImageState::Loading);
        true
    }

    /// Record the result of a fetch. `None` bytes (or undecodable) marks failure.
    pub fn finish_load(&mut self, url: String, bytes: Option<Vec<u8>>) {
        let state = match bytes {
            Some(bytes) => ImageState::Loaded(Handle::from_bytes(bytes)),
            None => ImageState::Failed,
        };
        self.entries.insert(url, state);
    }
}

/// Fetch raw image bytes for `url`. Returns the URL alongside the bytes so the
/// caller can route the result back into the cache. Errors become `None`.
pub async fn fetch_image(client: reqwest::Client, url: String) -> (String, Option<Vec<u8>>) {
    let result = async {
        client
            .get(&url)
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
            tracing::warn!("image fetch failed for {url}: {e}");
            (url, None)
        }
    }
}

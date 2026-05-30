//! The iced application — a port of ebb's React component tree.
//!
//! ebb's two React Contexts become flat state here: app-wide state on [`Ribb`]
//! (tabs, settings, shared image cache) and per-tab search state on [`Tab`].
//! `useEffect`-driven fetches become explicit [`Task`]s returned from `update`.

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;

use iced::advanced::widget::{operation, Id};
use iced::widget::{
    button, column, container, image, mouse_area, pick_list, responsive, row, scrollable, stack,
    text, text_input, Column, Row, Space,
};
use iced::{
    Border, Center, Color, ContentFit, Element, Length, Rectangle, Size, Subscription, Task, Theme,
    Vector,
};
use iced_ruffle::{Ruffle, RufflePlayer};

use crate::booru::{
    is_image, is_swf, is_video, BooruClient, BooruPost, BooruTag, PostQuery, PostsPage, Rating,
    Site, TagGroup,
};
use crate::cache::{fetch_bytes, fetch_image, DecodedImage, ImageCache, ImageKind, ImageState};
use crate::settings::Settings;
use crate::style;

const PAGE_LIMIT: u32 = 100;
const MAX_FETCH_ATTEMPTS: u32 = 3;
const GRID_GAP: f32 = 8.0;
/// Longest edge (px) thumbnails are downscaled to — small textures, fast decode.
const THUMB_MAX: u32 = 512;
/// Absolute ceiling on full-image texture size (memory bound).
const FULL_MAX_CEIL: u32 = 4096;
/// Decode full images at this multiple of their display size, then let the GPU
/// downscale — effectively supersampling, which keeps fine detail crisp on both
/// 1x and HiDPI displays (iced's GPU sampler has no mipmaps).
const FULL_SUPERSAMPLE: f32 = 2.0;
/// Max concurrent image fetch+decode jobs.
const IMAGE_CONCURRENCY: usize = 12;

/// Preview URLs that providers return as placeholders — never worth showing.
const PREVIEW_BLACKLIST: [&str; 2] = [
    "https://cdn.donmai.us/images/flash-preview.png",
    "https://static1.e621.net/images/download-preview.png",
];

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct Ribb {
    client: BooruClient,
    settings: Settings,
    settings_open: bool,
    settings_draft: String,
    tabs: Vec<Tab>,
    active: usize,
    next_tab_id: u64,
    images: ImageCache,
    /// Caps concurrent image fetch/decode jobs (shared by all fetch Tasks).
    image_sem: Arc<tokio::sync::Semaphore>,
    /// Id of the content scrollable, so we can scroll to an expanded post.
    scroll_id: Id,
    /// Id attached to the image of the post we want to center; an operation
    /// reads its laid-out bounds to compute the exact scroll offset.
    anchor_id: Id,
    /// Post id currently carrying `anchor_id` (the one to scroll to).
    scroll_anchor: Option<String>,
    /// Tag currently hovered in a detail view (its "open in new tab" + shows).
    hovered_tag: Option<String>,
    /// Exact content-area (scroll viewport) size, measured by the view's
    /// `responsive` wrapper and read back here for precise scroll math.
    viewport: Cell<Size>,
    window: Size,
}

/// One search session — ebb's per-tab `MainContext`.
struct Tab {
    id: u64,
    title: String,
    site: Site,
    rating: Option<Rating>,
    temp_query: String,
    /// `None` until the first search is submitted (ebb's `query === undefined`).
    query: Option<String>,
    page: u32,
    posts: Vec<BooruPost>,
    has_next_page: bool,
    /// Post IDs currently expanded to their detail view.
    selected: Vec<String>,
    loading: bool,
    error: Option<String>,
    /// Live tag suggestions for the last word being typed.
    autocomplete: Vec<BooruTag>,
    /// Bumped on every fresh fetch so stale responses can be discarded.
    generation: u64,
    /// Loaded Flash players, keyed by post ID.
    swf: HashMap<String, RufflePlayer>,
}

impl Tab {
    fn new(id: u64, query: Option<String>) -> Self {
        let title = match &query {
            Some(q) if !q.is_empty() => q.clone(),
            _ => "New Tab".to_string(),
        };
        Tab {
            id,
            title,
            site: Site::Danbooru,
            // ebb defaults the rating filter to the first rating (General).
            rating: Some(Rating::General),
            temp_query: query.clone().unwrap_or_default(),
            query,
            page: 0,
            posts: Vec::new(),
            has_next_page: false,
            selected: Vec::new(),
            loading: false,
            error: None,
            autocomplete: Vec::new(),
            generation: 0,
            swf: HashMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Message {
    // Tabs
    NewTab,
    CloseTab(u64),
    SelectTab(usize),
    SwitchTabLeft,
    SwitchTabRight,
    // Search / filters (act on the active tab)
    QueryChanged(String),
    SubmitSearch,
    NextPage,
    PrevPage,
    SiteSelected(Site),
    RatingSelected(RatingChoice),
    // Results
    PostsLoaded {
        tab: u64,
        generation: u64,
        attempt: u32,
        result: Result<PostsPage, String>,
    },
    TagGroupsLoaded {
        tab: u64,
        post_id: String,
        groups: Vec<TagGroup>,
    },
    AutocompleteLoaded {
        tab: u64,
        query_word: String,
        tags: Vec<BooruTag>,
    },
    AutocompleteSelected(String),
    TogglePost(String),
    TagClicked(String),
    TagHovered(Option<String>),
    OpenTagInNewTab(String),
    OpenExternal(String),
    // Images / SWF
    ImageLoaded(String, ImageKind, Option<DecodedImage>),
    SwfLoaded {
        tab: u64,
        post_id: String,
        bytes: Option<Vec<u8>>,
    },
    // Settings
    OpenSettings,
    CloseSettings,
    /// Absorbs a click (e.g. on the settings card) so it doesn't propagate.
    Noop,
    SettingsDraftChanged(String),
    SaveSettings,
    // Window / input
    WindowResized(Size),
    Key(iced::keyboard::Event),
    /// Computed scroll offset (content-space y) to center an expanded post.
    ScrollComputed(f32),
}

/// A rating choice in the selector, including the "All Content" (`None`) option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RatingChoice(pub Option<Rating>);

impl std::fmt::Display for RatingChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(r) => f.write_str(r.query_value()),
            None => f.write_str("All Content"),
        }
    }
}

// ---------------------------------------------------------------------------
// Application
// ---------------------------------------------------------------------------

impl Ribb {
    pub fn new() -> Self {
        let settings = Settings::load();
        let client = BooruClient::new()
            .expect("failed to build HTTP client")
            .with_gelbooru_credentials(settings.gelbooru_credentials.clone());
        Ribb {
            client,
            settings_draft: settings.gelbooru_credentials.clone(),
            settings,
            settings_open: false,
            tabs: vec![Tab::new(0, None)],
            active: 0,
            next_tab_id: 1,
            images: ImageCache::default(),
            image_sem: Arc::new(tokio::sync::Semaphore::new(IMAGE_CONCURRENCY)),
            scroll_id: Id::unique(),
            anchor_id: Id::unique(),
            scroll_anchor: None,
            hovered_tag: None,
            viewport: Cell::new(Size::new(1100.0, 700.0)),
            window: Size::new(1100.0, 800.0),
        }
    }

    /// Boot entry point: build state and, when `RIBB_DEBUG_QUERY` is set, kick
    /// off an initial search so the fetch/render pipeline can be exercised
    /// headlessly (logs show the result).
    pub fn boot() -> (Self, Task<Message>) {
        let mut app = Self::new();
        let task = match std::env::var("RIBB_DEBUG_QUERY") {
            Ok(q) => {
                tracing::info!("debug: auto-searching {q:?}");
                let tab = &mut app.tabs[0];
                tab.query = Some(q.clone());
                tab.temp_query = q.clone();
                tab.title = if q.is_empty() { "New Tab".into() } else { q };
                // Search unfiltered in debug mode so results aren't narrowed.
                tab.rating = None;
                app.fetch_tab(0)
            }
            Err(_) => Task::none(),
        };
        // Debug: simulate typing to exercise the autocomplete path.
        let task = match std::env::var("RIBB_DEBUG_TYPE") {
            Ok(typed) => Task::batch([task, Task::done(Message::QueryChanged(typed))]),
            Err(_) => task,
        };
        (app, task)
    }

    fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
    }

    pub fn theme(&self) -> Theme {
        Theme::Light
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let keys = iced::keyboard::listen().map(Message::Key);
        let resizes = iced::window::resize_events().map(|(_id, size)| Message::WindowResized(size));
        Subscription::batch([keys, resizes])
    }

    /// Translate a Ctrl-modified key press into a tab action (ebb's shortcuts).
    fn handle_key(&mut self, event: iced::keyboard::Event) -> Task<Message> {
        use iced::keyboard::key::{Key, Named};
        use iced::keyboard::Event;
        let Event::KeyPressed { key, modifiers, .. } = event else {
            return Task::none();
        };
        if !modifiers.control() {
            return Task::none();
        }
        let action = match key.as_ref() {
            Key::Character("t") => Message::NewTab,
            Key::Character("w") => Message::CloseTab(u64::MAX), // sentinel: active tab
            Key::Character("h") => Message::SwitchTabLeft,
            Key::Character("l") => Message::SwitchTabRight,
            Key::Named(Named::ArrowLeft) => Message::SwitchTabLeft,
            Key::Named(Named::ArrowRight) => Message::SwitchTabRight,
            _ => return Task::none(),
        };
        self.update(action)
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WindowResized(size) => {
                self.window = size;
                Task::none()
            }
            Message::Key(event) => self.handle_key(event),
            Message::Noop => Task::none(),
            Message::ScrollComputed(y) => iced::widget::operation::scroll_to(
                self.scroll_id.clone(),
                iced::widget::scrollable::AbsoluteOffset { x: 0.0, y },
            ),

            // --- Tabs ---------------------------------------------------
            Message::NewTab => self.push_tab(None, true),
            Message::SelectTab(idx) => {
                if idx < self.tabs.len() {
                    self.active = idx;
                }
                Task::none()
            }
            Message::CloseTab(id) => {
                let idx = if id == u64::MAX {
                    self.active
                } else {
                    self.tab_index(id).unwrap_or(self.active)
                };
                if self.tabs.len() > 1 {
                    self.tabs.remove(idx);
                    if self.active >= self.tabs.len() {
                        self.active = self.tabs.len() - 1;
                    } else if idx < self.active || (idx == self.active && idx > 0) {
                        self.active = self.active.saturating_sub(1);
                    }
                }
                Task::none()
            }
            Message::SwitchTabLeft => {
                if self.tabs.len() > 1 {
                    self.active = if self.active == 0 {
                        self.tabs.len() - 1
                    } else {
                        self.active - 1
                    };
                }
                Task::none()
            }
            Message::SwitchTabRight => {
                if self.tabs.len() > 1 {
                    self.active = (self.active + 1) % self.tabs.len();
                }
                Task::none()
            }

            // --- Search / filters --------------------------------------
            Message::QueryChanged(value) => {
                let tab = &mut self.tabs[self.active];
                tab.temp_query = value.clone();
                // Autocomplete the last word being typed (ebb completes the
                // word under the caret). A trailing space means "no word".
                let word = last_word(&value);
                if word.is_empty() {
                    tab.autocomplete.clear();
                    return Task::none();
                }
                let (client, site, tab_id) = (self.client.clone(), tab.site, tab.id);
                Task::perform(
                    async move { client.get_tags(site, &word).await },
                    move |res| Message::AutocompleteLoaded {
                        tab: tab_id,
                        query_word: last_word(&value),
                        tags: res.unwrap_or_default(),
                    },
                )
            }
            Message::AutocompleteLoaded {
                tab,
                query_word,
                tags,
            } => {
                if let Some(idx) = self.tab_index(tab) {
                    // Ignore stale responses for a word no longer being typed.
                    if last_word(&self.tabs[idx].temp_query) == query_word {
                        tracing::debug!(word = %query_word, count = tags.len(), "autocomplete");
                        self.tabs[idx].autocomplete = tags;
                    }
                }
                Task::none()
            }
            Message::AutocompleteSelected(value) => {
                let tab = &mut self.tabs[self.active];
                // Replace the last word with the selection (ebb's behavior).
                let last = last_word(&tab.temp_query);
                let mut combined = tab.temp_query.clone();
                combined.truncate(combined.len() - last.len());
                combined.push_str(&value);
                tab.temp_query = combined.clone();
                tab.query = Some(combined.clone());
                tab.title = if combined.is_empty() {
                    "New Tab".to_string()
                } else {
                    combined
                };
                tab.page = 0;
                tab.autocomplete.clear();
                self.fetch_tab(self.active)
            }
            Message::SubmitSearch => {
                let tab = &mut self.tabs[self.active];
                tab.query = Some(tab.temp_query.clone());
                tab.title = if tab.temp_query.is_empty() {
                    "New Tab".to_string()
                } else {
                    tab.temp_query.clone()
                };
                tab.page = 0;
                tab.autocomplete.clear();
                self.fetch_tab(self.active)
            }
            Message::NextPage => {
                let tab = &mut self.tabs[self.active];
                if tab.has_next_page {
                    tab.page += 1;
                    return self.fetch_tab(self.active);
                }
                Task::none()
            }
            Message::PrevPage => {
                let tab = &mut self.tabs[self.active];
                if tab.page > 0 {
                    tab.page -= 1;
                    return self.fetch_tab(self.active);
                }
                Task::none()
            }
            Message::SiteSelected(site) => {
                let tab = &mut self.tabs[self.active];
                tab.site = site;
                tab.page = 0;
                self.fetch_tab(self.active)
            }
            Message::RatingSelected(choice) => {
                self.tabs[self.active].rating = choice.0;
                self.fetch_tab(self.active)
            }

            // --- Results -----------------------------------------------
            Message::PostsLoaded {
                tab,
                generation,
                attempt,
                result,
            } => self.handle_posts_loaded(tab, generation, attempt, result),
            Message::TagGroupsLoaded {
                tab,
                post_id,
                groups,
            } => {
                if !groups.is_empty() {
                    if let Some(idx) = self.tab_index(tab) {
                        if let Some(post) =
                            self.tabs[idx].posts.iter_mut().find(|p| p.id == post_id)
                        {
                            post.tag_groups = groups;
                        }
                    }
                }
                Task::none()
            }
            Message::TogglePost(post_id) => self.toggle_post(post_id),
            Message::TagClicked(tag) => {
                let tab = &mut self.tabs[self.active];
                let mut words: Vec<String> = tab
                    .temp_query
                    .split_whitespace()
                    .map(str::to_string)
                    .collect();
                if let Some(pos) = words.iter().position(|w| w == &tag) {
                    words.remove(pos);
                } else {
                    words.push(tag);
                }
                tab.temp_query = words.join(" ");
                Task::none()
            }
            Message::TagHovered(tag) => {
                self.hovered_tag = tag;
                Task::none()
            }
            Message::OpenTagInNewTab(tag) => self.push_tab(Some(tag), false),
            Message::OpenExternal(url) => {
                if let Err(e) = open::that(&url) {
                    tracing::warn!("failed to open {url}: {e}");
                }
                Task::none()
            }

            // --- Images / SWF ------------------------------------------
            Message::ImageLoaded(url, kind, decoded) => {
                self.images.finish_load(url, kind, decoded);
                Task::none()
            }
            Message::SwfLoaded {
                tab,
                post_id,
                bytes,
            } => {
                if let Some(bytes) = bytes {
                    match RufflePlayer::from_bytes(&post_id, &bytes) {
                        Ok(player) => {
                            tracing::info!(post = %post_id, size = ?player.size(), "SWF player ready");
                            if let Some(idx) = self.tab_index(tab) {
                                self.tabs[idx].swf.insert(post_id, player);
                            }
                        }
                        Err(e) => tracing::warn!("failed to load SWF {post_id}: {e}"),
                    }
                }
                Task::none()
            }

            // --- Settings ----------------------------------------------
            Message::OpenSettings => {
                self.settings_draft = self.settings.gelbooru_credentials.clone();
                self.settings_open = true;
                Task::none()
            }
            Message::CloseSettings => {
                self.settings_open = false;
                Task::none()
            }
            Message::SettingsDraftChanged(value) => {
                self.settings_draft = value;
                Task::none()
            }
            Message::SaveSettings => {
                self.settings.gelbooru_credentials = self.settings_draft.clone();
                self.client
                    .set_gelbooru_credentials(self.settings.gelbooru_credentials.clone());
                if let Err(e) = self.settings.save() {
                    tracing::warn!("failed to save settings: {e}");
                }
                self.settings_open = false;
                Task::none()
            }
        }
    }

    // ----- helpers ------------------------------------------------------

    /// Add a tab (optionally pre-seeded with a query) and maybe activate it.
    /// Returns a fetch Task when the tab starts with a query.
    fn push_tab(&mut self, query: Option<String>, activate: bool) -> Task<Message> {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let has_query = query.as_ref().is_some();
        self.tabs.push(Tab::new(id, query));
        let new_idx = self.tabs.len() - 1;
        if activate {
            self.active = new_idx;
        }
        if has_query {
            self.fetch_tab(new_idx)
        } else {
            Task::none()
        }
    }

    /// Begin a fresh fetch for a tab (no-op if it has no submitted query).
    fn fetch_tab(&mut self, idx: usize) -> Task<Message> {
        let tab = &mut self.tabs[idx];
        if tab.query.is_none() {
            return Task::none();
        }
        tab.generation += 1;
        tab.loading = true;
        tracing::info!(
            tab = tab.id,
            site = %tab.site,
            page = tab.page,
            query = ?tab.query,
            "fetching posts"
        );
        build_fetch_task(
            self.client.clone(),
            tab.id,
            tab.generation,
            tab.site,
            tab.query.clone().unwrap_or_default(),
            tab.page,
            tab.rating,
            0,
        )
    }

    fn handle_posts_loaded(
        &mut self,
        tab_id: u64,
        generation: u64,
        attempt: u32,
        result: Result<PostsPage, String>,
    ) -> Task<Message> {
        let Some(idx) = self.tab_index(tab_id) else {
            return Task::none();
        };
        // Discard responses from a superseded fetch.
        if self.tabs[idx].generation != generation {
            return Task::none();
        }

        match result {
            Ok(page) => {
                tracing::info!(
                    tab = tab_id,
                    posts = page.posts.len(),
                    has_next = page.has_next_page,
                    "posts loaded"
                );
                let tab = &mut self.tabs[idx];
                tab.posts = page.posts;
                tab.has_next_page = page.has_next_page;
                tab.selected.clear();
                tab.swf.clear();
                tab.loading = false;
                tab.error = if tab.posts.is_empty() {
                    Some("No results found.".to_string())
                } else {
                    None
                };
                // Kick off thumbnail loads for the new posts.
                let urls: Vec<String> = tab.posts.iter().filter_map(preview_url).collect();
                let mut task = self.load_images(urls, Some(THUMB_MAX), ImageKind::Thumbnail);

                // Debug: auto-expand a post (prefer an SWF) to exercise the
                // detail view, full-image, and Ruffle paths headlessly.
                if std::env::var("RIBB_DEBUG_EXPAND").is_ok() {
                    let tab = &self.tabs[idx];
                    let target = tab
                        .posts
                        .iter()
                        .find(|p| is_swf(&p.file_url))
                        .or_else(|| tab.posts.first())
                        .map(|p| p.id.clone());
                    if let Some(id) = target {
                        tracing::info!("debug: auto-expanding post {id}");
                        task = Task::batch([task, Task::done(Message::TogglePost(id))]);
                    }
                }
                task
            }
            Err(msg) => {
                if attempt + 1 < MAX_FETCH_ATTEMPTS {
                    tracing::warn!(tab = tab_id, attempt, "fetch failed: {msg}; retrying");
                    let tab = &mut self.tabs[idx];
                    tab.error = Some(format!("{msg}\nRetrying..."));
                    build_fetch_task(
                        self.client.clone(),
                        tab.id,
                        generation,
                        tab.site,
                        tab.query.clone().unwrap_or_default(),
                        tab.page,
                        tab.rating,
                        attempt + 1,
                    )
                } else {
                    tracing::error!(tab = tab_id, "fetch failed: {msg}");
                    let tab = &mut self.tabs[idx];
                    tab.posts.clear();
                    tab.selected.clear();
                    tab.swf.clear();
                    tab.has_next_page = false;
                    tab.loading = false;
                    tab.error = Some(msg);
                    Task::none()
                }
            }
        }
    }

    /// Expand/collapse a post in the active tab, loading media as needed.
    fn toggle_post(&mut self, post_id: String) -> Task<Message> {
        let idx = self.active;
        let tab = &mut self.tabs[idx];
        if let Some(pos) = tab.selected.iter().position(|id| id == &post_id) {
            tab.selected.remove(pos);
            tab.swf.remove(&post_id);
            return Task::none();
        }
        tab.selected.push(post_id.clone());

        let Some(post) = tab.posts.iter().find(|p| p.id == post_id).cloned() else {
            return Task::none();
        };
        let tab_id = tab.id;
        let mut tasks = Vec::new();

        // Resolve category-grouped tags (inline for most providers; a fetch for
        // Gelbooru). Mirrors ebb's PostDetails `getTagGroups` on mount.
        let client = self.client.clone();
        let site = tab.site;
        let post_for_groups = post.clone();
        let gid = post_id.clone();
        tasks.push(Task::perform(
            async move { client.get_tag_groups(site, &post_for_groups).await },
            move |res| Message::TagGroupsLoaded {
                tab: tab_id,
                post_id: gid.clone(),
                groups: res.unwrap_or_default(),
            },
        ));

        if is_image(&post.file_url) {
            let cap = self.full_image_cap(&post);
            tracing::info!(
                native_w = post.width,
                native_h = post.height,
                decode_cap = cap,
                "expanding full image: {}",
                post.file_url
            );
            tasks.push(self.load_images(vec![post.file_url.clone()], Some(cap), ImageKind::Full));
        } else if is_swf(&post.file_url) && !self.tabs[idx].swf.contains_key(&post_id) {
            let http = self.client.http().clone();
            let url = post.file_url.clone();
            tasks.push(Task::perform(
                async move { fetch_bytes(http, url).await },
                move |(_url, bytes)| Message::SwfLoaded {
                    tab: tab_id,
                    post_id: post_id.clone(),
                    bytes,
                },
            ));
        }

        // Mark this post as the scroll anchor; the view tags its image with
        // `anchor_id`, then the operation reads the laid-out bounds and we
        // scroll to center it (exact for any number of expanded posts).
        self.scroll_anchor = Some(post.id.clone());
        tasks.push(
            iced::advanced::widget::operate(CenterOnAnchor::new(
                self.anchor_id.clone(),
                self.scroll_id.clone(),
            ))
            .map(Message::ScrollComputed),
        );
        Task::batch(tasks)
    }

    /// Target decode size (longest edge) for a post's full image: ~2x its
    /// on-screen size for crisp supersampling, clamped to the source's native
    /// size and an absolute ceiling.
    fn full_image_cap(&self, post: &BooruPost) -> u32 {
        if post.width == 0 || post.height == 0 {
            return FULL_MAX_CEIL;
        }
        let vp = self.viewport.get();
        let (rw, rh) = render_size(post.width, post.height, vp.width, vp.height);
        let target = (rw.max(rh) * FULL_SUPERSAMPLE).ceil() as u32;
        // Never exceed the source's native size or the memory ceiling.
        target.min(post.width.max(post.height)).min(FULL_MAX_CEIL)
    }

    /// Begin loading any of `urls` not already cached for `kind`, downscaling to
    /// `max_dim`; returns a batched Task. Decoding happens off the render thread.
    /// Full images use a Lanczos3 filter; thumbnails use the fast path.
    fn load_images(
        &mut self,
        urls: Vec<String>,
        max_dim: Option<u32>,
        kind: ImageKind,
    ) -> Task<Message> {
        let high_quality = matches!(kind, ImageKind::Full);
        let mut tasks = Vec::new();
        for url in urls {
            if self.images.begin_load(&url, kind) {
                let http = self.client.http().clone();
                let sem = self.image_sem.clone();
                tasks.push(Task::perform(
                    async move { fetch_image(http, sem, url, max_dim, high_quality).await },
                    move |(url, decoded)| Message::ImageLoaded(url, kind, decoded),
                ));
            }
        }
        Task::batch(tasks)
    }
}

/// A widget operation that finds the laid-out bounds of the anchored image and
/// the scroll viewport, then computes the scroll offset that centers the image.
/// Content child bounds are reported untranslated (relative to the scrollable's
/// top), so the content-space y is simply `anchor.y - scrollable.y`.
struct CenterOnAnchor {
    anchor: Id,
    scroll: Id,
    anchor_bounds: Option<Rectangle>,
    scroll_bounds: Option<Rectangle>,
}

impl CenterOnAnchor {
    fn new(anchor: Id, scroll: Id) -> Self {
        Self {
            anchor,
            scroll,
            anchor_bounds: None,
            scroll_bounds: None,
        }
    }
}

impl operation::Operation<f32> for CenterOnAnchor {
    fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn operation::Operation<f32>)) {
        operate(self);
    }

    fn container(&mut self, id: Option<&Id>, bounds: Rectangle) {
        if id == Some(&self.anchor) {
            self.anchor_bounds = Some(bounds);
        }
    }

    fn scrollable(
        &mut self,
        id: Option<&Id>,
        bounds: Rectangle,
        _content_bounds: Rectangle,
        _translation: Vector,
        _state: &mut dyn operation::Scrollable,
    ) {
        if id == Some(&self.scroll) {
            self.scroll_bounds = Some(bounds);
        }
    }

    fn finish(&self) -> operation::Outcome<f32> {
        match (self.anchor_bounds, self.scroll_bounds) {
            (Some(a), Some(s)) => {
                let content_y = a.y - s.y;
                let offset = (content_y - (s.height - a.height) / 2.0).max(0.0);
                operation::Outcome::Some(offset)
            }
            _ => operation::Outcome::None,
        }
    }
}

/// Build the async post-search Task. Delays one second before retries.
#[allow(clippy::too_many_arguments)]
fn build_fetch_task(
    client: BooruClient,
    tab_id: u64,
    generation: u64,
    site: Site,
    tags: String,
    page: u32,
    rating: Option<Rating>,
    attempt: u32,
) -> Task<Message> {
    Task::perform(
        async move {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            let query = PostQuery {
                tags,
                limit: PAGE_LIMIT,
                page,
                rating,
            };
            client
                .get_posts(site, &query)
                .await
                .map_err(|e| e.to_string())
        },
        move |result| Message::PostsLoaded {
            tab: tab_id,
            generation,
            attempt,
            result,
        },
    )
}

/// Number of grid columns for a given available width (ebb's responsive grid).
fn grid_cols(width: f32) -> usize {
    if width >= 1024.0 {
        4
    } else if width >= 768.0 {
        3
    } else if width >= 640.0 {
        2
    } else {
        1
    }
}

/// Square cell size for a given width and column count. Only the gaps *between*
/// cells are subtracted — the grid has no outer padding (touches the edges).
fn grid_cell(width: f32, cols: usize) -> f32 {
    ((width - GRID_GAP * (cols as f32 - 1.0)) / cols as f32).max(80.0)
}

/// The word currently being typed — the text after the last space. A query
/// ending in whitespace (or empty) has no active word.
fn last_word(query: &str) -> String {
    if query.is_empty() || query.ends_with(char::is_whitespace) {
        return String::new();
    }
    query
        .rsplit(char::is_whitespace)
        .next()
        .unwrap_or("")
        .to_string()
}

/// The thumbnail URL ebb's `PostPreview` would pick: first displayable image
/// among sample → preview → file, skipping known placeholders.
fn preview_url(post: &BooruPost) -> Option<String> {
    let mut candidates: Vec<&str> = Vec::new();
    if let Some(s) = &post.sample_url {
        candidates.push(s);
    }
    candidates.push(&post.preview_url);
    candidates.push(&post.file_url);
    candidates
        .into_iter()
        .filter(|u| !u.is_empty() && !PREVIEW_BLACKLIST.contains(u))
        .find(|u| is_image(u))
        .map(str::to_string)
}

impl Default for Ribb {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// View — see `view.rs`
// ---------------------------------------------------------------------------

include!("view.rs");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn last_word_extracts_active_word() {
        assert_eq!(last_word("landscape"), "landscape");
        assert_eq!(last_word("blue sky land"), "land");
        assert_eq!(last_word("a  b"), "b");
        assert_eq!(last_word("blue "), ""); // trailing space => no active word
        assert_eq!(last_word(""), "");
    }

    #[test]
    fn format_count_humanizes() {
        assert_eq!(format_count(5), "5");
        assert_eq!(format_count(1_500), "1.5k");
        assert_eq!(format_count(2_500_000), "2.5m");
    }

    #[test]
    fn preview_url_skips_placeholder_and_non_images() {
        let post = BooruPost {
            id: "1".into(),
            post_view: String::new(),
            tags: vec![],
            tag_groups: vec![],
            file_url: "https://x/a.webm".into(),
            preview_url: "https://cdn.donmai.us/images/flash-preview.png".into(),
            sample_url: Some("https://x/s.jpg".into()),
            width: 0,
            height: 0,
            rating: "general".into(),
            created_at: String::new(),
        };
        // Sample (image) wins over the blacklisted preview and the webm file.
        assert_eq!(preview_url(&post).as_deref(), Some("https://x/s.jpg"));
    }
}

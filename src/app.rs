//! The iced application — a port of ebb's React component tree.
//!
//! ebb's two React Contexts become flat state here: app-wide state on [`Ribb`]
//! (tabs, settings, shared image cache) and per-tab search state on [`Tab`].
//! `useEffect`-driven fetches become explicit [`Task`]s returned from `update`.

use std::collections::HashMap;

use iced::widget::{
    button, column, container, image, mouse_area, pick_list, responsive, row, scrollable, stack,
    text, text_input, Column, Row, Space,
};
use iced::{Center, Color, ContentFit, Element, Length, Size, Subscription, Task, Theme};
use iced_ruffle::{Ruffle, RufflePlayer};

use crate::booru::{
    is_image, is_swf, is_video, BooruClient, BooruPost, BooruTag, PostQuery, PostsPage, Rating,
    Site, TagGroup,
};
use crate::cache::{fetch_image, ImageCache, ImageState};
use crate::settings::Settings;
use crate::style;

const PAGE_LIMIT: u32 = 100;
const MAX_FETCH_ATTEMPTS: u32 = 3;
const GRID_GAP: f32 = 8.0;

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
    #[allow(dead_code)]
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
    TogglePost(String),
    TagClicked(String),
    OpenTagInNewTab(String),
    OpenExternal(String),
    // Images / SWF
    ImageLoaded(String, Option<Vec<u8>>),
    SwfLoaded {
        tab: u64,
        post_id: String,
        bytes: Option<Vec<u8>>,
    },
    // Settings
    OpenSettings,
    CloseSettings,
    SettingsDraftChanged(String),
    SaveSettings,
    // Window / input
    WindowResized(Size),
    Key(iced::keyboard::Event),
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
            window: Size::new(1100.0, 800.0),
        }
    }

    fn active_tab(&self) -> &Tab {
        &self.tabs[self.active]
    }

    fn tab_index(&self, id: u64) -> Option<usize> {
        self.tabs.iter().position(|t| t.id == id)
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
                self.tabs[self.active].temp_query = value;
                Task::none()
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
            Message::OpenTagInNewTab(tag) => self.push_tab(Some(tag), false),
            Message::OpenExternal(url) => {
                if let Err(e) = open::that(&url) {
                    tracing::warn!("failed to open {url}: {e}");
                }
                Task::none()
            }

            // --- Images / SWF ------------------------------------------
            Message::ImageLoaded(url, bytes) => {
                self.images.finish_load(url, bytes);
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
                self.load_images(urls)
            }
            Err(msg) => {
                if attempt + 1 < MAX_FETCH_ATTEMPTS {
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
            tasks.push(self.load_images(vec![post.file_url.clone()]));
        } else if is_swf(&post.file_url) && !self.tabs[idx].swf.contains_key(&post_id) {
            let http = self.client.http().clone();
            let url = post.file_url.clone();
            tasks.push(Task::perform(
                async move { fetch_image(http, url).await },
                move |(_url, bytes)| Message::SwfLoaded {
                    tab: tab_id,
                    post_id: post_id.clone(),
                    bytes,
                },
            ));
        }
        Task::batch(tasks)
    }

    /// Begin loading any of `urls` not already cached; returns a batched Task.
    fn load_images(&mut self, urls: Vec<String>) -> Task<Message> {
        let mut tasks = Vec::new();
        for url in urls {
            if self.images.begin_load(&url) {
                let http = self.client.http().clone();
                tasks.push(Task::perform(
                    async move { fetch_image(http, url).await },
                    |(url, bytes)| Message::ImageLoaded(url, bytes),
                ));
            }
        }
        Task::batch(tasks)
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

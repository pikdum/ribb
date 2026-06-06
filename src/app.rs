//! The iced application — a port of ebb's React component tree.
//!
//! ebb's two React Contexts become flat state here: app-wide state on [`Ribb`]
//! (tabs, settings, shared image cache) and per-tab search state on [`Tab`].
//! `useEffect`-driven fetches become explicit [`Task`]s returned from `update`.

use std::cell::Cell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iced::advanced::widget::Id;
use iced::widget::{
    button, column, container, image, mouse_area, pick_list, responsive, row, scrollable, stack,
    text, text_input, Column, Row, Space,
};
use iced::{Border, Center, Color, ContentFit, Element, Length, Size, Subscription, Task, Theme};
use iced_ruffle::{Ruffle, RufflePlayer};
use lucide_icons::iced::{
    icon_arrow_left, icon_check, icon_chevron_left, icon_chevron_right, icon_copy, icon_download,
    icon_external_link, icon_pause, icon_play, icon_plus, icon_search, icon_settings,
    icon_volume_2, icon_volume_x, icon_x,
};

use crate::booru::{
    is_image, is_swf, is_video, BooruClient, BooruPost, BooruTag, PostQuery, PostsPage, Rating,
    Site, TagGroup,
};
use crate::cache::{fetch_bytes, fetch_image, DecodedImage, ImageCache, ImageKind, ImageState};
use crate::settings::Settings;
use crate::style;
use crate::video::Player;

const PAGE_LIMIT: u32 = 100;
const MAX_FETCH_ATTEMPTS: u32 = 3;
/// Stable widget id for the search box, so `update` can drive its caret/focus.
const SEARCH_INPUT_ID: &str = "ribb-search";
/// Stable widget id for the settings credential input (focused when the modal opens).
const SETTINGS_INPUT_ID: &str = "ribb-settings";
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
    /// Tab whose saved scroll offset is about to be restored after activation.
    pending_scroll_restore: Option<u64>,
    /// Latest keyboard modifier state; used for Ctrl+click tag behavior.
    modifiers: iced::keyboard::Modifiers,
    /// Exact content-area size, measured by the view's `responsive` wrapper and
    /// read back here for full-image sizing.
    viewport: Cell<Size>,
    window: Size,
    /// Lazily-opened system clipboard for the "Copy" image action. Held alive
    /// for the app's lifetime so X11 selection ownership persists after a copy
    /// (dropping it would drop the selection); `None` until the first copy.
    clipboard: Option<arboard::Clipboard>,
    /// Transient UI status of per-post action buttons (Download / Copy). Absent
    /// = idle; a `Done`/`Failed` entry self-clears ~2s after it settles.
    actions: HashMap<(String, ActionKind), ActionStatus>,
}

/// Which per-post action a button drives (key into [`Ribb::actions`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActionKind {
    Download,
    Copy,
}

/// Transient UI status of a per-post action button. Idle is the absence of an
/// entry; this enum is only the non-idle states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionStatus {
    InProgress,
    Done,
    Failed,
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
    /// Post currently replacing the grid in focus mode.
    focused: Option<String>,
    loading: bool,
    error: Option<String>,
    /// Live tag suggestions for the last word being typed.
    autocomplete: Vec<BooruTag>,
    /// Keyboard-highlighted suggestion (downshift-style); `None` = no highlight.
    autocomplete_index: Option<usize>,
    /// Bumped on every fresh fetch so stale responses can be discarded.
    generation: u64,
    /// Loaded Flash players, keyed by post ID.
    swf: HashMap<String, RufflePlayer>,
    /// Loaded video players (ffmpeg-backed), keyed by post ID.
    video: HashMap<String, Player>,
    /// Id of this tab's content scrollable.
    scroll_id: Id,
    /// Last observed vertical scroll offset for this tab.
    scroll_y: f32,
    /// Grid scroll offset saved when entering a post's focus view, so exiting
    /// returns to exactly where the grid was.
    grid_scroll_y: f32,
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
            focused: None,
            loading: false,
            error: None,
            autocomplete: Vec::new(),
            autocomplete_index: None,
            generation: 0,
            swf: HashMap::new(),
            video: HashMap::new(),
            scroll_id: Id::unique(),
            scroll_y: 0.0,
            grid_scroll_y: 0.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Messages
// ---------------------------------------------------------------------------

/// A decoded RGBA8 image ready to be placed on the clipboard. Carried in a
/// `Message`, so it has a compact `Debug` that prints the byte count rather
/// than the whole pixel buffer.
#[derive(Clone)]
pub struct ClipboardImage {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

impl std::fmt::Debug for ClipboardImage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("bytes", &self.rgba.len())
            .finish()
    }
}

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
    /// Enter pressed in the search box: apply the highlighted suggestion if
    /// one is selected, otherwise submit the search.
    SearchEnter,
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
    DownloadPost {
        post_id: String,
        url: String,
    },
    DownloadFinished {
        post_id: String,
        result: Result<PathBuf, String>,
    },
    /// Copy the post's full image to the system clipboard (so it can be pasted
    /// into another app). Only offered for image posts.
    CopyImage {
        post_id: String,
        url: String,
    },
    /// The fetched+decoded image is ready to hand to the clipboard (or an error).
    ImageCopyReady {
        post_id: String,
        result: Result<ClipboardImage, String>,
    },
    /// Revert a settled (Done/Failed) action button back to its idle state.
    ClearAction {
        post_id: String,
        kind: ActionKind,
    },
    OpenExternal(String),
    // Images / SWF
    ImageLoaded(String, ImageKind, Option<DecodedImage>),
    SwfLoaded {
        tab: u64,
        post_id: String,
        bytes: Option<Vec<u8>>,
    },
    VideoLoaded {
        tab: u64,
        post_id: String,
        bytes: Option<Vec<u8>>,
    },
    /// Per-frame tick (from `window::frames`) to advance video playback.
    VideoTick(Instant),
    /// Video controls (act on the named post's player in the active tab).
    VideoTogglePlay(String),
    VideoToggleMute(String),
    VideoSeek {
        post_id: String,
        ms: f32,
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
    /// Current vertical scroll offset for a tab.
    ScrollChanged {
        tab: u64,
        y: f32,
    },
    /// Restore a tab's saved scroll offset after it becomes visible.
    RestoreScroll {
        tab: u64,
        y: f32,
    },
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
            pending_scroll_restore: None,
            modifiers: iced::keyboard::Modifiers::default(),
            viewport: Cell::new(Size::new(1100.0, 700.0)),
            window: Size::new(1100.0, 800.0),
            clipboard: None,
            actions: HashMap::new(),
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
        // Focus the search box on launch so the user can type immediately (and,
        // under RIBB_DEBUG_TYPE, so the on_submit/Enter path is reachable).
        let task = Task::batch([task, iced::widget::operation::focus(SEARCH_INPUT_ID)]);
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
        let mut subs = vec![keys, resizes];
        // Only drive per-frame redraws while the visible tab is playing a video.
        if !self.tabs[self.active].video.is_empty() {
            subs.push(iced::window::frames().map(Message::VideoTick));
        }
        Subscription::batch(subs)
    }

    /// Put a decoded image on the system clipboard, returning whether it stuck.
    /// The clipboard handle is opened lazily and kept for the app's lifetime;
    /// on X11 the selection is only served while the handle lives.
    fn copy_to_clipboard(&mut self, post_id: &str, image: ClipboardImage) -> bool {
        let clipboard = match &mut self.clipboard {
            Some(c) => c,
            None => match arboard::Clipboard::new() {
                Ok(c) => self.clipboard.insert(c),
                Err(e) => {
                    tracing::warn!("clipboard unavailable: {e}");
                    return false;
                }
            },
        };
        let (width, height) = (image.width, image.height);
        let data = arboard::ImageData {
            width,
            height,
            bytes: image.rgba.into(),
        };
        match clipboard.set_image(data) {
            Ok(()) => {
                tracing::info!(post = %post_id, width, height, "copied image to clipboard");
                true
            }
            Err(e) => {
                tracing::warn!(post = %post_id, "clipboard set failed: {e}");
                false
            }
        }
    }

    /// Translate a Ctrl-modified key press into a tab action (ebb's shortcuts).
    fn handle_key(&mut self, event: iced::keyboard::Event) -> Task<Message> {
        use iced::keyboard::key::{Key, Named};
        use iced::keyboard::Event;
        match &event {
            Event::KeyPressed { modifiers, .. } | Event::KeyReleased { modifiers, .. } => {
                self.modifiers = *modifiers;
            }
            Event::ModifiersChanged(modifiers) => {
                self.modifiers = *modifiers;
            }
        }
        let Event::KeyPressed { key, modifiers, .. } = event else {
            return Task::none();
        };

        // Downshift-style autocomplete navigation: plain (un-modified) Up/Down
        // cycle the highlight, Escape dismisses. Only while the dropdown shows,
        // so it never shadows browsing or the Ctrl shortcuts below.
        if !modifiers.control() && !self.tabs[self.active].autocomplete.is_empty() {
            let len = self.tabs[self.active].autocomplete.len();
            let tab = &mut self.tabs[self.active];
            match key.as_ref() {
                Key::Named(Named::ArrowDown) => {
                    tab.autocomplete_index = Some(match tab.autocomplete_index {
                        None => 0,
                        Some(i) => (i + 1) % len,
                    });
                    return Task::none();
                }
                Key::Named(Named::ArrowUp) => {
                    tab.autocomplete_index = Some(match tab.autocomplete_index {
                        None => len - 1,
                        Some(i) => (i + len - 1) % len,
                    });
                    return Task::none();
                }
                Key::Named(Named::Escape) => {
                    tab.autocomplete.clear();
                    tab.autocomplete_index = None;
                    return Task::none();
                }
                _ => {}
            }
        }

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
        // Sticky search focus (ebb's auto-focused input): most interactions
        // blur the search box, since iced focuses whatever widget you click.
        // After the interactions below we re-assert focus so the user can keep
        // typing and hit Enter — unless the settings modal is open, which owns
        // focus while shown.
        let refocus_search = wants_search_focus(&message);
        let task = self.dispatch(message);
        if refocus_search && !self.settings_open {
            Task::batch([task, iced::widget::operation::focus(SEARCH_INPUT_ID)])
        } else {
            task
        }
    }

    fn dispatch(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::WindowResized(size) => {
                self.window = size;
                Task::none()
            }
            Message::Key(event) => self.handle_key(event),
            Message::Noop => Task::none(),
            Message::ScrollChanged { tab, y } => {
                if self.pending_scroll_restore == Some(tab) {
                    return Task::none();
                }
                if let Some(idx) = self.tab_index(tab) {
                    self.tabs[idx].scroll_y = y;
                }
                Task::none()
            }
            Message::RestoreScroll { tab, y } => {
                if self.pending_scroll_restore == Some(tab) {
                    self.pending_scroll_restore = None;
                }
                self.tab_index(tab)
                    .map(|idx| {
                        self.tabs[idx].scroll_y = y;
                        self.scroll_tab_to(idx, y)
                    })
                    .unwrap_or_else(Task::none)
            }
            // --- Tabs ---------------------------------------------------
            Message::NewTab => self.push_tab(None, true),
            Message::SelectTab(idx) => {
                if idx < self.tabs.len() {
                    self.active = idx;
                    return self.defer_tab_restore(idx);
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
                    return self.defer_active_restore();
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
                    return self.defer_active_restore();
                }
                Task::none()
            }
            Message::SwitchTabRight => {
                if self.tabs.len() > 1 {
                    self.active = (self.active + 1) % self.tabs.len();
                    return self.defer_active_restore();
                }
                Task::none()
            }

            // --- Search / filters --------------------------------------
            Message::QueryChanged(value) => {
                let tab = &mut self.tabs[self.active];
                tab.temp_query = value.clone();
                // The suggestion set is about to change; drop any highlight.
                tab.autocomplete_index = None;
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
                    let t = &self.tabs[idx];
                    // Drop the response if the word being completed has moved on,
                    // or if this query was already submitted verbatim — a slow
                    // reply would otherwise re-open the dropdown after a search.
                    let accept = last_word(&t.temp_query) == query_word
                        && t.query.as_deref() != Some(t.temp_query.as_str());
                    if accept {
                        tracing::debug!(word = %query_word, count = tags.len(), "autocomplete");
                        self.tabs[idx].autocomplete = tags;
                        self.tabs[idx].autocomplete_index = None;
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
                tab.scroll_y = 0.0;
                tab.autocomplete.clear();
                tab.autocomplete_index = None;
                Task::batch([
                    self.scroll_active_to_top(),
                    self.fetch_tab(self.active),
                    // Keep the caret at the end of the now-inserted tag (and
                    // refocus when the selection came from a click).
                    iced::widget::operation::focus(SEARCH_INPUT_ID),
                    iced::widget::operation::move_cursor_to_end(SEARCH_INPUT_ID),
                ])
            }
            Message::SearchEnter => {
                // Apply the highlighted suggestion if one is selected, else
                // submit the search as-typed.
                let tab = &self.tabs[self.active];
                let selected = tab
                    .autocomplete_index
                    .and_then(|i| tab.autocomplete.get(i))
                    .map(|t| t.value.clone());
                match selected {
                    Some(value) => self.update(Message::AutocompleteSelected(value)),
                    None => self.update(Message::SubmitSearch),
                }
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
                tab.scroll_y = 0.0;
                tab.autocomplete.clear();
                tab.autocomplete_index = None;
                Task::batch([self.scroll_active_to_top(), self.fetch_tab(self.active)])
            }
            Message::NextPage => {
                let tab = &mut self.tabs[self.active];
                if tab.has_next_page {
                    tab.page += 1;
                    tab.scroll_y = 0.0;
                    return Task::batch([self.scroll_active_to_top(), self.fetch_tab(self.active)]);
                }
                Task::none()
            }
            Message::PrevPage => {
                let tab = &mut self.tabs[self.active];
                if tab.page > 0 {
                    tab.page -= 1;
                    tab.scroll_y = 0.0;
                    return Task::batch([self.scroll_active_to_top(), self.fetch_tab(self.active)]);
                }
                Task::none()
            }
            Message::SiteSelected(site) => {
                let tab = &mut self.tabs[self.active];
                tab.site = site;
                tab.page = 0;
                tab.scroll_y = 0.0;
                Task::batch([self.scroll_active_to_top(), self.fetch_tab(self.active)])
            }
            Message::RatingSelected(choice) => {
                let tab = &mut self.tabs[self.active];
                tab.rating = choice.0;
                tab.scroll_y = 0.0;
                Task::batch([self.scroll_active_to_top(), self.fetch_tab(self.active)])
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
                if self.modifiers.control() {
                    return self.push_tab(Some(tag), false);
                }
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
            Message::DownloadPost { post_id, url } => {
                self.actions.insert(
                    (post_id.clone(), ActionKind::Download),
                    ActionStatus::InProgress,
                );
                let client = self.client.http().clone();
                let download_post_id = post_id.clone();
                Task::perform(
                    async move { download_post_file(client, &download_post_id, url).await },
                    move |result| Message::DownloadFinished {
                        post_id: post_id.clone(),
                        result,
                    },
                )
            }
            Message::DownloadFinished { post_id, result } => {
                let status = match result {
                    Ok(path) => {
                        tracing::info!(post = %post_id, path = %path.display(), "downloaded post file");
                        ActionStatus::Done
                    }
                    Err(e) => {
                        tracing::warn!(post = %post_id, "download failed: {e}");
                        ActionStatus::Failed
                    }
                };
                self.actions
                    .insert((post_id.clone(), ActionKind::Download), status);
                clear_action_later(post_id, ActionKind::Download)
            }
            Message::CopyImage { post_id, url } => {
                self.actions.insert(
                    (post_id.clone(), ActionKind::Copy),
                    ActionStatus::InProgress,
                );
                let client = self.client.http().clone();
                Task::perform(
                    async move { fetch_image_rgba(client, url).await },
                    move |result| Message::ImageCopyReady {
                        post_id: post_id.clone(),
                        result,
                    },
                )
            }
            Message::ImageCopyReady { post_id, result } => {
                let ok = match result {
                    Ok(image) => self.copy_to_clipboard(&post_id, image),
                    Err(e) => {
                        tracing::warn!(post = %post_id, "copy failed: {e}");
                        false
                    }
                };
                let status = if ok {
                    ActionStatus::Done
                } else {
                    ActionStatus::Failed
                };
                self.actions
                    .insert((post_id.clone(), ActionKind::Copy), status);
                clear_action_later(post_id, ActionKind::Copy)
            }
            Message::ClearAction { post_id, kind } => {
                // Only clear a settled status; if a fresh action is now in
                // progress for this button, leave it be.
                let key = (post_id, kind);
                if self.actions.get(&key) != Some(&ActionStatus::InProgress) {
                    self.actions.remove(&key);
                }
                Task::none()
            }
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
            Message::VideoLoaded {
                tab,
                post_id,
                bytes,
            } => {
                if let Some(bytes) = bytes {
                    match Player::from_bytes(&bytes) {
                        Ok(player) => {
                            tracing::info!(post = %post_id, "video player ready");
                            if let Some(idx) = self.tab_index(tab) {
                                self.tabs[idx].video.insert(post_id, player);
                            }
                        }
                        Err(e) => tracing::warn!("failed to start video {post_id}: {e}"),
                    }
                }
                Task::none()
            }
            Message::VideoTick(now) => {
                // Advance the active tab's videos; the redraw after this message
                // re-runs `view`, which reads the freshly-presented frame.
                for player in self.tabs[self.active].video.values_mut() {
                    player.tick(now);
                }
                Task::none()
            }
            Message::VideoTogglePlay(post_id) => {
                if let Some(player) = self.tabs[self.active].video.get_mut(&post_id) {
                    player.toggle_playing(Instant::now());
                }
                Task::none()
            }
            Message::VideoToggleMute(post_id) => {
                if let Some(player) = self.tabs[self.active].video.get_mut(&post_id) {
                    player.toggle_muted();
                }
                Task::none()
            }
            Message::VideoSeek { post_id, ms } => {
                if let Some(player) = self.tabs[self.active].video.get_mut(&post_id) {
                    player.seek(ms as i64, Instant::now());
                }
                Task::none()
            }

            // --- Settings ----------------------------------------------
            Message::OpenSettings => {
                self.settings_draft = self.settings.gelbooru_credentials.clone();
                self.settings_open = true;
                iced::widget::operation::focus(SETTINGS_INPUT_ID)
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
        let fetch = if has_query {
            self.fetch_tab(new_idx)
        } else {
            Task::none()
        };
        if activate {
            Task::batch([fetch, self.defer_tab_restore(new_idx)])
        } else {
            fetch
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
                tab.focused = None;
                tab.swf.clear();
                tab.video.clear();
                tab.loading = false;
                tab.error = if tab.posts.is_empty() {
                    Some("No results found.".to_string())
                } else {
                    None
                };
                // Kick off thumbnail loads for the new posts.
                let urls: Vec<String> = tab.posts.iter().filter_map(preview_url).collect();
                let mut task = self.load_images(urls, Some(THUMB_MAX), ImageKind::Thumbnail);

                // Debug: auto-focus a post (prefer an SWF) to exercise the
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
                        tracing::info!("debug: auto-focusing post {id}");
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
                    tab.focused = None;
                    tab.swf.clear();
                    tab.video.clear();
                    tab.has_next_page = false;
                    tab.loading = false;
                    tab.error = Some(msg);
                    Task::none()
                }
            }
        }
    }

    fn scroll_active_to_top(&self) -> Task<Message> {
        self.scroll_tab_to(self.active, 0.0)
    }

    fn defer_active_restore(&mut self) -> Task<Message> {
        self.defer_tab_restore(self.active)
    }

    fn defer_tab_restore(&mut self, idx: usize) -> Task<Message> {
        let tab = self.tabs[idx].id;
        let y = self.tabs[idx].scroll_y;
        self.pending_scroll_restore = Some(tab);
        Task::perform(async move {}, move |_| Message::RestoreScroll { tab, y })
    }

    fn scroll_tab_to(&self, idx: usize, y: f32) -> Task<Message> {
        iced::widget::operation::scroll_to(
            self.tabs[idx].scroll_id.clone(),
            iced::widget::scrollable::AbsoluteOffset { x: 0.0, y },
        )
    }

    /// Enter/exit focus mode for a post in the active tab, loading media as needed.
    fn toggle_post(&mut self, post_id: String) -> Task<Message> {
        let idx = self.active;
        // Exit focus: clear the post and return to the grid where we left it.
        if self.tabs[idx].focused.as_deref() == Some(post_id.as_str()) {
            {
                let tab = &mut self.tabs[idx];
                tab.focused = None;
                tab.swf.remove(&post_id);
                tab.video.remove(&post_id);
                tab.scroll_y = tab.grid_scroll_y;
            }
            return self.defer_active_restore();
        }
        let entering_from_grid = self.tabs[idx].focused.is_none();
        let Some(post) = self.tabs[idx]
            .posts
            .iter()
            .find(|p| p.id == post_id)
            .cloned()
        else {
            return Task::none();
        };

        // Enter focus: remember the grid scroll position to come back to, then
        // start the focused detail view at the top.
        {
            let tab = &mut self.tabs[idx];
            if entering_from_grid {
                tab.grid_scroll_y = tab.scroll_y;
            }
            tab.focused = Some(post_id.clone());
            tab.scroll_y = 0.0;
        }

        let tab_id = self.tabs[idx].id;
        let mut tasks = Vec::new();

        // Resolve category-grouped tags (inline for most providers; a fetch for
        // Gelbooru). Mirrors ebb's PostDetails `getTagGroups` on mount.
        let client = self.client.clone();
        let site = self.tabs[idx].site;
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
                "loading focus image: {}",
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
        } else if is_video(&post.file_url) && !self.tabs[idx].video.contains_key(&post_id) {
            // Fetch with our own client (booru CDNs reject ffmpeg's default HTTP
            // headers; Gelbooru needs a Referer), then decode from a temp file.
            let http = self.client.http().clone();
            let url = post.file_url.clone();
            tasks.push(Task::perform(
                async move { fetch_bytes(http, url).await },
                move |(_url, bytes)| Message::VideoLoaded {
                    tab: tab_id,
                    post_id: post_id.clone(),
                    bytes,
                },
            ));
        }

        tasks.push(self.defer_active_restore());
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

/// Interactions after which focus should snap back to the search box (ebb's
/// sticky input). Excludes settings messages (the modal owns focus) and the
/// high-frequency background messages (image/posts loads, scrolls, typing).
fn wants_search_focus(message: &Message) -> bool {
    matches!(
        message,
        Message::TogglePost(_)
            | Message::TagClicked(_)
            | Message::NextPage
            | Message::PrevPage
            | Message::SiteSelected(_)
            | Message::RatingSelected(_)
            | Message::NewTab
            | Message::CloseTab(_)
            | Message::SelectTab(_)
            | Message::SwitchTabLeft
            | Message::SwitchTabRight
            | Message::SubmitSearch
            | Message::OpenExternal(_)
            | Message::DownloadPost { .. }
            | Message::CopyImage { .. }
            | Message::CloseSettings
            | Message::SaveSettings
    )
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

async fn download_post_file(
    client: reqwest::Client,
    post_id: &str,
    url: String,
) -> Result<PathBuf, String> {
    let (_url, bytes) = fetch_bytes(client, url.clone()).await;
    let bytes = bytes.ok_or_else(|| "failed to fetch file".to_string())?;
    let dir = directories::UserDirs::new()
        .and_then(|dirs| dirs.download_dir().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(download_filename(post_id, &url));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Revert a settled action button to idle after a short delay, so "Copied" /
/// "Downloaded" / "Failed" is a brief confirmation rather than a stuck label.
fn clear_action_later(post_id: String, kind: ActionKind) -> Task<Message> {
    Task::perform(
        async { tokio::time::sleep(Duration::from_secs(2)).await },
        move |()| Message::ClearAction {
            post_id: post_id.clone(),
            kind,
        },
    )
}

/// Fetch a post's image and decode it to full-resolution RGBA8 for the
/// clipboard. Like the grid path, decode runs on a blocking thread (CPU-bound);
/// unlike it, there's no downscale — clipboard paste wants the real pixels.
async fn fetch_image_rgba(client: reqwest::Client, url: String) -> Result<ClipboardImage, String> {
    let (_url, bytes) = fetch_bytes(client, url).await;
    let bytes = bytes.ok_or_else(|| "failed to fetch image".to_string())?;
    tokio::task::spawn_blocking(move || {
        let rgba = ::image::load_from_memory(&bytes)
            .map_err(|e| e.to_string())?
            .to_rgba8();
        Ok(ClipboardImage {
            width: rgba.width() as usize,
            height: rgba.height() as usize,
            rgba: rgba.into_raw(),
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

fn download_filename(post_id: &str, url: &str) -> String {
    let extension = reqwest::Url::parse(url)
        .ok()
        .and_then(|parsed| {
            parsed
                .path_segments()
                .and_then(|mut segments| segments.next_back().map(str::to_string))
        })
        .and_then(|name| {
            name.rsplit_once('.')
                .map(|(_, ext)| sanitize_extension(ext))
        })
        .filter(|ext| !ext.is_empty())
        .unwrap_or_else(|| "bin".to_string());
    format!("{post_id}.{extension}")
}

fn sanitize_extension(ext: &str) -> String {
    ext.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(12)
        .collect()
}

/// The post a tab is currently focused on (its detail replaces the grid), if any.
fn focused_post(tab: &Tab) -> Option<&BooruPost> {
    let id = tab.focused.as_ref()?;
    tab.posts.iter().find(|p| &p.id == id)
}

/// just pick preview url - gelbooru blocks using samples here
fn preview_url(post: &BooruPost) -> Option<String> {
    let candidates: Vec<&str> = vec![&post.preview_url];
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

    #[test]
    fn focus_mode_starts_at_top_and_restores_grid_scroll() {
        let mut app = Ribb::new();
        app.tabs[0].scroll_y = 420.0;
        app.tabs[0].posts.push(BooruPost {
            id: "1".into(),
            post_view: String::new(),
            tags: vec![],
            tag_groups: vec![],
            file_url: "https://x/a.bin".into(),
            preview_url: String::new(),
            sample_url: None,
            width: 100,
            height: 100,
            rating: "general".into(),
            created_at: String::new(),
        });

        let _ = app.toggle_post("1".into());
        assert_eq!(app.tabs[0].focused.as_deref(), Some("1"));
        assert_eq!(app.tabs[0].grid_scroll_y, 420.0);
        assert_eq!(app.tabs[0].scroll_y, 0.0);

        let _ = app.toggle_post("1".into());
        assert_eq!(app.tabs[0].focused, None);
        assert_eq!(app.tabs[0].scroll_y, 420.0);
    }

    #[test]
    fn action_status_transitions() {
        let mut app = Ribb::new();
        let pid = "123".to_string();
        let key = (pid.clone(), ActionKind::Download);
        let url = "https://x/a.jpg".to_string();

        // idle -> in progress on click
        assert_eq!(app.actions.get(&key), None);
        let _ = app.update(Message::DownloadPost {
            post_id: pid.clone(),
            url: url.clone(),
        });
        assert_eq!(app.actions.get(&key), Some(&ActionStatus::InProgress));

        // success -> done, then ClearAction reverts to idle
        let _ = app.update(Message::DownloadFinished {
            post_id: pid.clone(),
            result: Ok(PathBuf::from("/tmp/a.jpg")),
        });
        assert_eq!(app.actions.get(&key), Some(&ActionStatus::Done));
        let _ = app.update(Message::ClearAction {
            post_id: pid.clone(),
            kind: ActionKind::Download,
        });
        assert_eq!(app.actions.get(&key), None);

        // failure -> failed
        let _ = app.update(Message::DownloadFinished {
            post_id: pid.clone(),
            result: Err("boom".into()),
        });
        assert_eq!(app.actions.get(&key), Some(&ActionStatus::Failed));

        // a stale ClearAction must not wipe a freshly re-started action
        let _ = app.update(Message::DownloadPost {
            post_id: pid.clone(),
            url,
        });
        assert_eq!(app.actions.get(&key), Some(&ActionStatus::InProgress));
        let _ = app.update(Message::ClearAction {
            post_id: pid,
            kind: ActionKind::Download,
        });
        assert_eq!(app.actions.get(&key), Some(&ActionStatus::InProgress));
    }

    #[test]
    fn download_filename_uses_post_id_and_url_extension() {
        assert_eq!(
            download_filename("14182743", "https://img.example/post/file.jpeg?download=1"),
            "14182743.jpeg"
        );
        assert_eq!(download_filename("42", "not a url"), "42.bin");
    }
}

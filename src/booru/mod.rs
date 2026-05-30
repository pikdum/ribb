//! Native booru API layer.
//!
//! A port of ebb's `renderer/lib/booru/`. Where ebb used a class per provider
//! mixing request-building, HTTP, and parsing, this splits the per-provider
//! logic into a small, pure (synchronous, network-free) [`Provider`] trait —
//! think of it as an Elixir behaviour. The actual HTTP orchestration lives once
//! on [`BooruClient`], so every provider's request-builders and parsers stay
//! trivially unit-testable.

mod danbooru;
mod e621;
mod gelbooru;
mod rule34;
pub mod types;

pub use types::{
    is_image, is_swf, is_video, BooruPost, BooruTag, PostQuery, PostsPage, Rating, TagCategory,
    TagColor, TagGroup,
};

/// Errors surfaced by the booru layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("failed to parse response: {0}")]
    Parse(String),
    /// A structured API error (Danbooru returns `{error, message}`).
    #[error("{error}\n{message}")]
    Api { error: String, message: String },
    #[error("Access denied. Please configure your API credentials in settings.")]
    AuthRequired,
    #[error("Error: {0}")]
    Status(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Map a JSON parse failure into our error type, keeping the original message.
pub(crate) fn parse_err(e: serde_json::Error) -> Error {
    Error::Parse(e.to_string())
}

/// Split a whitespace-separated tag string into a list, dropping empties.
pub(crate) fn split_tags(s: &str) -> Vec<String> {
    s.split_whitespace().map(str::to_string).collect()
}

/// Per-provider request-building and response-parsing. Pure and synchronous:
/// no method performs IO, so each can be tested against canned JSON.
pub trait Provider: Sync {
    /// Build the autocomplete request, or `None` to short-circuit to an empty
    /// result (e.g. e621 ignores queries shorter than 3 chars).
    fn tag_request(&self, client: &BooruClient, query: &str) -> Option<reqwest::RequestBuilder>;
    /// Parse an autocomplete response body into normalized tags.
    fn parse_tags(&self, body: &str) -> Result<Vec<BooruTag>>;

    /// Build the post-search request.
    fn post_request(&self, client: &BooruClient, query: &PostQuery) -> reqwest::RequestBuilder;
    /// Parse a post-search response body into a normalized page of posts.
    fn parse_posts(&self, body: &str) -> Result<PostsPage>;

    /// Build a follow-up request to fetch category-grouped tags for a post.
    /// Defaults to `None`: most providers supply groups inline at parse time
    /// (see [`BooruPost::tag_groups`]).
    fn tag_groups_request(
        &self,
        _client: &BooruClient,
        _post: &BooruPost,
    ) -> Option<reqwest::RequestBuilder> {
        None
    }
    /// Parse the [`Self::tag_groups_request`] response.
    fn parse_tag_groups(&self, _body: &str) -> Result<Vec<TagGroup>> {
        Ok(Vec::new())
    }
}

/// A supported booru site. Acts as the public handle the GUI stores and the
/// dispatch point to the underlying [`Provider`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Site {
    Danbooru,
    Gelbooru,
    E621,
    Rule34,
}

impl Site {
    /// The provider implementation backing this site.
    pub fn provider(self) -> &'static dyn Provider {
        match self {
            Site::Danbooru => &danbooru::Danbooru,
            Site::Gelbooru => &gelbooru::Gelbooru,
            Site::E621 => &e621::E621,
            Site::Rule34 => &rule34::Rule34,
        }
    }

    /// Sites offered in the UI. Rule34 is intentionally omitted (disabled in ebb).
    pub fn enabled() -> &'static [Site] {
        &[Site::Danbooru, Site::Gelbooru, Site::E621]
    }

    /// Available content ratings — identical across providers.
    pub fn ratings(self) -> &'static [Rating] {
        &Rating::ALL
    }

    /// Human-readable site name.
    pub fn label(self) -> &'static str {
        match self {
            Site::Danbooru => "Danbooru",
            Site::Gelbooru => "Gelbooru",
            Site::E621 => "e621",
            Site::Rule34 => "Rule 34",
        }
    }

    /// Favicon/logo URL (as ebb's `getSites` used).
    pub fn icon_url(self) -> &'static str {
        match self {
            Site::Danbooru => "https://danbooru.donmai.us/favicon.svg",
            Site::Gelbooru => "https://gelbooru.com/layout/gelbooru-logo.svg",
            Site::E621 => "https://e621.net/packs/static/main-logo-109ca95d0f436bd372a1.png",
            Site::Rule34 => "https://rule34.xxx/apple-touch-icon-precomposed.png",
        }
    }
}

/// HTTP client + per-site configuration. Owns the shared async orchestration
/// that every provider relies on.
#[derive(Debug, Clone)]
pub struct BooruClient {
    http: reqwest::Client,
    /// Gelbooru API credentials, appended verbatim to API URLs — exactly the
    /// string ebb has you paste into settings, e.g.
    /// `&api_key=...&user_id=...`.
    gelbooru_credentials: String,
}

impl BooruClient {
    /// Construct a client with the default user agent.
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder().user_agent("ribb").build()?;
        Ok(Self {
            http,
            gelbooru_credentials: String::new(),
        })
    }

    /// The underlying reqwest client (used by providers to build requests).
    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// The configured Gelbooru credential string.
    pub fn gelbooru_credentials(&self) -> &str {
        &self.gelbooru_credentials
    }

    /// Set the Gelbooru credential string.
    pub fn set_gelbooru_credentials(&mut self, creds: impl Into<String>) {
        self.gelbooru_credentials = creds.into();
    }

    /// Builder-style variant of [`Self::set_gelbooru_credentials`].
    pub fn with_gelbooru_credentials(mut self, creds: impl Into<String>) -> Self {
        self.gelbooru_credentials = creds.into();
        self
    }

    /// Search for posts. Mirrors ebb's `getPosts`, including its special-cased
    /// error handling for Danbooru's error JSON and Gelbooru's 401.
    pub async fn get_posts(&self, site: Site, query: &PostQuery) -> Result<PostsPage> {
        let provider = site.provider();
        let resp = provider.post_request(self, query).send().await?;
        let status = resp.status();
        let body = resp.text().await?;

        if status.is_success() {
            return provider.parse_posts(&body);
        }

        // Danbooru-style structured error.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
            if let (Some(error), Some(message)) = (
                v.get("error").and_then(|x| x.as_str()),
                v.get("message").and_then(|x| x.as_str()),
            ) {
                return Err(Error::Api {
                    error: error.to_string(),
                    message: message.to_string(),
                });
            }
        }

        if status.as_u16() == 401 && site == Site::Gelbooru {
            return Err(Error::AuthRequired);
        }

        Err(Error::Status(format!(
            "{} {}",
            status.as_u16(),
            status.canonical_reason().unwrap_or("")
        )))
    }

    /// Fetch tag autocomplete suggestions. Mirrors ebb's `getTags`.
    pub async fn get_tags(&self, site: Site, query: &str) -> Result<Vec<BooruTag>> {
        let provider = site.provider();
        let Some(request) = provider.tag_request(self, query) else {
            return Ok(Vec::new());
        };
        let resp = request.send().await?;
        let status = resp.status();
        let body = resp.text().await?;
        if status.is_success() {
            provider.parse_tags(&body)
        } else {
            Err(Error::Status(format!(
                "{} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            )))
        }
    }

    /// Resolve a post's category-grouped tags. Returns the inline groups when
    /// the provider already supplied them, otherwise performs the follow-up
    /// fetch (Gelbooru). Mirrors ebb's per-post `getTagGroups`.
    pub async fn get_tag_groups(&self, site: Site, post: &BooruPost) -> Result<Vec<TagGroup>> {
        let provider = site.provider();
        match provider.tag_groups_request(self, post) {
            Some(request) => {
                let resp = request.send().await?;
                let body = resp.text().await?;
                provider.parse_tag_groups(&body)
            }
            None => Ok(post.tag_groups.clone()),
        }
    }
}

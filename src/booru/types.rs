//! Normalized data types shared across all booru providers.
//!
//! Ported from ebb's `renderer/lib/booru/index.ts` (`BooruPost`, `BooruTag`)
//! with the loose string fields tightened into enums where it helps the GUI.

use serde::{Deserialize, Serialize};

/// Content rating, consistent across the supported providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Rating {
    General,
    Sensitive,
    Questionable,
    Explicit,
}

impl Rating {
    /// Every rating, in display order — mirrors each provider's `ratings` list.
    pub const ALL: [Rating; 4] = [
        Rating::General,
        Rating::Sensitive,
        Rating::Questionable,
        Rating::Explicit,
    ];

    /// The value used in a `rating:<x>` search term (capitalized, as the APIs expect).
    pub fn query_value(self) -> &'static str {
        match self {
            Rating::General => "General",
            Rating::Sensitive => "Sensitive",
            Rating::Questionable => "Questionable",
            Rating::Explicit => "Explicit",
        }
    }

    /// Lowercase form, matching the normalized `BooruPost::rating` string.
    pub fn as_lower(self) -> &'static str {
        match self {
            Rating::General => "general",
            Rating::Sensitive => "sensitive",
            Rating::Questionable => "questionable",
            Rating::Explicit => "explicit",
        }
    }

    /// Single-letter label shown in the rating selector (G/S/Q/E).
    pub fn letter(self) -> char {
        match self {
            Rating::General => 'G',
            Rating::Sensitive => 'S',
            Rating::Questionable => 'Q',
            Rating::Explicit => 'E',
        }
    }

    /// Map a provider's single-char alias (`g`/`s`/`q`/`e`) to a rating.
    pub fn from_alias(alias: &str) -> Option<Rating> {
        match alias {
            "g" => Some(Rating::General),
            "s" => Some(Rating::Sensitive),
            "q" => Some(Rating::Questionable),
            "e" => Some(Rating::Explicit),
            _ => None,
        }
    }

    /// Best-effort parse of any rating string a provider might return
    /// (full word or single-char alias, any case).
    pub fn from_loose(s: &str) -> Option<Rating> {
        let s = s.trim().to_ascii_lowercase();
        match s.as_str() {
            "general" | "g" => Some(Rating::General),
            "sensitive" | "s" | "safe" => Some(Rating::Sensitive),
            "questionable" | "q" => Some(Rating::Questionable),
            "explicit" | "e" => Some(Rating::Explicit),
            _ => None,
        }
    }
}

/// Tag category, used both for autocomplete coloring and for grouping a post's
/// tags in the detail view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TagCategory {
    /// "general" tags — labeled simply "Tag" in ebb's UI.
    General,
    Artist,
    Copyright,
    Character,
    Species,
    Metadata,
    Lore,
    Invalid,
    Unknown,
}

/// The fixed palette ebb uses for tag categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TagColor {
    Blue,
    Orange,
    Purple,
    Green,
    Yellow,
    Gray,
}

impl TagCategory {
    /// Map the numeric `category` field Danbooru/e621 use in tag responses.
    pub fn from_numeric(category: i64) -> TagCategory {
        match category {
            0 => TagCategory::General,
            1 => TagCategory::Artist,
            3 => TagCategory::Copyright,
            4 => TagCategory::Character,
            5 => TagCategory::Metadata,
            _ => TagCategory::Unknown,
        }
    }

    /// Display label for a tag group heading (ebb names the general group "Tag").
    pub fn label(self) -> &'static str {
        match self {
            TagCategory::General => "Tag",
            TagCategory::Artist => "Artist",
            TagCategory::Copyright => "Copyright",
            TagCategory::Character => "Character",
            TagCategory::Species => "Species",
            TagCategory::Metadata => "Metadata",
            TagCategory::Lore => "Lore",
            TagCategory::Invalid => "Invalid",
            TagCategory::Unknown => "Unknown",
        }
    }

    /// Color shown for this category — matches ebb's `getCategoryColor`.
    pub fn color(self) -> TagColor {
        match self {
            TagCategory::General => TagColor::Blue,
            TagCategory::Artist => TagColor::Orange,
            TagCategory::Copyright => TagColor::Purple,
            TagCategory::Character => TagColor::Green,
            TagCategory::Metadata => TagColor::Yellow,
            TagCategory::Species
            | TagCategory::Lore
            | TagCategory::Invalid
            | TagCategory::Unknown => TagColor::Gray,
        }
    }
}

/// A tag suggestion returned by autocomplete.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BooruTag {
    /// Display text (may differ from `value`, e.g. shows an alias's target).
    pub label: String,
    /// The actual tag inserted into the search query.
    pub value: String,
    pub category: TagCategory,
    pub post_count: Option<u32>,
}

/// A post's tags, grouped by category for the detail view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagGroup {
    pub category: TagCategory,
    pub tags: Vec<String>,
}

/// A normalized post, identical in shape across every provider.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BooruPost {
    pub id: String,
    /// URL of the post's page on the booru site.
    pub post_view: String,
    /// All tags flattened into one list, for search interactions.
    pub tags: Vec<String>,
    /// Tags grouped by category for display. Empty when the provider doesn't
    /// supply category info inline (e.g. Gelbooru needs a separate fetch via
    /// [`crate::booru::BooruClient::get_tag_groups`]).
    pub tag_groups: Vec<TagGroup>,
    pub file_url: String,
    pub preview_url: String,
    pub sample_url: Option<String>,
    pub width: u32,
    pub height: u32,
    /// Lowercase rating string (e.g. "general"). Parse with [`Rating::from_loose`].
    pub rating: String,
    /// Creation time, normalized to an RFC3339 UTC string when parseable.
    pub created_at: String,
}

/// Parameters for a post search.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PostQuery {
    /// Space-separated tag query.
    pub tags: String,
    pub limit: u32,
    /// Zero-indexed page (providers translate this to their own scheme).
    pub page: u32,
    pub rating: Option<Rating>,
}

impl PostQuery {
    /// Build the effective tag string, appending `rating:<x>` when a rating
    /// filter is set — exactly as ebb's `buildPostRequest` does.
    pub fn effective_tags(&self) -> String {
        match self.rating {
            Some(r) => {
                if self.tags.is_empty() {
                    format!("rating:{}", r.query_value())
                } else {
                    format!("{} rating:{}", self.tags, r.query_value())
                }
            }
            None => self.tags.clone(),
        }
    }
}

/// A page of search results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostsPage {
    pub posts: Vec<BooruPost>,
    pub has_next_page: bool,
}

/// Lowercase file extension of a URL, ignoring any query string.
fn url_extension(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
}

/// Whether a URL points at a displayable still image (ebb's `isImage`).
pub fn is_image(url: &str) -> bool {
    matches!(
        url_extension(url).as_deref(),
        Some("jpg" | "jpeg" | "png" | "gif" | "webp")
    )
}

/// Whether a URL points at a video file (webm/mp4).
pub fn is_video(url: &str) -> bool {
    matches!(url_extension(url).as_deref(), Some("webm" | "mp4"))
}

/// Whether a URL points at a Flash movie.
pub fn is_swf(url: &str) -> bool {
    matches!(url_extension(url).as_deref(), Some("swf"))
}

/// Normalize a timestamp to an RFC3339 UTC string (like JS `Date.toISOString()`),
/// falling back to the raw input when it can't be parsed.
pub fn normalize_timestamp(raw: &str) -> String {
    use chrono::{DateTime, SecondsFormat, Utc};
    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return dt
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true);
    }
    // Gelbooru-style ctime, e.g. "Tue May 30 12:34:56 -0500 2023".
    if let Ok(dt) = DateTime::parse_from_str(raw, "%a %b %d %H:%M:%S %z %Y") {
        return dt
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true);
    }
    raw.to_string()
}

/// Normalize a Unix timestamp (seconds) to an RFC3339 UTC string.
pub fn timestamp_from_unix(secs: i64) -> String {
    use chrono::{DateTime, SecondsFormat, Utc};
    DateTime::<Utc>::from_timestamp(secs, 0)
        .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_default()
}

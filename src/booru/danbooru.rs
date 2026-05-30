//! Danbooru provider — <https://danbooru.donmai.us>. No auth required; tag
//! groups arrive inline on each post.

use serde::Deserialize;

use super::types::{
    normalize_timestamp, BooruPost, BooruTag, PostQuery, PostsPage, Rating, TagCategory, TagGroup,
};
use super::{parse_err, split_tags, BooruClient, Provider, Result};

pub struct Danbooru;

const BASE: &str = "https://danbooru.donmai.us";

#[derive(Deserialize)]
struct RawPost {
    id: i64,
    #[serde(default)]
    image_width: u32,
    #[serde(default)]
    image_height: u32,
    #[serde(default)]
    tag_string: String,
    file_url: Option<String>,
    #[serde(default)]
    large_file_url: Option<String>,
    #[serde(default)]
    preview_file_url: Option<String>,
    #[serde(default)]
    rating: String,
    #[serde(default)]
    tag_string_general: String,
    #[serde(default)]
    tag_string_character: String,
    #[serde(default)]
    tag_string_copyright: String,
    #[serde(default)]
    tag_string_artist: String,
    #[serde(default)]
    tag_string_meta: String,
    #[serde(default)]
    created_at: String,
}

#[derive(Deserialize)]
struct RawTag {
    label: String,
    value: String,
    #[serde(default)]
    category: i64,
    #[serde(default)]
    post_count: u32,
}

impl Provider for Danbooru {
    fn tag_request(&self, client: &BooruClient, query: &str) -> Option<reqwest::RequestBuilder> {
        Some(
            client
                .http()
                .get(format!("{BASE}/autocomplete.json"))
                .query(&[
                    ("search[query]", query),
                    ("search[type]", "tag_query"),
                    ("limit", "10"),
                ]),
        )
    }

    fn parse_tags(&self, body: &str) -> Result<Vec<BooruTag>> {
        let raw: Vec<RawTag> = serde_json::from_str(body).map_err(parse_err)?;
        Ok(raw
            .into_iter()
            .map(|t| BooruTag {
                label: t.label,
                value: t.value,
                category: TagCategory::from_numeric(t.category),
                post_count: Some(t.post_count),
            })
            .collect())
    }

    fn post_request(&self, client: &BooruClient, query: &PostQuery) -> reqwest::RequestBuilder {
        client.http().get(format!("{BASE}/posts.json")).query(&[
            ("tags", query.effective_tags()),
            ("limit", query.limit.to_string()),
            // Danbooru pages are 1-indexed.
            ("page", (query.page + 1).to_string()),
        ])
    }

    fn parse_posts(&self, body: &str) -> Result<PostsPage> {
        let raw: Vec<RawPost> = serde_json::from_str(body).map_err(parse_err)?;
        let posts: Vec<BooruPost> = raw
            .into_iter()
            .filter_map(|p| {
                let file_url = p.file_url?;
                let rating = Rating::from_alias(&p.rating)
                    .map(|r| r.as_lower().to_string())
                    .unwrap_or_else(|| p.rating.to_ascii_lowercase());
                let tag_groups = [
                    (TagCategory::General, &p.tag_string_general),
                    (TagCategory::Character, &p.tag_string_character),
                    (TagCategory::Copyright, &p.tag_string_copyright),
                    (TagCategory::Artist, &p.tag_string_artist),
                    (TagCategory::Metadata, &p.tag_string_meta),
                ]
                .into_iter()
                .filter_map(|(category, s)| {
                    let tags = split_tags(s);
                    (!tags.is_empty()).then_some(TagGroup { category, tags })
                })
                .collect();
                Some(BooruPost {
                    id: p.id.to_string(),
                    post_view: format!("{BASE}/posts/{}", p.id),
                    tags: split_tags(&p.tag_string),
                    tag_groups,
                    file_url,
                    preview_url: p.preview_file_url.unwrap_or_default(),
                    sample_url: p.large_file_url.filter(|s| !s.is_empty()),
                    width: p.image_width,
                    height: p.image_height,
                    rating,
                    created_at: normalize_timestamp(&p.created_at),
                })
            })
            .collect();
        let has_next_page = !posts.is_empty();
        Ok(PostsPage {
            posts,
            has_next_page,
        })
    }
}

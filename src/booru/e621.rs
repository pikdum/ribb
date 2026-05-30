//! e621 provider — <https://e621.net>. No auth; tags arrive pre-grouped by
//! category. Autocomplete requires at least 3 characters.

use serde::Deserialize;

use super::types::{
    normalize_timestamp, BooruPost, BooruTag, PostQuery, PostsPage, Rating, TagCategory, TagGroup,
};
use super::{parse_err, BooruClient, Provider, Result};

pub struct E621;

const BASE: &str = "https://e621.net";

#[derive(Deserialize)]
struct RawResponse {
    #[serde(default)]
    posts: Vec<RawPost>,
}

#[derive(Deserialize)]
struct RawPost {
    id: i64,
    file: RawFile,
    #[serde(default)]
    preview: RawUrl,
    #[serde(default)]
    sample: RawSample,
    #[serde(default)]
    tags: RawTags,
    #[serde(default)]
    rating: String,
    #[serde(default)]
    created_at: String,
}

#[derive(Deserialize)]
struct RawFile {
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    url: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawUrl {
    #[serde(default)]
    url: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawSample {
    #[serde(default)]
    has: bool,
    #[serde(default)]
    url: Option<String>,
}

#[derive(Deserialize, Default)]
struct RawTags {
    #[serde(default)]
    general: Vec<String>,
    #[serde(default)]
    artist: Vec<String>,
    #[serde(default)]
    copyright: Vec<String>,
    #[serde(default)]
    character: Vec<String>,
    #[serde(default)]
    species: Vec<String>,
    #[serde(default)]
    invalid: Vec<String>,
    #[serde(default)]
    meta: Vec<String>,
    #[serde(default)]
    lore: Vec<String>,
}

#[derive(Deserialize)]
struct RawTag {
    name: String,
    #[serde(default)]
    post_count: u32,
    #[serde(default)]
    category: i64,
}

impl Provider for E621 {
    fn tag_request(&self, client: &BooruClient, query: &str) -> Option<reqwest::RequestBuilder> {
        // e621 ignores queries shorter than 3 chars.
        if query.chars().count() < 3 {
            return None;
        }
        Some(
            client
                .http()
                .get(format!("{BASE}/tags/autocomplete.json"))
                .query(&[("search[name_matches]", query), ("expiry", "7")]),
        )
    }

    fn parse_tags(&self, body: &str) -> Result<Vec<BooruTag>> {
        let raw: Vec<RawTag> = serde_json::from_str(body).map_err(parse_err)?;
        Ok(raw
            .into_iter()
            .map(|t| BooruTag {
                label: t.name.clone(),
                value: t.name,
                category: TagCategory::from_numeric(t.category),
                post_count: Some(t.post_count),
            })
            .collect())
    }

    fn post_request(&self, client: &BooruClient, query: &PostQuery) -> reqwest::RequestBuilder {
        client.http().get(format!("{BASE}/posts.json")).query(&[
            ("tags", query.effective_tags()),
            ("limit", query.limit.to_string()),
            // e621 pages are 1-indexed.
            ("page", (query.page + 1).to_string()),
        ])
    }

    fn parse_posts(&self, body: &str) -> Result<PostsPage> {
        let raw: RawResponse = serde_json::from_str(body).map_err(parse_err)?;
        let posts: Vec<BooruPost> = raw
            .posts
            .into_iter()
            .filter_map(|p| {
                let file_url = p.file.url?;
                let rating = Rating::from_alias(&p.rating)
                    .map(|r| r.as_lower().to_string())
                    .unwrap_or_else(|| p.rating.to_ascii_lowercase());
                let groups = [
                    (TagCategory::General, p.tags.general),
                    (TagCategory::Artist, p.tags.artist),
                    (TagCategory::Copyright, p.tags.copyright),
                    (TagCategory::Character, p.tags.character),
                    (TagCategory::Species, p.tags.species),
                    (TagCategory::Invalid, p.tags.invalid),
                    (TagCategory::Metadata, p.tags.meta),
                    (TagCategory::Lore, p.tags.lore),
                ];
                let tags: Vec<String> = groups.iter().flat_map(|(_, t)| t.clone()).collect();
                let tag_groups = groups
                    .into_iter()
                    .filter_map(|(category, tags)| {
                        (!tags.is_empty()).then_some(TagGroup { category, tags })
                    })
                    .collect();
                Some(BooruPost {
                    id: p.id.to_string(),
                    post_view: format!("{BASE}/posts/{}", p.id),
                    tags,
                    tag_groups,
                    file_url,
                    preview_url: p.preview.url.unwrap_or_default(),
                    sample_url: if p.sample.has { p.sample.url } else { None },
                    width: p.file.width,
                    height: p.file.height,
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

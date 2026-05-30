//! Gelbooru provider — <https://gelbooru.com>. Requires API credentials (set on
//! the [`BooruClient`]). Tag names come back double-HTML-encoded, and category
//! groups need a separate fetch.

use serde::Deserialize;

use super::types::{
    normalize_timestamp, BooruPost, BooruTag, PostQuery, PostsPage, TagCategory, TagGroup,
};
use super::{parse_err, BooruClient, Provider, Result};

pub struct Gelbooru;

#[derive(Deserialize)]
struct RawResponse {
    #[serde(default)]
    post: Vec<RawPost>,
    #[serde(rename = "@attributes")]
    attributes: RawAttributes,
}

#[derive(Deserialize, Default)]
struct RawAttributes {
    #[serde(default)]
    limit: i64,
    #[serde(default)]
    offset: i64,
    #[serde(default)]
    count: i64,
}

#[derive(Deserialize)]
struct RawPost {
    id: i64,
    #[serde(default)]
    width: u32,
    #[serde(default)]
    height: u32,
    #[serde(default)]
    tags: String,
    #[serde(default)]
    file_url: String,
    #[serde(default)]
    preview_url: String,
    #[serde(default)]
    sample_url: String,
    #[serde(default)]
    rating: String,
    #[serde(default)]
    created_at: String,
}

#[derive(Deserialize)]
struct RawTag {
    label: String,
    value: String,
    #[serde(default)]
    category: String,
    #[serde(default)]
    post_count: String,
}

#[derive(Deserialize)]
struct RawTagGroupResponse {
    #[serde(default)]
    tag: Vec<RawTagGroupEntry>,
}

#[derive(Deserialize)]
struct RawTagGroupEntry {
    name: String,
    #[serde(default)]
    r#type: i64,
}

/// Category color for Gelbooru's string-typed autocomplete categories.
fn category_from_str(category: &str) -> TagCategory {
    match category {
        "tag" => TagCategory::General,
        "artist" => TagCategory::Artist,
        "copyright" => TagCategory::Copyright,
        "character" => TagCategory::Character,
        "metadata" => TagCategory::Metadata,
        _ => TagCategory::Unknown,
    }
}

impl Provider for Gelbooru {
    fn tag_request(&self, client: &BooruClient, query: &str) -> Option<reqwest::RequestBuilder> {
        Some(client.http().get("https://gelbooru.com/index.php").query(&[
            ("page", "autocomplete2"),
            ("term", query),
            ("type", "tag_query"),
            ("limit", "10"),
        ]))
    }

    fn parse_tags(&self, body: &str) -> Result<Vec<BooruTag>> {
        let raw: Vec<RawTag> = serde_json::from_str(body).map_err(parse_err)?;
        Ok(raw
            .into_iter()
            .map(|t| BooruTag {
                label: t.label,
                value: t.value,
                category: category_from_str(&t.category),
                post_count: t.post_count.parse().ok(),
            })
            .collect())
    }

    fn post_request(&self, client: &BooruClient, query: &PostQuery) -> reqwest::RequestBuilder {
        // Credentials are appended verbatim to the base URL, as ebb does.
        let base = format!(
            "https://gelbooru.com/index.php?page=dapi&s=post&q=index&json=1{}",
            client.gelbooru_credentials()
        );
        client.http().get(base).query(&[
            ("tags", query.effective_tags()),
            // Gelbooru pages are 0-indexed (`pid`).
            ("pid", query.page.to_string()),
            ("limit", query.limit.to_string()),
        ])
    }

    fn parse_posts(&self, body: &str) -> Result<PostsPage> {
        let raw: RawResponse = serde_json::from_str(body).map_err(parse_err)?;
        let a = &raw.attributes;
        let has_next_page = a.count > a.limit + a.offset;
        let posts = raw
            .post
            .into_iter()
            .map(|p| BooruPost {
                id: p.id.to_string(),
                post_view: format!(
                    "https://gelbooru.com/index.php?page=post&s=view&id={}",
                    p.id
                ),
                tags: p.tags.split_whitespace().map(str::to_string).collect(),
                // Filled in by a separate request; see `tag_groups_request`.
                tag_groups: Vec::new(),
                file_url: p.file_url,
                preview_url: p.preview_url,
                sample_url: Some(p.sample_url).filter(|s| !s.is_empty()),
                width: p.width,
                height: p.height,
                rating: p.rating,
                created_at: normalize_timestamp(&p.created_at),
            })
            .collect();
        Ok(PostsPage {
            posts,
            has_next_page,
        })
    }

    fn tag_groups_request(
        &self,
        client: &BooruClient,
        post: &BooruPost,
    ) -> Option<reqwest::RequestBuilder> {
        let base = format!(
            "https://gelbooru.com/index.php?page=dapi&s=tag&q=index&json=1&orderby=name&order=asc{}",
            client.gelbooru_credentials()
        );
        Some(
            client
                .http()
                .get(base)
                .query(&[("names", post.tags.join(" "))]),
        )
    }

    fn parse_tag_groups(&self, body: &str) -> Result<Vec<TagGroup>> {
        let raw: RawTagGroupResponse = serde_json::from_str(body).map_err(parse_err)?;
        let mut groups: Vec<TagGroup> = Vec::new();
        for entry in raw.tag {
            let category = TagCategory::from_numeric(entry.r#type);
            // Names are double-encoded; decode once for consistency.
            let name = html_escape::decode_html_entities(&entry.name).into_owned();
            match groups.iter_mut().find(|g| g.category == category) {
                Some(g) => g.tags.push(name),
                None => groups.push(TagGroup {
                    category,
                    tags: vec![name],
                }),
            }
        }
        Ok(groups)
    }
}

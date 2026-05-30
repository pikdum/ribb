//! Rule 34 provider — <https://rule34.xxx>. Disabled in ebb's UI (see
//! [`super::Site::enabled`]) but kept for completeness. No auth.

use serde::Deserialize;

use super::types::{timestamp_from_unix, BooruPost, BooruTag, PostQuery, PostsPage, TagCategory};
use super::{parse_err, BooruClient, Provider, Result};

pub struct Rule34;

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
    change: i64,
}

#[derive(Deserialize)]
struct RawTag {
    label: String,
    value: String,
    #[serde(default)]
    r#type: String,
}

fn category_from_str(category: &str) -> TagCategory {
    match category {
        "general" => TagCategory::General,
        "artist" => TagCategory::Artist,
        "copyright" => TagCategory::Copyright,
        "character" => TagCategory::Character,
        "metadata" => TagCategory::Metadata,
        _ => TagCategory::Unknown,
    }
}

/// Extract the post count from an autocomplete label like `"samus_aran (1234)"`.
fn count_from_label(label: &str) -> Option<u32> {
    let start = label.rfind('(')?;
    let rest = &label[start + 1..];
    let end = rest.find(')')?;
    rest[..end].trim().parse().ok()
}

impl Provider for Rule34 {
    fn tag_request(&self, client: &BooruClient, query: &str) -> Option<reqwest::RequestBuilder> {
        Some(
            client
                .http()
                .get("https://ac.rule34.xxx/autocomplete.php")
                .query(&[("q", query)]),
        )
    }

    fn parse_tags(&self, body: &str) -> Result<Vec<BooruTag>> {
        let raw: Vec<RawTag> = serde_json::from_str(body).map_err(parse_err)?;
        Ok(raw
            .into_iter()
            .map(|t| {
                let post_count = count_from_label(&t.label);
                let value = html_escape::decode_html_entities(&t.value).into_owned();
                BooruTag {
                    label: value.clone(),
                    value,
                    category: category_from_str(&t.r#type),
                    post_count,
                }
            })
            .collect())
    }

    fn post_request(&self, client: &BooruClient, query: &PostQuery) -> reqwest::RequestBuilder {
        client
            .http()
            .get("https://api.rule34.xxx/index.php?page=dapi&s=post&q=index&json=1")
            .query(&[
                ("tags", query.effective_tags()),
                // Rule34 pages are 0-indexed (`pid`).
                ("pid", query.page.to_string()),
                ("limit", query.limit.to_string()),
            ])
    }

    fn parse_posts(&self, body: &str) -> Result<PostsPage> {
        let raw: Vec<RawPost> = serde_json::from_str(body).map_err(parse_err)?;
        let has_next_page = !raw.is_empty();
        let posts = raw
            .into_iter()
            .map(|p| {
                // Avoid .gif samples — fall back to the preview, as ebb does.
                let sample_url = if p.sample_url.ends_with(".gif") {
                    p.preview_url.clone()
                } else {
                    p.sample_url.clone()
                };
                BooruPost {
                    id: p.id.to_string(),
                    post_view: format!("https://rule34.xxx/index.php?page=post&s=view&id={}", p.id),
                    tags: p.tags.split_whitespace().map(str::to_string).collect(),
                    tag_groups: Vec::new(),
                    file_url: p.file_url,
                    preview_url: p.preview_url,
                    sample_url: Some(sample_url).filter(|s| !s.is_empty()),
                    width: p.width,
                    height: p.height,
                    rating: p.rating,
                    created_at: timestamp_from_unix(p.change),
                }
            })
            .collect();
        Ok(PostsPage {
            posts,
            has_next_page,
        })
    }
}

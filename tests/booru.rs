//! Booru layer tests.
//!
//! Offline tests exercise each provider's pure `parse_*` methods against canned
//! JSON (no network, no auth). Live tests hit Danbooru only — it needs no auth
//! and returns tag groups inline, so it exercises the full path end to end.

use ribb::booru::{BooruClient, PostQuery, Site, TagCategory};

// ---------------------------------------------------------------------------
// Offline parse tests (all providers)
// ---------------------------------------------------------------------------

#[test]
fn danbooru_parse_posts() {
    let body = r#"[
        {
            "id": 123,
            "image_width": 800, "image_height": 600,
            "tag_string": "1girl solo blue_sky artist_name",
            "file_url": "https://cdn.donmai.us/original/abcd.jpg",
            "large_file_url": "https://cdn.donmai.us/sample/sample-abcd.jpg",
            "preview_file_url": "https://cdn.donmai.us/preview/abcd.jpg",
            "rating": "s",
            "tag_string_general": "1girl solo blue_sky",
            "tag_string_character": "hatsune_miku",
            "tag_string_copyright": "vocaloid",
            "tag_string_artist": "artist_name",
            "tag_string_meta": "highres",
            "created_at": "2023-05-30T12:34:56.789-04:00"
        },
        { "id": 124, "file_url": null, "tag_string": "x", "rating": "e",
          "created_at": "2023-01-01T00:00:00.000Z" }
    ]"#;
    let page = Site::Danbooru.provider().parse_posts(body).unwrap();

    // Second post has no file_url and is filtered out.
    assert_eq!(page.posts.len(), 1);
    assert!(page.has_next_page);

    let p = &page.posts[0];
    assert_eq!(p.id, "123");
    assert_eq!(p.post_view, "https://danbooru.donmai.us/posts/123");
    assert_eq!(p.tags, ["1girl", "solo", "blue_sky", "artist_name"]);
    assert_eq!(
        p.sample_url.as_deref(),
        Some("https://cdn.donmai.us/sample/sample-abcd.jpg")
    );
    assert_eq!(p.width, 800);
    assert_eq!(p.rating, "sensitive");
    // -04:00 normalized to UTC, with trailing Z.
    assert_eq!(p.created_at, "2023-05-30T16:34:56.789Z");

    let groups: Vec<_> = p
        .tag_groups
        .iter()
        .map(|g| (g.category, g.tags.clone()))
        .collect();
    assert_eq!(
        groups,
        vec![
            (
                TagCategory::General,
                vec!["1girl".into(), "solo".into(), "blue_sky".into()]
            ),
            (TagCategory::Character, vec!["hatsune_miku".to_string()]),
            (TagCategory::Copyright, vec!["vocaloid".to_string()]),
            (TagCategory::Artist, vec!["artist_name".to_string()]),
            (TagCategory::Metadata, vec!["highres".to_string()]),
        ]
    );
}

#[test]
fn danbooru_parse_tags() {
    let body = r#"[
        {"label":"1girl","value":"1girl","category":0,"post_count":1000},
        {"label":"hatsune_miku","value":"hatsune_miku","category":4,"post_count":500}
    ]"#;
    let tags = Site::Danbooru.provider().parse_tags(body).unwrap();
    assert_eq!(tags.len(), 2);
    assert_eq!(tags[0].value, "1girl");
    assert_eq!(tags[0].category, TagCategory::General);
    assert_eq!(tags[0].post_count, Some(1000));
    assert_eq!(tags[1].category, TagCategory::Character);
}

#[test]
fn e621_parse_posts() {
    let body = r#"{"posts":[
        {"id":1,"file":{"width":100,"height":200,"ext":"jpg","url":"https://static1.e621.net/a.jpg"},
         "preview":{"url":"https://static1.e621.net/p.jpg"},
         "sample":{"has":true,"url":"https://static1.e621.net/s.jpg"},
         "tags":{"general":["a","b"],"artist":["artistx"],"copyright":[],"character":["charx"],
                 "species":["wolf"],"invalid":[],"meta":["hi"],"lore":[]},
         "rating":"e","created_at":"2022-02-02T02:02:02.000Z"},
        {"id":2,"file":{"width":0,"height":0,"ext":"webm","url":null},"preview":{"url":""},
         "sample":{"has":false},"tags":{},"rating":"s","created_at":"2022-01-01T00:00:00.000Z"}
    ]}"#;
    let page = Site::E621.provider().parse_posts(body).unwrap();
    assert_eq!(page.posts.len(), 1); // null file.url filtered out

    let p = &page.posts[0];
    assert_eq!(p.tags, ["a", "b", "artistx", "charx", "wolf", "hi"]);
    assert_eq!(
        p.sample_url.as_deref(),
        Some("https://static1.e621.net/s.jpg")
    );
    assert_eq!(p.rating, "explicit");
    let cats: Vec<_> = p.tag_groups.iter().map(|g| g.category).collect();
    assert_eq!(
        cats,
        vec![
            TagCategory::General,
            TagCategory::Artist,
            TagCategory::Character,
            TagCategory::Species,
            TagCategory::Metadata,
        ]
    );
}

#[test]
fn gelbooru_parse_posts_and_pagination() {
    let body = r#"{"@attributes":{"limit":100,"offset":0,"count":250},"post":[
        {"id":5,"width":10,"height":20,"tags":"cat dog",
         "file_url":"https://img.gelbooru.com/f.jpg",
         "preview_url":"https://img.gelbooru.com/p.jpg",
         "sample_url":"https://img.gelbooru.com/s.jpg",
         "rating":"general","created_at":"Tue May 30 12:34:56 -0500 2023"}
    ]}"#;
    let page = Site::Gelbooru.provider().parse_posts(body).unwrap();
    assert!(page.has_next_page); // 250 > 100 + 0
    assert_eq!(page.posts.len(), 1);
    let p = &page.posts[0];
    assert_eq!(p.tags, ["cat", "dog"]);
    assert!(p.tag_groups.is_empty()); // needs a separate fetch
    assert_eq!(p.created_at, "2023-05-30T17:34:56.000Z"); // ctime -> UTC
}

#[test]
fn gelbooru_parse_posts_empty() {
    let body = r#"{"@attributes":{"limit":100,"offset":0,"count":0}}"#;
    let page = Site::Gelbooru.provider().parse_posts(body).unwrap();
    assert!(page.posts.is_empty());
    assert!(!page.has_next_page);
}

#[test]
fn gelbooru_parse_tag_groups_decodes_once() {
    let body = r#"{"@attributes":{},"tag":[
        {"id":1,"name":"cat","count":1,"type":0,"ambiguous":0},
        {"id":2,"name":"artist&amp;name","count":1,"type":1}
    ]}"#;
    let groups = Site::Gelbooru.provider().parse_tag_groups(body).unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[0].category, TagCategory::General);
    assert_eq!(groups[0].tags, ["cat"]);
    assert_eq!(groups[1].category, TagCategory::Artist);
    assert_eq!(groups[1].tags, ["artist&name"]); // decoded once
}

#[test]
fn rule34_parse_posts_avoids_gif_sample() {
    let body = r#"[
        {"id":9,"width":1,"height":2,"tags":"a b",
         "file_url":"https://rule34.xxx/f.jpg",
         "preview_url":"https://rule34.xxx/p.jpg",
         "sample_url":"https://rule34.xxx/s.gif",
         "rating":"explicit","change":1685458496}
    ]"#;
    let page = Site::Rule34.provider().parse_posts(body).unwrap();
    assert_eq!(page.posts.len(), 1);
    let p = &page.posts[0];
    // .gif sample replaced by the preview.
    assert_eq!(p.sample_url.as_deref(), Some("https://rule34.xxx/p.jpg"));
    assert!(p.created_at.ends_with('Z'));
}

#[test]
fn rule34_parse_tags_extracts_count() {
    let body = r#"[{"label":"samus_aran (1234)","value":"samus_aran","type":"character"}]"#;
    let tags = Site::Rule34.provider().parse_tags(body).unwrap();
    assert_eq!(tags.len(), 1);
    assert_eq!(tags[0].value, "samus_aran");
    assert_eq!(tags[0].category, TagCategory::Character);
    assert_eq!(tags[0].post_count, Some(1234));
}

// ---------------------------------------------------------------------------
// Live network tests (Danbooru only — no auth)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn danbooru_live_get_posts_and_tag_groups() {
    let client = BooruClient::new().unwrap();
    let query = PostQuery {
        tags: String::new(),
        limit: 3,
        page: 0,
        rating: None,
    };
    let page = client.get_posts(Site::Danbooru, &query).await.unwrap();
    assert!(!page.posts.is_empty(), "expected at least one post");

    let post = &page.posts[0];
    assert!(post.created_at.ends_with('Z'));
    assert!(!post.file_url.is_empty());

    // Danbooru supplies tag groups inline, so this needs no extra request.
    let groups = client.get_tag_groups(Site::Danbooru, post).await.unwrap();
    assert_eq!(groups, post.tag_groups);
}

#[tokio::test]
async fn danbooru_live_get_tags() {
    let client = BooruClient::new().unwrap();
    let tags = client.get_tags(Site::Danbooru, "land").await.unwrap();
    assert!(!tags.is_empty(), "expected autocomplete results for 'land'");
}

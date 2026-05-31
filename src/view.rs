// This file is `include!`d into `app.rs`, so it shares its imports and module
// scope. It holds the view tree — ebb's component render functions.

type El<'a> = Element<'a, Message>;

// ----- small style helpers -------------------------------------------------

/// A solid colored button (white text, rounded), with a hover/pressed shade.
fn solid(
    bg: Color,
    hover: Color,
) -> impl Fn(&Theme, iced::widget::button::Status) -> iced::widget::button::Style {
    move |_theme, status| {
        let background = match status {
            iced::widget::button::Status::Hovered | iced::widget::button::Status::Pressed => hover,
            _ => bg,
        };
        iced::widget::button::Style {
            background: Some(background.into()),
            text_color: style::WHITE,
            border: iced::border::rounded(6.0),
            ..iced::widget::button::Style::default()
        }
    }
}

/// A borderless, transparent button (icons in the tab bar).
fn ghost(_theme: &Theme, status: iced::widget::button::Status) -> iced::widget::button::Style {
    let background = match status {
        iced::widget::button::Status::Hovered | iced::widget::button::Status::Pressed => {
            Some(style::INDIGO_300.into())
        }
        _ => None,
    };
    iced::widget::button::Style {
        background,
        text_color: style::BLACK,
        border: iced::border::rounded(999.0),
        ..iced::widget::button::Style::default()
    }
}

fn active_tab_label(
    _theme: &Theme,
    status: iced::widget::button::Status,
) -> iced::widget::button::Style {
    let background = match status {
        iced::widget::button::Status::Pressed => Some(style::INDIGO_600.into()),
        _ => None,
    };
    iced::widget::button::Style {
        background,
        text_color: style::WHITE,
        border: iced::border::rounded(6.0),
        ..iced::widget::button::Style::default()
    }
}

fn active_tab_close(
    _theme: &Theme,
    status: iced::widget::button::Status,
) -> iced::widget::button::Style {
    let background = match status {
        iced::widget::button::Status::Hovered | iced::widget::button::Status::Pressed => {
            Some(style::INDIGO_600.into())
        }
        _ => None,
    };
    iced::widget::button::Style {
        background,
        text_color: style::WHITE,
        border: iced::border::rounded(999.0),
        ..iced::widget::button::Style::default()
    }
}

/// A solid colored pill button (fully rounded), with a hover/pressed shade.
fn pill(
    bg: Color,
    hover: Color,
) -> impl Fn(&Theme, iced::widget::button::Status) -> iced::widget::button::Style {
    move |_theme, status| {
        let background = match status {
            iced::widget::button::Status::Hovered | iced::widget::button::Status::Pressed => hover,
            _ => bg,
        };
        iced::widget::button::Style {
            background: Some(background.into()),
            text_color: style::WHITE,
            border: iced::border::rounded(999.0),
            ..iced::widget::button::Style::default()
        }
    }
}

/// Container background fill.
fn bg(color: Color) -> impl Fn(&Theme) -> iced::widget::container::Style {
    move |_theme| iced::widget::container::Style {
        background: Some(color.into()),
        ..iced::widget::container::Style::default()
    }
}

/// Container with a rounded, colored background (chips/badges).
fn rounded_bg(color: Color, radius: f32) -> impl Fn(&Theme) -> iced::widget::container::Style {
    move |_theme| iced::widget::container::Style {
        background: Some(color.into()),
        border: iced::border::rounded(radius),
        ..iced::widget::container::Style::default()
    }
}

/// White rounded card (settings modal).
fn card(_theme: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(style::WHITE.into()),
        text_color: Some(style::BLACK),
        border: iced::border::rounded(10.0),
        ..iced::widget::container::Style::default()
    }
}

/// Dimmed full-screen backdrop behind a modal.
fn backdrop(_theme: &Theme) -> iced::widget::container::Style {
    iced::widget::container::Style {
        background: Some(Color { a: 0.5, ..style::BLACK }.into()),
        ..iced::widget::container::Style::default()
    }
}

fn colored(
    content: impl iced::widget::text::IntoFragment<'static>,
    color: Color,
) -> iced::widget::Text<'static> {
    text(content).color(color)
}

/// Format an RFC3339 timestamp as e.g. "May 30, 2026".
fn format_date(raw: &str) -> String {
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|dt| dt.format("%B %-d, %Y").to_string())
        .unwrap_or_else(|_| raw.to_string())
}

/// Humanize a post count, e.g. 1234 -> "1.2k", 2_500_000 -> "2.5m".
fn format_count(n: u32) -> String {
    let n = n as f64;
    if n >= 1_000_000.0 {
        format!("{:.1}m", n / 1_000_000.0)
    } else if n >= 1_000.0 {
        format!("{:.1}k", n / 1_000.0)
    } else {
        format!("{n:.0}")
    }
}

/// A small gray pill used as a group/field label.
fn label_chip(s: &str) -> El<'static> {
    container(text(s.to_string()).size(13).color(style::BLACK))
        .padding([3, 12])
        .style(rounded_bg(style::GRAY_200, 4.0))
        .into()
}

/// Rough rendered width of a chip with the given label text (size-13 font plus
/// horizontal padding). Used to pack chips into width-aware rows.
fn chip_width(text: &str) -> f32 {
    text.chars().count() as f32 * 7.5 + 30.0
}

/// Pack pre-measured chips into left-aligned rows that wrap at `max_w`.
fn flow_left(items: Vec<(f32, El<'static>)>, max_w: f32) -> El<'static> {
    let gap = 4.0;
    let mut rows: Vec<El<'static>> = Vec::new();
    let mut current: Vec<El<'static>> = Vec::new();
    let mut width = 0.0;
    for (w, el) in items {
        let add = if current.is_empty() { w } else { w + gap };
        if !current.is_empty() && width + add > max_w {
            rows.push(
                Row::with_children(std::mem::take(&mut current))
                    .spacing(gap)
                    .align_y(Center)
                    .into(),
            );
            width = 0.0;
        }
        width += if current.is_empty() { w } else { w + gap };
        current.push(el);
    }
    if !current.is_empty() {
        rows.push(Row::with_children(current).spacing(gap).align_y(Center).into());
    }
    Column::with_children(rows).spacing(4).align_x(iced::Left).into()
}

/// Fit `(iw, ih)` within `max_w` × `max_h`, preserving aspect ratio and never
/// upscaling past the image's natural size (ebb's `max-w-full` / `max-h`).
/// Falls back to filling the width when dims are unknown.
fn render_size(iw: u32, ih: u32, max_w: f32, max_h: f32) -> (f32, f32) {
    if iw == 0 || ih == 0 {
        return (max_w, max_h);
    }
    let mut w = iw as f32;
    let mut h = ih as f32;
    // Only ever scale down — to fit the width, then the height.
    if w > max_w {
        let s = max_w / w;
        w *= s;
        h *= s;
    }
    if h > max_h {
        let s = max_h / h;
        w *= s;
        h *= s;
    }
    // Don't floor — truncating the fractional viewport height leaves the image
    // ~1px short of filling it.
    (w, h)
}

// ----- views ----------------------------------------------------------------

impl Ribb {
    pub fn view(&self) -> El<'_> {
        let base: El<'_> = column![self.tab_bar(), self.tab_view(self.active_tab())].into();
        // Always render the stack (with an empty top layer when the modal is
        // closed) so the base subtree keeps its position in the widget tree —
        // otherwise toggling the modal resets the scrollable's offset to top.
        let overlay: El<'_> = if self.settings_open {
            self.settings_modal()
        } else {
            Space::new()
                .width(Length::Fixed(0.0))
                .height(Length::Fixed(0.0))
                .into()
        };
        stack![base, overlay].into()
    }

    fn tab_bar(&self) -> El<'_> {
        let mut bar = Row::new().spacing(8).align_y(Center);
        for (i, tab) in self.tabs.iter().enumerate() {
            let active = i == self.active;
            if active && self.tabs.len() > 1 {
                let chip = row![
                    button(text(tab.title.clone()).size(13).color(style::WHITE))
                        .padding(iced::Padding {
                            top: 5.0,
                            right: 4.0,
                            bottom: 5.0,
                            left: 8.0,
                        })
                        .on_press(Message::SelectTab(i))
                        .style(active_tab_label),
                    button(
                        container(icon_x().size(13).color(style::WHITE))
                            .center_x(Length::Fixed(20.0))
                            .center_y(Length::Fixed(20.0)),
                    )
                    .padding(0)
                    .width(Length::Fixed(20.0))
                    .height(Length::Fixed(20.0))
                        .on_press(Message::CloseTab(tab.id))
                        .style(active_tab_close),
                    Space::new().width(Length::Fixed(4.0)),
                ]
                .spacing(0)
                .align_y(Center);
                bar = bar.push(container(chip).style(rounded_bg(style::INDIGO_500, 6.0)));
            } else {
                let (fill, hover) = if active {
                    (style::INDIGO_500, style::INDIGO_600)
                } else {
                    (style::GRAY_300, style::GRAY_300)
                };
                bar = bar.push(
                    button(
                        text(tab.title.clone())
                            .size(13)
                            .color(if active { style::WHITE } else { style::BLACK }),
                    )
                    .padding([5, 10])
                    .on_press(Message::SelectTab(i))
                    .style(solid(fill, hover)),
                );
            }
        }
        bar = bar
            .push(button(icon_plus().size(16)).on_press(Message::NewTab).style(ghost))
            .push(Space::new().width(Length::Fill))
            .push(button(icon_settings().size(16)).on_press(Message::OpenSettings).style(ghost));

        container(bar)
            .padding(8)
            .width(Length::Fill)
            .style(bg(style::GRAY_100))
            .into()
    }

    fn tab_view<'a>(&'a self, tab: &'a Tab) -> El<'a> {
        // The header stays fixed (ebb's `sticky`); only the body scrolls.

        // Subtle separator under the sticky header (ebb's border-b).
        let divider = iced::widget::rule::horizontal(1).style(|_theme| iced::widget::rule::Style {
            color: style::GRAY_200,
            radius: 0.0.into(),
            fill_mode: iced::widget::rule::FillMode::Full,
            snap: true,
        });

        // `responsive` measures the exact space left for the scroll area (the
        // content viewport), so the expanded image can fill it precisely
        // instead of us guessing the chrome height.
        let area = responsive(move |viewport| self.scroll_area(tab, viewport.height));

        column![self.header(tab), divider, area]
            .height(Length::Fill)
            .into()
    }

    fn scroll_area<'a>(&'a self, tab: &'a Tab, viewport_h: f32) -> El<'a> {
        let mut body = Column::new();

        if tab.loading {
            body = body.push(
                container(text("Loading…").size(18).color(style::BLUE_500))
                    .padding(24)
                    .center_x(Length::Fill),
            );
        } else {
            body = body.push(self.grid(tab, viewport_h));
        }

        if let Some(err) = &tab.error {
            body = body.push(
                container(text(err.clone()))
                    .padding(20)
                    .center_x(Length::Fill),
            );
        }

        if tab.query.is_none() {
            body = body.push(self.empty_state());
        }

        scrollable(body)
            .id(tab.scroll_id.clone())
            .on_scroll(|viewport| Message::ScrollChanged {
                tab: tab.id,
                y: viewport.absolute_offset().y,
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    fn header<'a>(&'a self, tab: &'a Tab) -> El<'a> {
        let input = text_input("search tags…", &tab.temp_query)
            .on_input(Message::QueryChanged)
            .on_submit(Message::SubmitSearch)
            .padding(8)
            .width(Length::Fill);
        let submit = button(icon_search().size(18).color(style::WHITE))
            .on_press(Message::SubmitSearch)
            .style(solid(style::BLUE_500, style::BLUE_600));

        let prev = button(icon_chevron_left().size(20))
            .on_press_maybe((tab.page > 0).then_some(Message::PrevPage))
            .style(ghost);
        let page_no = text(format!("Page {}", tab.page + 1)).size(17);
        let next = button(icon_chevron_right().size(20))
            .on_press_maybe(tab.has_next_page.then_some(Message::NextPage))
            .style(ghost);
        let pager = row![prev, page_no, next].spacing(6).align_y(Center);

        let site_select = pick_list(Site::enabled().to_vec(), Some(tab.site), Message::SiteSelected)
            .text_size(15);
        let rating_choices: Vec<RatingChoice> = tab
            .site
            .ratings()
            .iter()
            .map(|r| RatingChoice(Some(*r)))
            .chain(std::iter::once(RatingChoice(None)))
            .collect();
        let rating_select = pick_list(
            rating_choices,
            Some(RatingChoice(tab.rating)),
            Message::RatingSelected,
        )
        .text_size(15);

        // Everything on one row (ebb's header), search input flexing to fill.
        let bar = row![input, submit, pager, site_select, rating_select]
            .spacing(8)
            .align_y(Center);

        let mut form = column![bar].spacing(8);
        if !tab.autocomplete.is_empty() {
            form = form.push(self.autocomplete_list(tab));
        }

        // Slightly taller vertical padding to match ebb's header height.
        container(form)
            .padding([10, 8])
            .width(Length::Fill)
            .style(bg(style::WHITE))
            .into()
    }

    /// Live tag suggestions for the word being typed (ebb's SearchInput dropdown).
    fn autocomplete_list(&self, tab: &Tab) -> El<'static> {
        let mut list = Column::new().spacing(2);
        for tag in &tab.autocomplete {
            let count = tag
                .post_count
                .map(format_count)
                .unwrap_or_default();
            let entry = row![
                colored(tag.label.clone(), tag.category.color().text_color()).size(13),
                Space::new().width(Length::Fill),
                text(count).size(13).color(style::GRAY_500),
            ]
            .align_y(Center);
            list = list.push(
                button(entry)
                    .on_press(Message::AutocompleteSelected(tag.value.clone()))
                    .width(Length::Fill)
                    .style(|_theme, status| {
                        let background = matches!(
                            status,
                            iced::widget::button::Status::Hovered
                                | iced::widget::button::Status::Pressed
                        )
                        .then(|| style::GRAY_100.into());
                        iced::widget::button::Style {
                            background,
                            text_color: style::BLACK,
                            border: iced::border::rounded(4.0),
                            ..iced::widget::button::Style::default()
                        }
                    }),
            );
        }
        container(list)
            .padding(4)
            .width(Length::Fill)
            .style(|_theme| iced::widget::container::Style {
                background: Some(style::WHITE.into()),
                border: Border {
                    color: style::GRAY_200,
                    width: 1.0,
                    radius: 4.0.into(),
                },
                ..iced::widget::container::Style::default()
            })
            .into()
    }

    fn grid<'a>(&'a self, tab: &'a Tab, viewport_h: f32) -> El<'a> {
        responsive(move |size| {
            // Record the exact content-area size for precise scroll math in
            // `update` (the grid's width is the true content width, after any
            // scrollbar; the height comes from the outer responsive).
            self.viewport.set(iced::Size::new(size.width, viewport_h));
            let cols = grid_cols(size.width);
            let cell = grid_cell(size.width, cols);

            let mut rows: Vec<El<'a>> = Vec::new();
            let mut current: Vec<El<'a>> = Vec::new();
            let flush = |current: &mut Vec<El<'a>>, rows: &mut Vec<El<'a>>| {
                if !current.is_empty() {
                    rows.push(Row::with_children(std::mem::take(current)).spacing(GRID_GAP).into());
                }
            };

            for post in &tab.posts {
                if tab.selected.contains(&post.id) {
                    flush(&mut current, &mut rows);
                    rows.push(self.expanded_post(tab, post, size.width, viewport_h));
                } else {
                    current.push(self.thumbnail(post, cell));
                    if current.len() == cols {
                        flush(&mut current, &mut rows);
                    }
                }
            }
            if tab.has_next_page {
                current.push(self.next_page_cell(cell));
                if current.len() == cols {
                    flush(&mut current, &mut rows);
                }
            }
            flush(&mut current, &mut rows);

            // No outer padding — the grid touches the window edges and the
            // header border (ebb). Only row/cell gaps remain.
            Column::with_children(rows).spacing(GRID_GAP).into()
        })
        .into()
    }

    fn thumbnail<'a>(&'a self, post: &'a BooruPost, cell: f32) -> El<'a> {
        let inner: El<'a> = match preview_url(post).and_then(|u| self.image_element(&u, cell)) {
            Some(img) => img,
            None => {
                // No usable image — show the extension and tags, as ebb does.
                let ext = post
                    .file_url
                    .rsplit('.')
                    .next()
                    .unwrap_or("?")
                    .to_string();
                container(
                    column![
                        text(ext).size(14).color(style::WHITE),
                        text(post.tags.join(" ")).size(11).color(style::WHITE),
                    ]
                    .spacing(4),
                )
                .padding(6)
                .width(Length::Fixed(cell))
                .height(Length::Fixed(cell))
                .style(bg(style::GRAY_500))
                .into()
            }
        };
        mouse_area(inner)
            .on_press(Message::TogglePost(post.id.clone()))
            .into()
    }

    /// A cached image sized to a square cell, or a placeholder while loading.
    fn image_element(&self, url: &str, cell: f32) -> Option<El<'_>> {
        match self.images.get(url, ImageKind::Thumbnail) {
            Some(ImageState::Loaded(handle)) => Some(
                image(handle.clone())
                    .content_fit(ContentFit::Cover)
                    .width(Length::Fixed(cell))
                    .height(Length::Fixed(cell))
                    .into(),
            ),
            Some(ImageState::Loading) => Some(
                container(Space::new().width(Length::Fixed(cell)).height(Length::Fixed(cell)))
                    .style(bg(style::GRAY_100))
                    .into(),
            ),
            Some(ImageState::Failed) | None => None,
        }
    }

    fn next_page_cell(&self, cell: f32) -> El<'_> {
        mouse_area(
            container(icon_chevron_right().size(96).color(style::GRAY_700))
                .center_x(Length::Fixed(cell))
                .center_y(Length::Fixed(cell))
                .style(bg(style::GRAY_200)),
        )
        .on_press(Message::NextPage)
        .into()
    }

    fn expanded_post<'a>(
        &'a self,
        tab: &'a Tab,
        post: &'a BooruPost,
        avail: f32,
        viewport_h: f32,
    ) -> El<'a> {
        // Max height = the exact content viewport, so the image fills it like ebb.
        let max_h = viewport_h.max(240.0);
        let media_h = max_h;
        // Size the image to its own aspect within the available area, so wide
        // images aren't letterboxed into a tall fixed box (ebb sizes to content).
        let (render_w, render_h) = render_size(post.width, post.height, avail - 8.0, max_h);

        let media: El<'a> = if is_image(&post.file_url) {
            match self.images.get(&post.file_url, ImageKind::Full) {
                Some(ImageState::Loaded(handle)) => mouse_area(
                    image(handle.clone())
                        .content_fit(ContentFit::Contain)
                        .width(Length::Fixed(render_w))
                        .height(Length::Fixed(render_h)),
                )
                .on_press(Message::TogglePost(post.id.clone()))
                .into(),
                Some(ImageState::Failed) => container(text("Failed to load image."))
                    .height(Length::Fixed(240.0))
                    .center_x(Length::Fill)
                    .center_y(Length::Fixed(240.0))
                    .into(),
                _ => container(text("Loading…").color(style::BLUE_500))
                    .height(Length::Fixed(render_h))
                    .center_x(Length::Fill)
                    .center_y(Length::Fixed(render_h))
                    .into(),
            }
        } else if is_swf(&post.file_url) {
            match tab.swf.get(&post.id) {
                Some(player) => container(
                    Ruffle::new(player)
                        .width(Length::Fill)
                        .height(Length::Fixed(media_h)),
                )
                .style(bg(style::BLACK))
                .into(),
                None => container(text("Loading Flash…").color(style::BLUE_500))
                    .height(Length::Fixed(media_h))
                    .center_x(Length::Fill)
                    .center_y(Length::Fixed(media_h))
                    .into(),
            }
        } else if is_video(&post.file_url) {
            // Video playback is intentionally not implemented in this build.
            column![
                text("Video playback is not supported in this build."),
                button(text("Open externally").color(style::WHITE))
                    .on_press(Message::OpenExternal(post.file_url.clone()))
                    .style(solid(style::BLUE_500, style::BLUE_600)),
            ]
            .spacing(8)
            .align_x(Center)
            .into()
        } else {
            column![
                text("Unknown file type"),
                text(post.file_url.clone()).size(12),
            ]
            .spacing(4)
            .align_x(Center)
            .into()
        };

        let mut stack = Column::new().spacing(8).width(Length::Fill);
        // ebb left-aligns the expanded image (max-w-full, no centering).
        let mut media_container = container(media).width(Length::Fill);
        // Tag the post we're scrolling to so the centering operation can read
        // this image's exact laid-out bounds.
        if self.scroll_anchor.as_deref() == Some(post.id.as_str()) {
            media_container = media_container.id(self.anchor_id.clone());
        }
        stack = stack.push(media_container);

        // Images collapse on click; SWF (you click to interact with the movie)
        // and the video stub get an explicit Close button — as ebb does for SWF.
        if !is_image(&post.file_url) {
            let close = button(text(format!("Close {}", post.id)).color(style::WHITE))
                .on_press(Message::TogglePost(post.id.clone()))
                .style(solid(style::BLUE_500, style::BLUE_600));
            stack = stack.push(container(close).center_x(Length::Fill));
        }

        stack.push(self.post_details(tab, post, avail)).into()
    }

    fn post_details<'a>(&self, tab: &Tab, post: &'a BooruPost, avail: f32) -> El<'a> {
        let query_words: Vec<String> = tab
            .query
            .as_deref()
            .unwrap_or("")
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let temp_words: Vec<String> = tab
            .temp_query
            .split_whitespace()
            .map(str::to_string)
            .collect();

        // Use the post's category groups when available; otherwise a single
        // "Tag" group over the flat tag list (ebb's fallback).
        let mut groups: Vec<(String, Vec<String>)> = if post.tag_groups.is_empty() {
            vec![("Tag".to_string(), post.tags.clone())]
        } else {
            post.tag_groups
                .iter()
                .map(|g| (g.category.label().to_string(), g.tags.clone()))
                .collect()
        };
        groups.sort_by(|a, b| a.0.cmp(&b.0));

        let value_w = (avail - 140.0).max(180.0);
        let mut rows: Vec<El<'static>> = Vec::new();
        for (label, tags) in &groups {
            let chips = tags
                .iter()
                .map(|tag| {
                    (
                        chip_width(tag),
                        tag_button(tag, &query_words, &temp_words),
                    )
                })
                .collect();
            rows.push(detail_row(label, flow_left(chips, value_w)));
        }

        rows.push(detail_row(
            "Rating",
            container(text(post.rating.clone()).size(13).color(style::WHITE))
                .padding([2, 12])
                .style(rounded_bg(style::rating_color(&post.rating), 999.0))
                .into(),
        ));
        rows.push(detail_row(
            "Post Date",
            container(colored(format_date(&post.created_at), style::WHITE).size(13))
                .padding([2, 12])
                .style(rounded_bg(style::GRAY_700, 999.0))
                .into(),
        ));
        rows.push(detail_row(
            "Actions",
            row![
                action_button(
                    icon_external_link().color(style::WHITE).size(14),
                    "Open",
                    Message::OpenExternal(post.post_view.clone()),
                    style::BLUE_500,
                    style::BLUE_600,
                ),
                action_button(
                    icon_download().color(style::WHITE).size(14),
                    "Download",
                    Message::DownloadPost {
                        post_id: post.id.clone(),
                        url: post.file_url.clone(),
                    },
                    style::INDIGO_500,
                    style::INDIGO_600,
                ),
            ]
            .spacing(8)
            .align_y(Center)
            .into(),
        ));

        Column::with_children(rows)
            .spacing(8)
            .padding(8)
            .width(Length::Fill)
            .into()
    }

    fn settings_modal(&self) -> El<'_> {
        let header = row![
            text("Settings").size(20).color(style::BLACK),
            Space::new().width(Length::Fill),
            button(icon_x().color(style::BLACK))
                .on_press(Message::CloseSettings)
                .style(ghost),
        ]
        .align_y(Center);

        let field = column![
            text("Gelbooru API Credentials")
                .size(14)
                .color(style::GRAY_700),
            text_input("&api_key=YOUR_KEY&user_id=YOUR_ID", &self.settings_draft)
                .on_input(Message::SettingsDraftChanged)
                .padding(8),
            text("Format: &api_key=YOUR_KEY&user_id=YOUR_ID")
                .size(11)
                .color(style::GRAY_500),
        ]
        .spacing(4);

        let actions = row![
            Space::new().width(Length::Fill),
            button(text("Cancel").color(style::BLACK))
                .on_press(Message::CloseSettings)
                .style(ghost),
            button(text("Save").color(style::WHITE))
                .on_press(Message::SaveSettings)
                .style(solid(style::INDIGO_500, style::INDIGO_600)),
        ]
        .spacing(8);

        let card = container(column![header, field, actions].spacing(16))
            .padding(20)
            .width(Length::Fixed(460.0))
            .style(card);
        // Absorb clicks on the card so they don't bubble to the scrim (which
        // would otherwise close the modal).
        let card = mouse_area(card).on_press(Message::Noop);

        let scrim = container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .center_x(Length::Fill)
            .center_y(Length::Fill)
            .style(backdrop);

        // The scrim absorbs clicks so they don't hit the UI beneath, but does
        // not close the modal (ebb only closes via X/Cancel/Save). It captures
        // presses only, so scrolling still passes through to the grid.
        mouse_area(scrim).on_press(Message::Noop).into()
    }

    fn empty_state(&self) -> El<'_> {
        container(
            column![
                text("ribb").size(40).color(style::BLACK),
                button(colored("https://github.com/pikdum/ribb", style::BLUE_500))
                    .on_press(Message::OpenExternal(
                        "https://github.com/pikdum/ribb".to_string()
                    ))
                    .style(ghost),
                text("rust iced booru browser — a native port of ebb")
                    .size(14)
                    .color(style::GRAY_700),
            ]
            .spacing(16)
            .align_x(Center),
        )
        .padding(40)
        .width(Length::Fill)
        .center_x(Length::Fill)
        .into()
    }
}

fn detail_row(label: &str, value: El<'static>) -> El<'static> {
    row![
        container(label_chip(label))
            .width(Length::Fixed(96.0))
            .align_right(Length::Fixed(96.0)),
        container(value).width(Length::Fill),
    ]
    .spacing(10)
    .align_y(Center)
    .width(Length::Fill)
    .into()
}

fn action_button(
    icon: iced::widget::Text<'static>,
    label: &'static str,
    message: Message,
    bg: Color,
    hover: Color,
) -> El<'static> {
    button(row![icon, text(label).size(13).color(style::WHITE)]
        .spacing(6)
        .align_y(Center))
    .padding([3, 12])
    .on_press(message)
    .style(pill(bg, hover))
    .into()
}

/// A single tag button, colored by whether it is in the submitted query and/or
/// the unsubmitted input (ebb's TagButton states). Normal click toggles the tag
/// in the current query; Ctrl+click opens the tag in a background tab.
fn tag_button(tag: &str, query_words: &[String], temp_words: &[String]) -> El<'static> {
    let in_query = query_words.iter().any(|w| w == tag);
    let in_temp = temp_words.iter().any(|w| w == tag);
    let (bg_color, hover) = match (in_query, in_temp) {
        (true, true) => (style::BLUE_700, style::BLUE_600),      // active & saved
        (false, true) => (style::PURPLE_500, style::PURPLE_600), // added, unsaved
        (true, false) => (style::RED_500, style::RED_500),       // removed, unsaved
        (false, false) => (style::BLUE_500, style::BLUE_700),    // default
    };
    let tag_owned = tag.to_string();
    button(text(tag_owned.clone()).size(13).color(style::WHITE))
        .padding([3, 12])
        .on_press(Message::TagClicked(tag_owned))
        .style(pill(bg_color, hover))
        .into()
}

# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

ribb ("rust iced booru browser") is a native Rust + iced port of **ebb**, an Electron/React booru browser at `~/code/ebb/`. When porting behavior, the source of truth is ebb's `renderer/` (components and `lib/booru/`).

## Commands

The dev shell (Rust toolchain + native libs) is provided by `devenv` and loaded automatically via `direnv`, so plain `cargo` works. If the env isn't active, wrap commands as `devenv shell "<cmd>"`.

- Build / run: `cargo build`, `cargo run --release` (release matters — image decode is far faster optimized)
- Tests: `cargo test`; single test: `cargo test --test booru danbooru_parse_posts`; lib-only: `cargo test --lib`
- The Gelbooru live test needs credentials and is `#[ignore]`d:
  `SETTING_GELBOORU_API_CREDENTIALS='&api_key=...&user_id=...' cargo test --test booru -- --ignored`
- Lint (matches the commit hook): `cargo clippy --all-targets -- -D warnings`

Git hooks (configured in `devenv.nix`, run on commit) act as CI: **clippy `--all-targets -D warnings`**, rustfmt, nixfmt, actionlint. Commits will fail on any clippy warning. Use Conventional Commits.

## Validating the GUI (no screenshots)

This environment can render an iced window but cannot capture it. Validate GUI changes by: (1) it builds, (2) running with debug env vars and reading `tracing` logs (`RUST_LOG=ribb=debug`). Do not attempt screenshots.

- `RIBB_DEBUG_QUERY="tags"` — run a search on launch (rating filter off)
- `RIBB_DEBUG_EXPAND=1` — auto-expand a post (prefers an SWF), exercising detail / full-image / Ruffle paths
- `RIBB_DEBUG_TYPE="word"` — simulate typing to exercise autocomplete

Example: `timeout 30 env RUST_LOG=ribb=debug RIBB_DEBUG_QUERY=flash RIBB_DEBUG_EXPAND=1 cargo run --release` then grep `ribb::app`/`ribb::cache` lines.

## Architecture

### Booru API layer (`src/booru/`)
A pure, **synchronous** `Provider` trait (`mod.rs`) — one zero-sized unit struct per site (`danbooru`/`gelbooru`/`e621`/`rule34`) implements only request-building (`*_request` → `reqwest::RequestBuilder`) and response-parsing (`parse_*`). No method does IO, so parsers are unit-testable against canned JSON. The `Site` enum dispatches to `provider()`. All async HTTP orchestration (send + parse + ebb's error handling) lives **once** on `BooruClient` (`get_posts`/`get_tags`/`get_tag_groups`). `types.rs` holds the normalized types every provider maps into (`BooruPost`, `BooruTag`, `Rating`, `TagCategory`, timestamp normalization to RFC3339 UTC). Gelbooru needs credentials + a follow-up request for tag groups; other providers supply groups inline. Rule34 exists but is omitted from `Site::enabled()` (the UI list), matching ebb.

### GUI (`src/app.rs` + `src/view.rs`)
iced 0.14, functional `application` API in `main.rs`. ebb's two React Contexts become flat state: app-wide `Ribb` (tabs, settings, shared image cache, viewport/scroll ids) and per-tab `Tab` (search session). `useEffect`-style fetches become explicit `Task`s returned from `update`.

- **`view.rs` is `include!`d into `app.rs`** (last line of `app.rs`), so it shares the `app` module's scope and imports. Do not add `use` statements there for things already imported in `app.rs`, and helper free-fns/types defined in either file are visible in both.
- Stale-fetch guard: each `Tab` has a `generation` counter bumped per fetch; `PostsLoaded` is discarded if it doesn't match.
- Keyboard shortcuts (Ctrl+T/W/H/L, arrows) come through `Message::Key` from `keyboard::listen()` and are translated in `handle_key`.

### Images (`src/cache.rs`)
iced re-runs `view()` constantly, so images must be fetched/decoded once and cached. Key gotchas baked in here:
- Decode + downscale happen **off the render thread** (`spawn_blocking`) and are handed to iced as ready RGBA via `Handle::from_rgba`. Using `Handle::from_bytes` instead would defer decode to the single render thread and stall the UI.
- The cache is keyed by **(url, `ImageKind::{Thumbnail,Full}`)** via two maps — a post's thumbnail and full image can share a URL, and reusing the small thumbnail for the full view caused blur.
- Thumbnails cap at `THUMB_MAX` (fast filter); full images decode to ~`FULL_SUPERSAMPLE`× the on-screen size with Lanczos3 (supersampling, since iced's GPU sampler has no mipmaps).
- All image/SWF fetches send `Referer: <the URL itself>` to defeat hotlink protection (Gelbooru's CDN 302s to an HTML page otherwise) — mirrors ebb's `onBeforeSendHeaders`.

### Notable iced patterns to preserve
- **Exact scroll-to-center**: expanding a post sets `scroll_anchor`; `view` tags that post's image container with `anchor_id`; a `CenterOnAnchor` widget operation (run via `iced::advanced::widget::operate`) reads the real laid-out bounds and computes the offset. Don't reintroduce geometry estimates.
- The scroll viewport size is measured by a `responsive` wrapper and stashed in a `Cell<Size>` for `update` to read.
- **Settings modal**: an always-present `stack` overlay (empty 0-size layer when closed — toggling the layer in/out would reset the scrollable's offset). A `mouse_area` scrim absorbs background clicks (`Message::Noop`); only X/Cancel/Save close it.
- Video is intentionally **stubbed** (would need ffmpeg); SWF plays via `iced_ruffle`.

### Constraints
iced is pinned to **0.14** (with `wgpu` + `advanced` features) because `iced_ruffle` (the SWF widget, a git dependency on `github.com/pikdum/iced_ruffle`) must resolve to the same wgpu. Ruffle is built from git `master` (needs a JDK + Rust ≥1.95) and audio uses cpal/ALSA — `devenv.nix` provides these and `LD_LIBRARY_PATH` for the winit/wgpu runtime libs. To bump the SWF widget: `cargo update -p iced_ruffle`.

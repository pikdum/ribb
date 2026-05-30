//! ribb — rust iced booru browser. GUI entry point.

fn main() -> iced::Result {
    tracing_subscriber_init();
    iced::application(
        ribb::app::Ribb::boot,
        ribb::app::Ribb::update,
        ribb::app::Ribb::view,
    )
    .title("ribb")
    .window_size((1100.0, 800.0))
    .theme(ribb::app::Ribb::theme)
    .subscription(ribb::app::Ribb::subscription)
    // Bundle the Lucide icon font (ebb used Feather, which became Lucide).
    .font(lucide_icons::LUCIDE_FONT_BYTES)
    .run()
}

/// Minimal tracing setup; level controlled by `RUST_LOG` (default `info`).
fn tracing_subscriber_init() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

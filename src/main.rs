//! ribb — rust iced booru browser. GUI entry point.

fn main() -> iced::Result {
    tracing_subscriber_init();
    iced::application(
        ribb::app::Ribb::new,
        ribb::app::Ribb::update,
        ribb::app::Ribb::view,
    )
    .title("ribb")
    .window_size((1100.0, 800.0))
    .subscription(ribb::app::Ribb::subscription)
    .run()
}

/// Minimal tracing setup; level controlled by `RUST_LOG` (default `info`).
fn tracing_subscriber_init() {
    use tracing_subscriber::EnvFilter;
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).try_init();
}

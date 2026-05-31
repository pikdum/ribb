//! ribb — rust iced booru browser. GUI entry point.

// On Windows, use the GUI subsystem in release builds so launching ribb doesn't
// pop up a console window. Debug builds keep the console for tracing output.
// (The attribute only affects Windows targets; it's a no-op elsewhere.)
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

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

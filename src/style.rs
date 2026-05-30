//! Color palette and small style helpers, approximating ebb's Tailwind theme.

use iced::Color;

use crate::booru::{Rating, TagColor};

/// Build a `Color` from 8-bit RGB components (opaque).
pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
}

// Tailwind palette subset used by ebb.
pub const WHITE: Color = rgb(0xff, 0xff, 0xff);
pub const BLACK: Color = rgb(0x00, 0x00, 0x00);

pub const GRAY_100: Color = rgb(0xf3, 0xf4, 0xf6);
pub const GRAY_200: Color = rgb(0xe5, 0xe7, 0xeb);
pub const GRAY_300: Color = rgb(0xd1, 0xd5, 0xdb);
pub const GRAY_500: Color = rgb(0x6b, 0x72, 0x80);
pub const GRAY_600: Color = rgb(0x4b, 0x55, 0x63);
pub const GRAY_700: Color = rgb(0x37, 0x41, 0x51);

pub const BLUE_200: Color = rgb(0xbf, 0xdb, 0xfe);
pub const BLUE_500: Color = rgb(0x3b, 0x82, 0xf6);
pub const BLUE_600: Color = rgb(0x25, 0x63, 0xeb);
pub const BLUE_700: Color = rgb(0x1d, 0x4e, 0xd8);

pub const INDIGO_300: Color = rgb(0xa5, 0xb4, 0xfc);
pub const INDIGO_400: Color = rgb(0x81, 0x8c, 0xf8);
pub const INDIGO_500: Color = rgb(0x63, 0x66, 0xf1);
pub const INDIGO_600: Color = rgb(0x4f, 0x46, 0xe5);

pub const PURPLE_500: Color = rgb(0xa8, 0x55, 0xf7);
pub const PURPLE_600: Color = rgb(0x93, 0x33, 0xea);

pub const GREEN_500: Color = rgb(0x22, 0xc5, 0x5e);
pub const GREEN_600: Color = rgb(0x16, 0xa3, 0x4a);

pub const YELLOW_500: Color = rgb(0xea, 0xb3, 0x08);
pub const YELLOW_600: Color = rgb(0xca, 0x8a, 0x04);

pub const ORANGE_500: Color = rgb(0xf9, 0x73, 0x16);
pub const ORANGE_600: Color = rgb(0xea, 0x58, 0x0c);

pub const RED_500: Color = rgb(0xef, 0x44, 0x44);

impl TagColor {
    /// Text color used for an autocomplete tag of this category.
    pub fn text_color(self) -> Color {
        match self {
            TagColor::Blue => BLUE_600,
            TagColor::Purple => PURPLE_600,
            TagColor::Green => GREEN_600,
            TagColor::Orange => ORANGE_600,
            TagColor::Yellow => YELLOW_600,
            TagColor::Gray => GRAY_600,
        }
    }
}

/// Badge color for a post's rating (ebb's PostDetails rating chip).
pub fn rating_color(rating: &str) -> Color {
    match Rating::from_loose(rating) {
        Some(Rating::General) => GREEN_500,
        Some(Rating::Sensitive) => YELLOW_500,
        Some(Rating::Questionable) => ORANGE_500,
        Some(Rating::Explicit) => RED_500,
        None => GRAY_500,
    }
}

/// Button color for the rating selector (ebb's RatingSelect), where `None`
/// means "All Content".
pub fn rating_select_color(rating: Option<Rating>) -> Color {
    match rating {
        Some(Rating::General) => GREEN_500,
        Some(Rating::Sensitive) => YELLOW_500,
        Some(Rating::Questionable) => ORANGE_500,
        Some(Rating::Explicit) => RED_500,
        None => PURPLE_500,
    }
}

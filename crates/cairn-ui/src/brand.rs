// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Brand constants and the decoded app mark, shared by every Cairn window.

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use gpui_kit::gpui::{App, Font, FontFallbacks, FontWeight, Image, ImageFormat, SharedString};

/// The base font of a Cairn window at zoom 1.0, in pixels. It anchors every `rem` helper.
pub const BASE_FONT_PX: f32 = 14.0;

/// The one corner radius product frames use.
pub const RADIUS: gpui_kit::gpui::Pixels = gpui_kit::gpui::px(3.0);

/// The brand wordmark size in rem: 16 px at 100%, shared by the app and installer title bars.
pub const BRAND_TITLE_SIZE: f32 = 16.0 / BASE_FONT_PX;

/// Cormorant's ascent and descent total 1.211 em. A 1.25 line box keeps the glyph ink,
/// including the overshoot at the bottom of round letters, inside the text bounds.
pub const SERIF_LINE_HEIGHT: f32 = 1.25;

/// The installer's title-bar descriptor: one UI label step (12.25 px at 100%).
pub const BRAND_DESCRIPTOR_SIZE: f32 = 12.25 / BASE_FONT_PX;

/// The title-bar app mark token: a separate 20 px (5 × 4 px) size, distinct from the 16 px
/// wordmark. The mark itself remains the frozen master asset.
pub const APP_ICON_SIZE: gpui_kit::gpui::Pixels = gpui_kit::gpui::px(20.0);

/// The one control-glyph token: 1rem (14 px at 100%). Title-bar navigation, window captions,
/// status actions/loading and sidebar rows all read this value; the 20 px app mark and 12 px
/// legend swatches are the two deliberate exceptions.
pub const UI_ICON_SIZE: f32 = 1.0;

/// One device pixel: an audited physical exception, not a layout distance.
pub const HAIRLINE: gpui_kit::gpui::Pixels = gpui_kit::gpui::px(1.0);

/// Designated Western serif family for brand mark and Latin headings.
pub const SERIF_FONT_FAMILY_WESTERN: &str = "Cormorant";
/// Paired CJK serif family for Chinese headings.
pub const SERIF_FONT_FAMILY_CJK: &str = "Noto Serif SC";

/// Embedded TTF font files for the designated Western serif family (Cormorant).
const CORMORANT_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Cormorant-Regular.ttf");
const CORMORANT_SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/Cormorant-SemiBold.ttf");
const CORMORANT_BOLD: &[u8] = include_bytes!("../../../assets/fonts/Cormorant-Bold.ttf");
const CORMORANT_ITALIC: &[u8] = include_bytes!("../../../assets/fonts/Cormorant-Italic.ttf");

/// Registers the embedded Cormorant font family with GPUI's text system.
///
/// Safe to call multiple times; registers the embedded fonts once per process.
pub fn init_fonts(cx: &App) {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let fonts = vec![
            Cow::Borrowed(CORMORANT_REGULAR),
            Cow::Borrowed(CORMORANT_SEMIBOLD),
            Cow::Borrowed(CORMORANT_BOLD),
            Cow::Borrowed(CORMORANT_ITALIC),
        ];
        let _ = cx.text_system().add_fonts(fonts);
    });
}

/// The 32 px mark used at its existing 22 px optical size in every title bar. Assets are frozen:
/// the mark is not redrawn or expanded into a size ladder, so the title bar keeps the mark's
/// known-clear optical size and lets the 16 px wordmark sit beside it.
pub const APP_ICON_PNG: &[u8] = include_bytes!("../../../assets/icons/cairn-32.png");

/// The decoded app mark, built once at first paint.
pub fn app_icon() -> Arc<Image> {
    static APP_ICON: OnceLock<Arc<Image>> = OnceLock::new();
    APP_ICON
        .get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Png, APP_ICON_PNG.to_vec())))
        .clone()
}

/// Fixed-width figures for every value whose digits change while a window is open.
pub fn tabular_figures() -> gpui_kit::gpui::FontFeatures {
    static FEATURES: OnceLock<gpui_kit::gpui::FontFeatures> = OnceLock::new();
    FEATURES
        .get_or_init(|| gpui_kit::gpui::FontFeatures(Arc::new(vec![("tnum".to_string(), 1)])))
        .clone()
}

struct SerifConfig {
    primary: SharedString,
    fallbacks: Option<FontFallbacks>,
}

fn resolve_serif() -> &'static SerifConfig {
    static CONFIG: OnceLock<SerifConfig> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let primary = SharedString::from(SERIF_FONT_FAMILY_WESTERN);

        // The brand forbids fallbacks: Cormorant is embedded, so Western text always resolves to
        // it, and Chinese text resolves to Noto Serif SC. No other family may stand in.
        let fallbacks = vec![SERIF_FONT_FAMILY_CJK.to_string()];

        SerifConfig {
            primary,
            fallbacks: Some(FontFallbacks::from_fonts(fallbacks)),
        }
    })
}

/// Returns the paired serif font: embedded Cormorant for Western, Noto Serif SC for CJK, and
/// no other fallback.
pub fn serif_font(weight: FontWeight) -> Font {
    let config = resolve_serif();
    Font {
        family: config.primary.clone(),
        features: Default::default(),
        fallbacks: config.fallbacks.clone(),
        weight,
        style: Default::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_cormorant_fonts_are_valid_and_non_empty() {
        assert!(CORMORANT_REGULAR.len() > 100_000);
        assert!(CORMORANT_SEMIBOLD.len() > 100_000);
        assert!(CORMORANT_BOLD.len() > 100_000);
        assert!(CORMORANT_ITALIC.len() > 100_000);
        assert_eq!(&CORMORANT_REGULAR[0..4], &[0x00, 0x01, 0x00, 0x00]);
        assert_eq!(&CORMORANT_SEMIBOLD[0..4], &[0x00, 0x01, 0x00, 0x00]);
        assert_eq!(&CORMORANT_BOLD[0..4], &[0x00, 0x01, 0x00, 0x00]);
        assert_eq!(&CORMORANT_ITALIC[0..4], &[0x00, 0x01, 0x00, 0x00]);
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn serif_line_height_clears_cormorant_metrics() {
        // Cormorant's hhea ascent + descent is 1.211 em. A tighter line box clips the
        // bottom overshoot of round letters such as C.
        assert!(SERIF_LINE_HEIGHT >= 1.211);
    }

    #[test]
    fn serif_font_config_points_to_cormorant() {
        let font = serif_font(FontWeight::SEMIBOLD);
        assert_eq!(font.family.as_str(), "Cormorant");
        assert!(font.fallbacks.is_some());
    }

    #[test]
    fn serif_fallback_chain_holds_noto_serif_sc_and_nothing_else() {
        let font = serif_font(FontWeight::NORMAL);
        let fallbacks = font
            .fallbacks
            .expect("the serif font carries a fallback chain");
        assert_eq!(
            fallbacks.fallback_list(),
            [SERIF_FONT_FAMILY_CJK.to_string()],
            "the brand forbids fallbacks: Noto Serif SC is the only one"
        );
    }
}

// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Brand constants and the decoded app mark, shared by every Cairn window.

use std::borrow::Cow;
use std::sync::{Arc, OnceLock};

use gpui_kit::gpui::{
    div, AnyElement, App, Font, FontFallbacks, FontWeight, Image, ImageFormat, IntoElement as _,
    ParentElement as _, SharedString, Styled as _, TextRenderingMode,
};

/// The base font of a Cairn window at zoom 1.0, in pixels. It anchors every `rem` helper.
pub const BASE_FONT_PX: f32 = 14.0;

/// The one corner radius product frames use.
pub const RADIUS: gpui_kit::gpui::Pixels = gpui_kit::gpui::px(3.0);

/// The brand wordmark size in rem: 16 px at 100%, shared by the app and installer title bars.
pub const BRAND_TITLE_SIZE: f32 = 16.0 / BASE_FONT_PX;

/// Cormorant's ascent and descent total 1.211 em. A 1.25 line box keeps the glyph ink,
/// including the overshoot at the bottom of round letters, inside the text bounds; CJK
/// headings share the same heading line box.
pub const SERIF_LINE_HEIGHT: f32 = 1.25;

/// The small UI text step: 0.875 rem, 12.25 px at 100%. Status-bar text, hints, section
/// labels, state copy, control labels and the installer descriptor use it in place of the
/// framework's 10.5 px `text_xs`.
pub const SMALL_TEXT_SIZE: f32 = 12.25 / BASE_FONT_PX;

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
/// Embedded CJK serif for Chinese headings, a common-character subset of Noto Serif SC.
pub const SERIF_FONT_FAMILY_CJK: &str = "Cairn CJK Serif";
/// The platform system fallback covers any glyph the embedded subset does not contain.
pub const SERIF_FONT_FALLBACK_SYSTEM: &str = ".SystemUIFont";

/// Embedded OpenType files from the official Cormorant v4.002 release.
const CORMORANT_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/Cormorant-Regular.otf");
const CORMORANT_SEMIBOLD: &[u8] = include_bytes!("../../../assets/fonts/Cormorant-SemiBold.otf");
const CJK_SERIF_REGULAR: &[u8] = include_bytes!("../../../assets/fonts/CairnSerifCJK-Regular.ttf");

/// Registers the embedded serif faces with GPUI's text system: Cormorant Regular and
/// SemiBold, plus the common-character CJK subset.
///
/// Safe to call multiple times; registers the embedded fonts once per process.
pub fn init_fonts(cx: &App) {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(|| {
        let fonts = vec![
            Cow::Borrowed(CORMORANT_REGULAR),
            Cow::Borrowed(CORMORANT_SEMIBOLD),
            Cow::Borrowed(CJK_SERIF_REGULAR),
        ];
        let _ = cx.text_system().add_fonts(fonts);
    });
}

/// Switches every product glyph to grayscale antialiasing.
///
/// ClearType paints coloured fringes on the serif's thin strokes at fractional display scales.
/// Grayscale keeps the edges neutral and still uses the platform's hinting. It must run before
/// the first window opens so every window inherits the mode.
pub fn init_text_rendering(cx: &mut App) {
    cx.set_text_rendering_mode(TextRenderingMode::Grayscale);
}

/// The 32 px mark drawn at the 20 px title-bar token in every title bar. Assets are frozen:
/// the mark is not redrawn or expanded into a size ladder; the title bar keeps a clear mark and
/// lets the 16 px wordmark sit beside it.
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

        // GPUI resolves fallback families against the system font collection, so the embedded
        // CJK face cannot appear here; `heading_text` runs it as a primary family instead.
        let fallbacks = vec![SERIF_FONT_FALLBACK_SYSTEM.to_string()];

        SerifConfig {
            primary,
            fallbacks: Some(FontFallbacks::from_fonts(fallbacks)),
        }
    })
}

/// Returns the Western heading font: embedded Cormorant, with the platform UI sans for any
/// glyph Cormorant lacks. Chinese headings use [`cjk_heading_font`] through [`heading_text`].
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

/// A localized heading whose CJK runs use the embedded serif at its regular weight while
/// Latin runs keep the requested heading weight. Shared punctuation such as curly double
/// quotes, dashes and the ellipsis follows the CJK face when the heading contains Chinese.
/// The base element should carry the same `serif_font(weight)` so unstyled text and line
/// metrics stay coherent.
pub fn heading_text(text: impl Into<SharedString>, latin_weight: FontWeight) -> AnyElement {
    let text = text.into();

    let children: Vec<AnyElement> = split_scripts(&text)
        .into_iter()
        .map(|(cjk, part)| {
            let font = if cjk {
                cjk_heading_font()
            } else {
                serif_font(latin_weight)
            };
            div()
                .min_w_0()
                .font(font)
                .child(part.to_string())
                .into_any_element()
        })
        .collect();

    div()
        .flex()
        .flex_row()
        .items_baseline()
        .flex_wrap()
        .children(children)
        .into_any_element()
}

/// Splits a heading into CJK and Latin runs. Punctuation shared by both scripts follows the
/// CJK face only when the heading itself contains Chinese, so `删除“报告”` keeps Song quotes
/// while an English title keeps Cormorant quotes. Apostrophes stay Latin so names such as
/// `Cairn's` never borrow a Chinese glyph.
fn split_scripts(text: &str) -> Vec<(bool, &str)> {
    let cjk_context = text.chars().any(is_cjk);
    let mut segments = Vec::new();
    let mut start = 0;
    let mut current = None;
    for (index, ch) in text.char_indices() {
        let cjk = is_cjk(ch) || (cjk_context && is_shared_punctuation(ch));
        match current {
            None => {
                current = Some(cjk);
                start = index;
            }
            Some(value) if value == cjk => {}
            Some(value) => {
                segments.push((value, &text[start..index]));
                current = Some(cjk);
                start = index;
            }
        }
    }
    if let Some(value) = current {
        segments.push((value, &text[start..text.len()]));
    }
    segments
}

/// Punctuation that reads correctly in either script and takes the heading's dominant face.
fn is_shared_punctuation(ch: char) -> bool {
    matches!(
        ch,
        '\u{2013}' | '\u{2014}' | '\u{201C}' | '\u{201D}' | '\u{2026}'
    )
}

/// The embedded CJK heading face: Source Han Serif at its regular weight, with the platform UI
/// sans handling characters outside the embedded common subset.
pub fn cjk_heading_font() -> Font {
    Font {
        family: SERIF_FONT_FAMILY_CJK.into(),
        features: Default::default(),
        fallbacks: Some(FontFallbacks::from_fonts(vec![
            SERIF_FONT_FALLBACK_SYSTEM.to_string()
        ])),
        weight: FontWeight::NORMAL,
        style: Default::default(),
    }
}

/// Whether a string contains CJK characters handled by the embedded serif.
pub fn has_cjk(text: &str) -> bool {
    text.chars().any(is_cjk)
}

fn is_cjk(ch: char) -> bool {
    matches!(ch as u32, 0x3000..=0x303F | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xFF00..=0xFFEF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_cormorant_fonts_are_valid_and_non_empty() {
        assert!(CORMORANT_REGULAR.len() > 100_000);
        assert!(CORMORANT_SEMIBOLD.len() > 100_000);
        assert_eq!(&CORMORANT_REGULAR[0..4], b"OTTO");
        assert_eq!(&CORMORANT_SEMIBOLD[0..4], b"OTTO");
    }

    #[test]
    fn split_scripts_keeps_chinese_punctuation_with_the_chinese_run() {
        assert_eq!(
            split_scripts("删除“报告.pdf”？"),
            vec![(true, "删除“报告"), (false, ".pdf"), (true, "”？")]
        );
        assert_eq!(
            split_scripts("永久删除“安装包”？"),
            vec![(true, "永久删除“安装包”？")]
        );
    }

    #[test]
    fn split_scripts_leaves_shared_punctuation_latin_without_chinese() {
        assert_eq!(
            split_scripts("Delete “Report”…"),
            vec![(false, "Delete “Report”…")]
        );
        assert_eq!(
            split_scripts("Cairn's files"),
            vec![(false, "Cairn's files")]
        );
    }

    #[test]
    fn split_scripts_separates_mixed_runs() {
        assert_eq!(
            split_scripts("Cairn 文件已移除"),
            vec![(false, "Cairn "), (true, "文件已移除")]
        );
        assert!(split_scripts("").is_empty());
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn serif_line_height_clears_cormorant_metrics() {
        // Cormorant's hhea ascent + descent is 1.211 em. A tighter line box clips the
        // bottom overshoot of round letters such as C.
        assert!(SERIF_LINE_HEIGHT >= 1.211);
    }

    #[test]
    fn serif_font_config_selects_the_embedded_weights() {
        let regular = serif_font(FontWeight::NORMAL);
        assert_eq!(regular.family.as_str(), SERIF_FONT_FAMILY_WESTERN);
        assert_eq!(regular.weight, FontWeight::NORMAL);

        let semibold = serif_font(FontWeight::SEMIBOLD);
        assert_eq!(semibold.family.as_str(), SERIF_FONT_FAMILY_WESTERN);
        assert_eq!(semibold.weight, FontWeight::SEMIBOLD);
        assert!(semibold.fallbacks.is_some());
    }

    #[test]
    fn the_cjk_heading_face_is_regular_with_a_system_sans_fallback() {
        let font = cjk_heading_font();
        assert_eq!(font.family.as_str(), SERIF_FONT_FAMILY_CJK);
        assert_eq!(font.weight, FontWeight::NORMAL);
        let fallbacks = font
            .fallbacks
            .expect("the CJK heading font carries a fallback chain");
        assert_eq!(
            fallbacks.fallback_list(),
            [SERIF_FONT_FALLBACK_SYSTEM.to_string()]
        );
    }

    #[test]
    fn the_western_serif_falls_back_to_the_system_sans() {
        let font = serif_font(FontWeight::NORMAL);
        let fallbacks = font
            .fallbacks
            .expect("the serif font carries a fallback chain");
        assert_eq!(
            fallbacks.fallback_list(),
            [SERIF_FONT_FALLBACK_SYSTEM.to_string()]
        );
    }

    #[test]
    fn the_embedded_cjk_serif_is_a_named_common_subset() {
        assert!(CJK_SERIF_REGULAR.len() > 1_400_000);
        assert_eq!(&CJK_SERIF_REGULAR[0..4], &[0x00, 0x01, 0x00, 0x00]);
        let family: Vec<u8> = SERIF_FONT_FAMILY_CJK
            .encode_utf16()
            .flat_map(|unit| unit.to_be_bytes())
            .collect();
        assert!(
            CJK_SERIF_REGULAR
                .windows(family.len())
                .any(|window| window == family),
            "the embedded subset carries its platform family name"
        );
    }
}

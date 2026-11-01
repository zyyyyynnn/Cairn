// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Shared surface for every modal alert: one fixed size, one scrollable body.

use crate::app::layout;
use cairn_ui::brand;
use gpui_kit::component::dialog::DialogDescription;
use gpui_kit::component::v_flex;
use gpui_kit::gpui::*;

/// Seats the fixed surface in the vertical middle of the window, below the caption bar.
pub(crate) fn top_offset(window: &Window) -> Pixels {
    top_offset_for_height(
        window.viewport_size().height,
        window.rem_size() * layout::DIALOG_SEAT_HEIGHT,
        window.rem_size() * layout::SPACE_24,
    )
}

pub(crate) fn top_offset_for_height(
    viewport_height: Pixels,
    dialog_height: Pixels,
    minimum: Pixels,
) -> Pixels {
    ((viewport_height - dialog_height) / 2.).max(minimum)
}

/// One title recipe: heading role, serif face and script split. The runs are shaped as one
/// line, so Latin, CJK and digits share a baseline.
pub(crate) fn title(title: String, window: &Window) -> AnyElement {
    let color = window.text_style().color;
    let runs: Vec<TextRun> = brand::heading_runs(&title, FontWeight::SEMIBOLD)
        .into_iter()
        .map(|(len, font)| TextRun {
            len,
            font,
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        })
        .collect();
    div()
        .id("dialog-title")
        .role(Role::Heading)
        .aria_level(1)
        .aria_label(title.clone())
        .text_base()
        .line_height(relative(brand::SERIF_LINE_HEIGHT))
        .text_color(color)
        .child(StyledText::new(title).with_runs(runs))
        .into_any_element()
}

/// The title and explanation share one body. The dialog wraps this body in its scroll area, so a
/// long filename or a two-line explanation never clips inside the fixed surface.
pub(crate) fn body(title: String, description: String, window: &Window) -> AnyElement {
    v_flex()
        .w_full()
        .gap(rems(layout::SPACE_4))
        .child(self::title(title, window))
        .child(DialogDescription::new().child(description))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::top_offset_for_height;
    use crate::app::layout;
    use crate::shell::theme;
    use gpui_kit::gpui::px;

    #[test]
    fn dialogs_share_a_compact_seat() {
        assert!((layout::DIALOG_WIDTH * theme::BASE_FONT_PX - 384.0).abs() < 0.001);
        assert!((layout::DIALOG_SEAT_HEIGHT * theme::BASE_FONT_PX - 128.0).abs() < 0.001);
        assert_eq!(top_offset_for_height(px(800.), px(128.), px(24.)), px(336.));
        assert_eq!(top_offset_for_height(px(350.), px(128.), px(24.)), px(111.));
        assert_eq!(top_offset_for_height(px(100.), px(128.), px(24.)), px(24.));
    }
}

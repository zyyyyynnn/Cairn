// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Locations and drives sidebar view.

use crate::app::{element_id, layout, CairnApp};
use crate::search::filters::FilterCategory;
use crate::shell::accessibility::{self, ControlElement};
use crate::shell::commands::OpenSelected;
use crate::shell::theme::{self, ShellTokens};
use fluent_bundle::FluentArgs;
use gpui_kit::assets::{AllAssets, IconName};
use gpui_kit::base::StyledExt;
use gpui_kit::component::{h_flex, v_flex, Icon};
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::{AssetSource as _, *};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Copy)]
struct SidebarIconMetrics {
    size: Pixels,
    label_offset: Pixels,
}

/// How far the label's actual glyph ink sits below the centre of its line box.
/// The line box centres ascent and descent; ideographic ink sits at a different
/// height. The icon stays on the row centre; the label absorbs this difference.
fn text_ink_offset(line_ascent: Pixels, line_descent: Pixels, ink_center: Pixels) -> Pixels {
    (line_ascent - line_descent) / 2.0 - ink_center
}

/// The bundled 24-grid glyph keeps its stroke constant in view-box units, so
/// the larger 1rem box would also thicken the painted stroke. Serve a sidebar
/// variant whose 1.75-unit stroke returns the optical weight the glyph had at
/// 0.875rem without touching the same icons elsewhere in the shell.
fn sidebar_icon_svg(icon: IconName) -> Option<Arc<[u8]>> {
    type SvgCache = Mutex<HashMap<String, Option<Arc<[u8]>>>>;
    static CACHE: OnceLock<SvgCache> = OnceLock::new();
    let path = icon.path().to_string();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache.lock().ok()?;
    let bytes = cache.entry(path.clone()).or_insert_with(|| {
        let bytes = AllAssets.load(&path).ok().flatten()?;
        let svg = String::from_utf8_lossy(&bytes);
        Some(Arc::from(
            svg.replace("stroke-width=\"2\"", "stroke-width=\"1.75\"")
                .into_bytes(),
        ))
    });
    bytes.clone()
}

/// The 24-grid Lucide stroke is scaled by the larger icon box and reads heavier
/// than the 0.875rem label. Keep the approved 1rem box and restore the stroke
/// weight the icon had at 0.875rem. The vertical seat is derived from the
/// label's font metrics so the icon shares the text's optical centreline.
fn sidebar_icon_metrics(window: &Window) -> SidebarIconMetrics {
    let style = window.text_style();
    let font = style.font();
    let font_size = window.rem_size() * theme::SMALL_TEXT_SIZE;
    let text_system = window.text_system();
    let sample = "文";
    let layout = text_system.layout_line(
        sample,
        font_size,
        &[TextRun {
            len: sample.len(),
            font,
            color: style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    );
    let label_offset = layout
        .runs
        .first()
        .and_then(|run| {
            text_system
                .typographic_bounds(run.font_id, font_size, '文')
                .ok()
        })
        .map(|bounds| {
            let ink_center = bounds.origin.y + bounds.size.height / 2.0;
            -text_ink_offset(layout.ascent, layout.descent, ink_center)
        })
        .unwrap_or(Pixels::ZERO);
    SidebarIconMetrics {
        size: window.rem_size() * layout::SIDEBAR_ICON,
        label_offset,
    }
}

/// One fixed leading lane for every sidebar glyph. Keeping the lane fixed means
/// a larger icon changes the glyph, not the label's shared left edge.
fn sidebar_icon(icon: IconName, metrics: SidebarIconMetrics) -> Div {
    let glyph = match sidebar_icon_svg(icon) {
        Some(bytes) => Icon::new(icon).data(bytes.as_ref()),
        None => Icon::new(icon),
    };
    div()
        .w(metrics.size)
        .h(metrics.size)
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .child(glyph.size(metrics.size))
}

/// The visible surface of one sidebar row.
///
/// A plain row, not a `SidebarMenuItem`: the component renders its own `TreeItem` with a name and
/// selected state, and this app already wraps every row in the registered control that owns focus,
/// activation and the accessible name. Two nested controls announced the same row twice. GPUI has no
/// `aria_hidden` in this version, so the inner row keeps only its pixels; the wrapper is the single
/// accessible node. The surface copies the component's icon, selected fill and hover from the same
/// theme tokens.
fn sidebar_row(
    icon: IconName,
    label: impl Into<SharedString>,
    is_active: bool,
    tokens: &ShellTokens,
    icon_metrics: SidebarIconMetrics,
) -> Div {
    h_flex()
        .w_full()
        .min_h(rems(layout::ROW_HEIGHT))
        // The four-pixel vertical padding leaves a 20px content box for the 1rem
        // glyph and its line; the horizontal inset stays on the shared 12px spine.
        .px(rems(layout::INNER_SPACING))
        .py(rems(layout::TIGHT_LEAD))
        .gap_x(rems(layout::INNER_SPACING))
        .rounded(tokens.radius)
        .text_sm()
        .overflow_x_hidden()
        .when(is_active, |this| {
            this.bg(tokens.sidebar_accent)
                .text_color(tokens.sidebar_accent_foreground)
                .font_medium()
        })
        .child(sidebar_icon(icon, icon_metrics))
        .child(
            div()
                .relative()
                .top(icon_metrics.label_offset)
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .child(label.into()),
        )
}

/// The action a sidebar row runs when the pointer or the keyboard chooses it.
type SidebarActivate = dyn Fn(&mut CairnApp, &mut Window, &mut Context<CairnApp>);

/// A sidebar row the keyboard can reach.
///
/// The registered control wraps the visual row: the wrapper owns focus and activation, the single
/// accessible name and selected state, and paints its ring over the row instead of adding a border
/// that would move the label when focus arrives.
///
/// Enter and Space both arrive through the wrapper. Enter is the window's own `OpenSelected`
/// binding, so the wrapper handles that Action first and stops it before the window root could open
/// a result the person did not point at. Space has no binding: GPUI's own keyboard click turns the
/// focused element's `on_click` into the activation. A pointer click moves focus to this row before
/// activating it, so the ring and the next Enter belong to the row the person just pressed, not to
/// whichever row had focus before.
fn keyboard_row(
    row: ControlElement,
    focus: FocusHandle,
    is_focused: bool,
    tokens: &ShellTokens,
    content: impl IntoElement,
    activate: impl Fn(&mut CairnApp, &mut Window, &mut Context<CairnApp>) + 'static,
    cx: &mut Context<CairnApp>,
) -> AnyElement {
    let activate: Rc<SidebarActivate> = Rc::new(activate);
    let action_activate = activate.clone();
    let click_activate = activate;
    let click_focus = focus.clone();

    row.relative()
        .w_full()
        .flex_shrink_0()
        .rounded(tokens.radius)
        .cursor_pointer()
        // The wrapper is the interactive element, so it is the node GPUI hit-tests for hover. The
        // child row draws only the active fill; a hover style on that plain child stopped firing
        // once the shell moved to gpui-kit 0.8 and the navigation pane lost its pointer feedback.
        .hover(|this| {
            this.bg(tokens.row_hover)
                .text_color(tokens.sidebar_accent_foreground)
        })
        .track_focus(&focus)
        .on_action(cx.listener(move |this, _: &OpenSelected, window, cx| {
            cx.stop_propagation();
            action_activate(this, window, cx);
        }))
        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
            if matches!(event, ClickEvent::Mouse(_)) {
                this.results.keyboard_navigation = false;
                click_focus.focus(window, cx);
                cx.notify();
            }
            click_activate(this, window, cx);
        }))
        .child(content)
        .when(is_focused, |this| {
            this.child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(tokens.radius)
                    .border_1()
                    .border_color(tokens.ring),
            )
        })
        .into_any_element()
}

impl CairnApp {
    pub(crate) fn render_sidebar(
        &self,
        tokens: &ShellTokens,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        if self.sidebar_collapsed {
            return div()
                .id("cairn-sidebar-collapsed")
                .w_0()
                .overflow_hidden()
                .into_any_element();
        }

        let locations = accessibility::control("sidebar-locations");
        let drives = accessibility::control("sidebar-drives");
        let locations_title = accessibility::name(locations, &self.i18n);
        let drives_title = accessibility::name(drives, &self.i18n);
        let icon_metrics = sidebar_icon_metrics(window);

        let quick_access: Vec<AnyElement> = FilterCategory::menu_order()
            .iter()
            .map(|category| {
                let category = *category;
                let is_active = self.query.active_filter == category
                    && self.drive_view.is_none()
                    && !self.recycle_bin.is_open();
                let control =
                    accessibility::control_at(&element_id("sidebar-location", category.key()));
                let name = accessibility::name(control, &self.i18n);
                let focus = window
                    .use_keyed_state(
                        element_id("sidebar-location-focus", category.key()),
                        cx,
                        |_, cx| cx.focus_handle().tab_stop(true),
                    )
                    .read(cx)
                    .clone();
                let is_focused =
                    focus.contains_focused(window, cx) && self.results.keyboard_navigation;
                let item = sidebar_row(
                    category_icon(category),
                    self.i18n.get_text(category.label_key()),
                    is_active,
                    tokens,
                    icon_metrics,
                );

                keyboard_row(
                    accessibility::element(control)
                        .aria_label(name.clone())
                        .aria_selected(is_active),
                    focus,
                    is_focused,
                    tokens,
                    item,
                    move |this, _window, cx| this.set_filter(category, cx),
                    cx,
                )
            })
            .collect();

        // 回收站 is a location, not a search category: it lists what the shell
        // has binned and restores from there, so it closes the 位置 section.
        let bin_active = self.recycle_bin.is_open() && self.drive_view.is_none();
        let bin_control = accessibility::control("sidebar-recycle-bin");
        let bin_name = accessibility::name(bin_control, &self.i18n);
        let bin_focus = window
            .use_keyed_state("sidebar-recycle-bin-focus", cx, |_, cx| {
                cx.focus_handle().tab_stop(true)
            })
            .read(cx)
            .clone();
        let bin_is_focused =
            bin_focus.contains_focused(window, cx) && self.results.keyboard_navigation;
        let bin_row = sidebar_row(
            IconName::Recycle,
            self.i18n.get_text("sidebar_recycle_bin"),
            bin_active,
            tokens,
            icon_metrics,
        );
        let recycle_entry = keyboard_row(
            accessibility::element(bin_control)
                .aria_label(bin_name.clone())
                .aria_selected(bin_active),
            bin_focus,
            bin_is_focused,
            tokens,
            bin_row,
            |this, window, cx| this.open_recycle_bin(window, cx),
            cx,
        );

        let drive_rows: Vec<AnyElement> = self
            .service
            .status
            .volumes
            .iter()
            .enumerate()
            .map(|(ix, volume)| {
                let drive = volume.drive_letter.to_ascii_uppercase();
                let is_active = self.drive_view == Some(drive);
                let mut args = FluentArgs::new();
                args.set("drive", drive.to_string());
                let label = self.i18n.get_text_with_args("drive_label", &args);
                let state = self.service.status.volume_capsule_text(&self.i18n, volume);
                let accessible = format!("{label} {state}");
                let control = accessibility::control("sidebar-drive");
                let focus = window
                    .use_keyed_state(
                        element_id("sidebar-drive-focus", &drive.to_string()),
                        cx,
                        |_, cx| cx.focus_handle().tab_stop(true),
                    )
                    .read(cx)
                    .clone();
                let is_focused =
                    focus.contains_focused(window, cx) && self.results.keyboard_navigation;
                let item = sidebar_row(IconName::HardDrive, label, is_active, tokens, icon_metrics);

                keyboard_row(
                    accessibility::element_at(control, ix)
                        .aria_label(accessible)
                        .aria_selected(is_active),
                    focus,
                    is_focused,
                    tokens,
                    item,
                    move |this, window, cx| this.open_drive_visualization(drive, window, cx),
                    cx,
                )
            })
            .collect();

        // While the connection probe or the first query is still running, an empty drive list is
        // not a fact about the machine. Saying "no drives found" there reports a failure that has
        // not happened; the table's skeleton already says the index is being read.
        let awaiting_index =
            drive_rows.is_empty() && (self.service.is_probing_service || self.query.is_searching);
        let drive_elements = if awaiting_index {
            Vec::new()
        } else if drive_rows.is_empty() {
            let message = self.i18n.get_text("sidebar_no_drives");
            vec![h_flex()
                .w_full()
                .min_h(rems(layout::ROW_HEIGHT))
                .px(rems(layout::INNER_SPACING))
                .py(rems(layout::TIGHT_LEAD))
                .gap_x(rems(layout::INNER_SPACING))
                .items_center()
                .text_sm()
                .text_color(tokens.muted_foreground)
                .child(sidebar_icon(IconName::HardDrive, icon_metrics))
                .child(
                    div()
                        .relative()
                        .top(icon_metrics.label_offset)
                        .child(message),
                )
                .into_any_element()]
        } else {
            drive_rows
        };

        v_flex()
            .id("cairn-sidebar")
            .w(rems(layout::SIDEBAR_WIDTH))
            .min_w(rems(layout::SIDEBAR_MIN_WIDTH))
            .flex_shrink_0()
            .h_full()
            .bg(tokens.sidebar)
            .border_r_1()
            .border_color(tokens.border)
            // The sidebar is another content surface: any press in it leaves the result rows,
            // including a press on a row that is navigating away. The bin keeps its
            // identity-keyed selection across these presses.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.dismiss_result_selection_on_chrome_press(cx)),
            )
            .justify_between()
            .child(
                v_flex()
                    .id("sidebar-scroll-viewport")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.sidebar_scroll)
                    // The row's own INNER_SPACING supplies the rest of the lead, so icons and
                    // section labels start on the same 12px baseline as the content column. The
                    // same 4px viewport inset on the right keeps the row fill symmetric.
                    .pl(rems(layout::SIDEBAR_VIEWPORT_LEAD))
                    .pr(rems(layout::SIDEBAR_VIEWPORT_LEAD))
                    .py(rems(layout::SPACE_4))
                    .gap_y(rems(layout::SPACE_4))
                    .child(
                        accessibility::element(locations)
                            .aria_label(locations_title.clone())
                            .px(rems(layout::INNER_SPACING))
                            // The caption fills the search-band height so its optical centre and
                            // the first row share the search row and table-head bands.
                            .h(rems(layout::SEARCH_BAND_HEIGHT - 2.0 * layout::SPACE_4))
                            .flex()
                            .items_center()
                            .text_sm()
                            .font_medium()
                            .text_color(tokens.muted_foreground)
                            .child(locations_title),
                    )
                    .children(quick_access)
                    .child(recycle_entry)
                    .child(
                        accessibility::element(drives)
                            .aria_label(drives_title.clone())
                            .px(rems(layout::INNER_SPACING))
                            .pt(rems(layout::SPACE_8))
                            .pb(rems(layout::SPACE_4))
                            .text_sm()
                            .font_medium()
                            .text_color(tokens.muted_foreground)
                            .child(drives_title),
                    )
                    .children(drive_elements),
            )
            .into_any_element()
    }
}

/// The icon a file-type row shows. It repeats the words beside it rather than replacing them.
///
/// `main.rs` installs the complete icon catalog, so each kind can use its own shape instead of
/// the nearest default glyph.
fn category_icon(category: FilterCategory) -> IconName {
    match category {
        FilterCategory::All => IconName::LayoutDashboard,
        FilterCategory::Document => IconName::FileText,
        FilterCategory::Picture => IconName::FileImage,
        FilterCategory::Audio => IconName::FileMusic,
        FilterCategory::Video => IconName::FileVideoCamera,
        FilterCategory::Compressed => IconName::FileArchive,
        FilterCategory::Executable => IconName::FileCode,
        FilterCategory::Folder => IconName::Folder,
    }
}

#[cfg(test)]
mod tests {
    use super::{sidebar_icon_svg, text_ink_offset};
    use gpui_kit::assets::IconName;
    use gpui_kit::gpui::px;

    #[test]
    fn text_ink_offset_is_measured_from_the_line_centre() {
        assert_eq!(text_ink_offset(px(12.0), px(3.0), px(4.0)), px(0.5));
    }

    #[test]
    fn sidebar_variant_keeps_the_light_stroke() {
        let bytes = sidebar_icon_svg(IconName::FileText).expect("FileText asset");
        let svg = std::str::from_utf8(&bytes).expect("UTF-8 SVG asset");
        assert!(svg.contains("stroke-width=\"1.75\""));
        assert!(!svg.contains("stroke-width=\"2\""));
    }
}

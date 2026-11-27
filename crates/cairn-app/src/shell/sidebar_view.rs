// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Locations and organization sidebar view.

use crate::app::{CairnApp, element_id, layout};
use crate::search::filters::FilterCategory;
use crate::shell::accessibility::{self, ControlElement};
use crate::shell::commands::OpenSelected;
use crate::shell::folder_sources::{folder_source_summaries, source_state_label};
use crate::shell::theme::{self, ShellTokens};
use cairn_protocol::status::VolumeState;
use fluent_bundle::FluentArgs;
use gpui_kit::assets::IconName;
use gpui_kit::base::StyledExt;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::gpui::prelude::FluentBuilder as _;
use gpui_kit::gpui::{AssetSource as _, *};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

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
    thread_local! {
        static CACHE: RefCell<HashMap<SharedString, Option<Arc<[u8]>>>> =
            RefCell::new(HashMap::new());
    }
    let path = icon.path();
    CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache
            .entry(path.clone())
            .or_insert_with(|| {
                let bytes = crate::shell::assets::CairnAssets
                    .load(path.as_ref())
                    .ok()
                    .flatten()?;
                let svg = String::from_utf8_lossy(&bytes);
                Some(Arc::from(
                    svg.replace("stroke-width=\"2\"", "stroke-width=\"1.75\"")
                        .into_bytes(),
                ))
            })
            .clone()
    })
}

/// How far the sample glyph's ink centre sits below the centre of a line box at
/// the sidebar's label size. The line box centres ascent and descent; the ink
/// sits elsewhere, so every label and caption lifts by this one measurement.
fn glyph_ink_offset(window: &Window, sample: &str) -> Pixels {
    let Some(glyph) = sample.chars().next() else {
        return Pixels::ZERO;
    };
    let rem_bits = f32::from(window.rem_size()).to_bits();
    thread_local! {
        static OFFSET_CACHE: RefCell<HashMap<(u32, char), Pixels>> =
            RefCell::new(HashMap::new());
    }
    if let Some(cached) = OFFSET_CACHE.with(|cache| cache.borrow().get(&(rem_bits, glyph)).copied())
    {
        return cached;
    }
    let style = window.text_style();
    let font = style.font();
    let font_size = window.rem_size() * theme::SMALL_TEXT_SIZE;
    let text_system = window.text_system();
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
    let Some(run) = layout.runs.first() else {
        return Pixels::ZERO;
    };
    let Ok(bounds) = text_system.typographic_bounds(run.font_id, font_size, glyph) else {
        return Pixels::ZERO;
    };
    let ink_center = bounds.origin.y + bounds.size.height / 2.0;
    let offset = text_ink_offset(layout.ascent, layout.descent, ink_center);
    OFFSET_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() >= 32 {
            cache.clear();
        }
        cache.insert((rem_bits, glyph), offset);
    });
    offset
}

/// The 24-grid Lucide stroke is scaled by the larger icon box and reads heavier
/// than the 0.875rem label. Keep the approved 1rem box and restore the stroke
/// weight the icon had at 0.875rem. The vertical seat is derived from the
/// label's font metrics so the icon shares the text's optical centreline.
fn sidebar_icon_metrics(window: &Window) -> SidebarIconMetrics {
    SidebarIconMetrics {
        size: window.rem_size() * layout::SIDEBAR_ICON,
        label_offset: -glyph_ink_offset(window, "文"),
    }
}

/// The caption's ink sits a fraction below the centre of its line box.
/// Centre the line box, then lift the ink by the measured difference so the
/// label's optical centre lands on the query box's centreline at every zoom.
fn sidebar_caption_ink_offset(window: &Window, text: &str) -> Pixels {
    glyph_ink_offset(window, text)
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
    sidebar_row_with_trailing(icon, label, None, is_active, tokens, icon_metrics)
}

/// A sidebar row whose label can be followed by one compact state word. The label keeps the
/// remaining width and truncates; the trailing word stays whole, so a long root path never hides
/// the state it is in.
fn sidebar_row_with_trailing(
    icon: IconName,
    label: impl Into<SharedString>,
    trailing: Option<SharedString>,
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
        .when_some(trailing, |this, trailing| {
            this.child(
                div()
                    .relative()
                    .top(icon_metrics.label_offset)
                    .flex_shrink_0()
                    .whitespace_nowrap()
                    .text_size(rems(theme::SMALL_TEXT_SIZE))
                    .text_color(if is_active {
                        tokens.sidebar_accent_foreground
                    } else {
                        tokens.muted_foreground
                    })
                    .child(trailing),
            )
        })
}

/// A group caption: 位置 heads the location rows. The box fills the search-band height, which puts the
/// optical centre on the query box's centreline and the first row on the table heading.
fn sidebar_caption(
    control: &'static accessibility::KeyControl,
    title: SharedString,
    window: &Window,
    tokens: &ShellTokens,
) -> impl IntoElement + use<> {
    let ink_offset = sidebar_caption_ink_offset(window, title.as_ref());
    accessibility::element(control)
        .aria_label(title.clone())
        .px(rems(layout::INNER_SPACING))
        .h(rems(
            layout::SEARCH_BAND_HEIGHT - 2.0 * layout::SIDEBAR_VIEWPORT_LEAD,
        ))
        .flex_shrink_0()
        .flex()
        .items_center()
        .text_sm()
        .font_medium()
        .text_color(tokens.muted_foreground)
        .child(div().relative().top(-ink_offset).child(title))
}

/// The action a sidebar row runs when the pointer or the keyboard chooses it.
type SidebarActivate = dyn Fn(&mut CairnApp, &mut Window, &mut Context<'_, CairnApp>);

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
struct KeyboardRowChrome<'a> {
    control: ControlElement,
    focus: FocusHandle,
    keyboard_navigation: bool,
    tokens: &'a ShellTokens,
    window: &'a Window,
}

fn keyboard_row(
    chrome: KeyboardRowChrome<'_>,
    content: impl IntoElement,
    activate: impl Fn(&mut CairnApp, &mut Window, &mut Context<'_, CairnApp>) + 'static,
    cx: &mut Context<'_, CairnApp>,
) -> AnyElement {
    let KeyboardRowChrome {
        control: row,
        focus,
        keyboard_navigation,
        tokens,
        window,
    } = chrome;
    let activate: Rc<SidebarActivate> = Rc::new(activate);
    let action_activate = activate.clone();
    let click_activate = activate;
    let click_focus = focus.clone();

    row.relative()
        .w_full()
        .flex_shrink_0()
        .rounded(tokens.radius)
        // The interactive wrapper owns the GPUI hit-test area for hover feedback; the child row
        // draws only the active fill because hover styles on a non-interactive child do not fire.
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
                cairn_ui::focus::set_keyboard_navigation(false, cx);
                click_focus.focus(window, cx);
                cx.notify();
            }
            click_activate(this, window, cx);
        }))
        .child(content)
        .children(cairn_ui::focus::edge(
            &focus,
            tokens.ring,
            tokens.radius,
            keyboard_navigation,
            window,
            cx,
        ))
        .into_any_element()
}

impl CairnApp {
    pub(crate) fn render_sidebar(
        &self,
        tokens: &ShellTokens,
        sidebar_width: f32,
        window: &mut Window,
        cx: &mut Context<'_, Self>,
    ) -> AnyElement {
        if sidebar_width <= 0.5 {
            return div()
                .id("cairn-sidebar-collapsed")
                .track_focus(&self.sidebar_focus)
                .w_0()
                .overflow_hidden()
                .into_any_element();
        }

        let locations = accessibility::control("sidebar-locations");
        let locations_title = self.cached_control_name(locations);
        let icon_metrics = sidebar_icon_metrics(window);

        let location = self.active_location();
        let quick_access: Vec<AnyElement> = FilterCategory::menu_order()
            .iter()
            .map(|category| {
                let category = *category;
                let is_active = self.query.active_filter == category && location.is_index();
                let control =
                    accessibility::control_at(&element_id("sidebar-location", category.key()));
                let name = self.cached_control_name(control);
                let focus = window
                    .use_keyed_state(
                        element_id("sidebar-location-focus", category.key()),
                        cx,
                        |_, cx| cx.focus_handle().tab_stop(true),
                    )
                    .read(cx)
                    .clone();
                let keyboard_nav = cairn_ui::focus::keyboard_navigation(cx);
                let item = sidebar_row(
                    category_icon(category),
                    self.cached_text(category.label_key()),
                    is_active,
                    tokens,
                    icon_metrics,
                );

                keyboard_row(
                    KeyboardRowChrome {
                        control: accessibility::element(control)
                            .aria_label(name)
                            .aria_selected(is_active),
                        focus,
                        keyboard_navigation: keyboard_nav,
                        tokens,
                        window,
                    },
                    item,
                    move |this, _window, cx| this.set_filter(category, cx),
                    cx,
                )
            })
            .collect();

        // 回收站 is a location, not a search category: it lists what the shell
        // has binned and restores from there, so it closes the 位置 section.
        let bin_active = location.is_recycle_bin();
        let bin_control = accessibility::control("sidebar-recycle-bin");
        let bin_name = self.cached_control_name(bin_control);
        let bin_focus = window
            .use_keyed_state("sidebar-recycle-bin-focus", cx, |_, cx| {
                cx.focus_handle().tab_stop(true)
            })
            .read(cx)
            .clone();
        let bin_keyboard_nav = cairn_ui::focus::keyboard_navigation(cx);
        let bin_row = sidebar_row(
            IconName::Recycle,
            self.cached_text("sidebar_recycle_bin"),
            bin_active,
            tokens,
            icon_metrics,
        );
        let recycle_entry = keyboard_row(
            KeyboardRowChrome {
                control: accessibility::element(bin_control)
                    .aria_label(bin_name)
                    .aria_selected(bin_active),
                focus: bin_focus,
                keyboard_navigation: bin_keyboard_nav,
                tokens,
                window,
            },
            bin_row,
            Self::open_recycle_bin,
            cx,
        );

        // 文件夹源 appears only while a folder source is registered; its rows scope the current
        // query, and the manager entry at the end owns add/rescan/remove.
        let folder_sources: Vec<_> = folder_source_summaries(&self.service.sources).collect();
        let folder_section = if folder_sources.is_empty() {
            None
        } else {
            let heading = accessibility::control("sidebar-folder-sources");
            let heading_title = self.cached_control_name(heading);
            let rows: Vec<AnyElement> = folder_sources
                .into_iter()
                .map(|source| {
                    let source_id = source.source_id;
                    let is_active =
                        self.query.source_scope == Some(source_id) && location.is_index();
                    let control = accessibility::control("sidebar-folder-source");
                    let mut args = FluentArgs::new();
                    args.set("path", source.root_path.as_str());
                    args.set("state", source_state_label(&self.i18n, source.status));
                    let name = self
                        .i18n
                        .get_text_with_args("folder_source_row_label", &args);
                    let focus = window
                        .use_keyed_state(
                            element_id("sidebar-folder-source-focus", &source_id.0.to_string()),
                            cx,
                            |_, cx| cx.focus_handle().tab_stop(true),
                        )
                        .read(cx)
                        .clone();
                    let keyboard_nav = cairn_ui::focus::keyboard_navigation(cx);
                    let row = sidebar_row_with_trailing(
                        IconName::Folder,
                        source.root_path.clone(),
                        Some(source_state_label(&self.i18n, source.status).into()),
                        is_active,
                        tokens,
                        icon_metrics,
                    );
                    keyboard_row(
                        KeyboardRowChrome {
                            control: accessibility::element_at(control, source_id.0 as usize)
                                .aria_label(name)
                                .aria_selected(is_active),
                            focus,
                            keyboard_navigation: keyboard_nav,
                            tokens,
                            window,
                        },
                        row,
                        move |this, _window, cx| this.select_folder_source(source_id, cx),
                        cx,
                    )
                })
                .collect();

            let manage_control = accessibility::control("sidebar-manage-folder-sources");
            let manage_name = self.cached_control_name(manage_control);
            let manage_focus = window
                .use_keyed_state("sidebar-manage-folder-sources-focus", cx, |_, cx| {
                    cx.focus_handle().tab_stop(true)
                })
                .read(cx)
                .clone();
            let manage_keyboard_nav = cairn_ui::focus::keyboard_navigation(cx);
            let manage_row = keyboard_row(
                KeyboardRowChrome {
                    control: accessibility::element(manage_control).aria_label(manage_name.clone()),
                    focus: manage_focus,
                    keyboard_navigation: manage_keyboard_nav,
                    tokens,
                    window,
                },
                sidebar_row(
                    IconName::FolderCog,
                    manage_name,
                    false,
                    tokens,
                    icon_metrics,
                ),
                Self::open_folder_sources_dialog,
                cx,
            );

            Some(
                v_flex()
                    .w_full()
                    .gap_y(rems(layout::SPACE_4))
                    .child(sidebar_caption(heading, heading_title, window, tokens))
                    .children(rows)
                    .child(manage_row),
            )
        };

        // Surface non-synced or offline volumes in one compact warning line carrying the worst
        // state and the count of remaining lagging volumes.
        let unsynced: Vec<_> = self
            .service
            .status
            .volumes
            .iter()
            .filter(|volume| volume.status != VolumeState::Synced)
            .collect();
        let index_state = unsynced
            .iter()
            .min_by_key(|volume| match volume.status {
                VolumeState::Offline => 0,
                VolumeState::Rebuilding => 1,
                VolumeState::CatchingUp(_) => 2,
                VolumeState::SnapshotMode => 3,
                VolumeState::Synced => 4,
            })
            .map(|volume| {
                let mut args = FluentArgs::new();
                args.set("volume", format!("{}:", volume.drive_letter));
                args.set(
                    "state",
                    self.i18n
                        .get_text(&format!("volume_state_{}", volume.status.state_key())),
                );
                let text = if unsynced.len() > 1 {
                    args.set("count", (unsynced.len() - 1) as i64);
                    self.i18n
                        .get_text_with_args("sidebar_index_state_more", &args)
                } else {
                    self.i18n.get_text_with_args("sidebar_index_state", &args)
                };
                h_flex()
                    .id("sidebar-index-state")
                    .role(Role::Status)
                    .aria_label(text.clone())
                    .w_full()
                    .min_h(rems(layout::ROW_HEIGHT))
                    .px(rems(layout::INNER_SPACING))
                    .py(rems(layout::TIGHT_LEAD))
                    .gap_x(rems(layout::INNER_SPACING))
                    .items_center()
                    .text_sm()
                    .text_color(tokens.warning_ink)
                    .child(sidebar_icon(IconName::TriangleAlert, icon_metrics))
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .truncate()
                            .relative()
                            .top(icon_metrics.label_offset)
                            .child(text),
                    )
                    .into_any_element()
            });

        // The content keeps its full width while the outer surface clips it, so
        // labels and rows never reflow during a collapse or an expand.
        let full_width = f32::from(theme::rem(window) * layout::SIDEBAR_WIDTH);
        let opacity = (sidebar_width / full_width.max(1.0)).clamp(0.0, 1.0);
        let sidebar = v_flex()
            .id("cairn-sidebar")
            .w(rems(layout::SIDEBAR_WIDTH))
            .h_full()
            .justify_between()
            .child(
                v_flex()
                    .id("sidebar-scroll-viewport")
                    .w(rems(layout::SIDEBAR_WIDTH))
                    .flex_shrink_0()
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.sidebar_scroll)
                    // The row's own INNER_SPACING supplies the rest of the lead, so icons and
                    // section labels start on the same 12px baseline as the content column. The
                    // same 4px viewport inset on the right keeps the row fill symmetric.
                    .pl(rems(layout::SIDEBAR_VIEWPORT_LEAD))
                    .pr(rems(layout::SIDEBAR_VIEWPORT_LEAD))
                    .py(rems(layout::SIDEBAR_VIEWPORT_LEAD))
                    .gap_y(rems(layout::SPACE_4))
                    .child(sidebar_caption(locations, locations_title, window, tokens))
                    .children(quick_access)
                    .child(recycle_entry)
                    .when_some(folder_section, gpui_kit::ParentElement::child)
                    .when_some(index_state, gpui_kit::ParentElement::child),
            );

        div()
            .id("cairn-sidebar-surface")
            .track_focus(&self.sidebar_focus)
            .w(px(sidebar_width))
            .h_full()
            .flex_shrink_0()
            .overflow_hidden()
            .bg(tokens.sidebar)
            .border_r_1()
            .border_color(tokens.border)
            .opacity(opacity)
            // The sidebar is another content surface: any press in it leaves the result rows,
            // including a press on a row that is navigating away. The bin keeps its
            // identity-keyed selection across these presses.
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.dismiss_result_selection_on_chrome_press(cx)),
            )
            .child(sidebar)
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
    use super::sidebar_icon_svg;
    use crate::shell::i18n::I18nManager;
    use cairn_protocol::status::VolumeState;
    use gpui_kit::assets::IconName;

    #[test]
    fn every_non_synced_volume_state_has_a_sidebar_label() {
        for locale in ["zh-CN", "en-US"] {
            let i18n = I18nManager::new(locale);
            for state in [
                VolumeState::CatchingUp(0),
                VolumeState::Rebuilding,
                VolumeState::Offline,
                VolumeState::SnapshotMode,
            ] {
                let key = format!("volume_state_{}", state.state_key());
                assert!(i18n.has_key(&key), "{locale} is missing {key}");
            }
            for key in ["sidebar_index_state", "sidebar_index_state_more"] {
                assert!(i18n.has_key(key), "{locale} is missing {key}");
            }
            let mut args = fluent_bundle::FluentArgs::new();
            args.set("volume", "C:");
            args.set("state", "syncing");
            args.set("count", 1i64);
            let one = i18n.get_text_with_args("sidebar_index_state_more", &args);
            args.set("count", 2i64);
            let many = i18n.get_text_with_args("sidebar_index_state_more", &args);
            if locale == "en-US" {
                assert!(one.contains("one more volume"), "{locale}: {one}");
                assert!(many.contains("more volumes"), "{locale}: {many}");
            } else {
                assert!(one.contains("另有"), "{locale}: {one}");
                assert!(many.contains("另有"), "{locale}: {many}");
            }
        }
    }

    #[test]
    fn sidebar_variant_keeps_the_light_stroke() {
        let bytes = sidebar_icon_svg(IconName::FileText).expect("FileText asset");
        let svg = std::str::from_utf8(&bytes).expect("UTF-8 SVG asset");
        assert!(svg.contains("stroke-width=\"1.75\""));
        assert!(!svg.contains("stroke-width=\"2\""));
    }
}

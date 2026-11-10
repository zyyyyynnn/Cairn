// Cairn - High-performance Windows file search engine
// Copyright (c) 2026 Cairn Contributors
// SPDX-License-Identifier: Apache-2.0

//! Context menus for Treemap blocks: the actions a directory, file or summary
//! block offers.

use crate::app::CairnApp;
use crate::shell::os::{open_file, open_folder_and_select, show_properties_dialog};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::gpui::*;
use std::path::Path;
use std::rc::Rc;

/// The menu a summary block can honour: it lists its owning directory in the table.
pub(super) fn build_block_context_menu(
    view: Entity<CairnApp>,
    parent_path: Option<String>,
    i18n: &Rc<crate::shell::i18n::I18nManager>,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    let i18n = i18n.clone();
    move |menu: PopupMenu, _window: &mut Window, cx: &mut Context<PopupMenu>| {
        let dismiss_view = view.clone();
        cx.subscribe_self::<DismissEvent>(move |_menu, _event, cx| {
            dismiss_view.update(cx, |this, cx| {
                if let Some(st) = this.treemap_state.as_mut() {
                    st.suppress_tooltip = false;
                }
                cx.notify();
            });
        })
        .detach();
        let release_view = view.clone();
        cx.on_release(move |_menu, cx| {
            release_view.update(cx, |this, cx| {
                if let Some(st) = this.treemap_state.as_mut() {
                    st.suppress_tooltip = false;
                }
                cx.notify();
            });
        })
        .detach();
        let table_view = view.clone();
        let table_parent = parent_path.clone();
        menu.item(PopupMenuItem::new(i18n.get_text("view_in_table")).on_click(
            move |_, window, cx| {
                if let Some(ref dir) = table_parent {
                    let dir = dir.clone();
                    table_view.update(cx, |this, cx| {
                        this.switch_to_table_with_path(&dir, window, cx);
                    });
                }
            },
        ))
    }
}

pub(super) fn build_context_menu(
    view: Entity<CairnApp>,
    full_path: String,
    is_dir: bool,
    i18n: &Rc<crate::shell::i18n::I18nManager>,
) -> impl Fn(PopupMenu, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
    let i18n = i18n.clone();
    move |menu: PopupMenu, _window: &mut Window, cx: &mut Context<PopupMenu>| {
        let dismiss_view = view.clone();
        cx.subscribe_self::<DismissEvent>(move |_menu, _event, cx| {
            dismiss_view.update(cx, |this, cx| {
                if let Some(st) = this.treemap_state.as_mut() {
                    st.suppress_tooltip = false;
                }
                cx.notify();
            });
        })
        .detach();
        let release_view = view.clone();
        cx.on_release(move |_menu, cx| {
            release_view.update(cx, |this, cx| {
                if let Some(st) = this.treemap_state.as_mut() {
                    st.suppress_tooltip = false;
                }
                cx.notify();
            });
        })
        .detach();

        let file_name = Path::new(&full_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(&full_path)
            .to_string();

        let table_dir = if is_dir {
            full_path.clone()
        } else {
            Path::new(&full_path)
                .parent()
                .and_then(|p| p.to_str())
                .unwrap_or(&full_path)
                .to_string()
        };

        let open_view = view.clone();
        let reveal_view = view.clone();
        let copy_path_view = view.clone();
        let copy_name_view = view.clone();
        let delete_view = view.clone();
        let properties_view = view.clone();
        let table_view = view.clone();
        let open_path = full_path.clone();
        let reveal_path = full_path.clone();
        let copy_path = full_path.clone();
        let delete_path = full_path.clone();
        let properties_path = full_path.clone();

        menu.item(
            PopupMenuItem::new(i18n.get_text("ctx_open")).on_click(move |_, _, cx| {
                open_view.update(cx, |_, _| {
                    let _ = open_file(&open_path);
                });
            }),
        )
        .item(
            PopupMenuItem::new(i18n.get_text("ctx_open_path")).on_click(move |_, _, cx| {
                reveal_view.update(cx, |_, _| {
                    let _ = open_folder_and_select(&reveal_path);
                });
            }),
        )
        .item(
            PopupMenuItem::new(i18n.get_text("ctx_copy_path")).on_click(move |_, _, cx| {
                copy_path_view.update(cx, |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(copy_path.clone()));
                });
            }),
        )
        .item(
            PopupMenuItem::new(i18n.get_text("ctx_copy_name")).on_click(move |_, _, cx| {
                copy_name_view.update(cx, |_, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(file_name.clone()));
                });
            }),
        )
        .separator()
        .item(
            PopupMenuItem::new(i18n.get_text("ctx_delete")).on_click(move |_, window, cx| {
                delete_view.update(cx, |this, cx| {
                    this.confirm_delete_paths(vec![delete_path.clone()], window, cx);
                });
            }),
        )
        .separator()
        .item(
            PopupMenuItem::new(i18n.get_text("view_in_table")).on_click(move |_, window, cx| {
                table_view.update(cx, |this, cx| {
                    this.switch_to_table_with_path(&table_dir, window, cx);
                });
            }),
        )
        .separator()
        .item(
            PopupMenuItem::new(i18n.get_text("ctx_properties")).on_click(move |_, _, cx| {
                properties_view.update(cx, |_, _| {
                    let _ = show_properties_dialog(&properties_path);
                });
            }),
        )
    }
}

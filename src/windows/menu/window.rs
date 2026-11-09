use std::cell::Cell;

use gpui::prelude::*;
use gpui::{
    AnyElement, AnyWindowHandle, App, AvailableSpace, Context, IntoElement, MouseButton, Render,
    ScrollHandle, Window, div, px, rgb, size,
};
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::{ActiveTheme as _, ThemeColor};

use crate::{platform::window, ui::hover_icon_path};

use super::{
    animation::{
        MenuEntranceDirection, MenuPopupEntrance, point_anchored_content_bounds,
        submenu_content_bounds, vertical_entrance_direction,
    },
    layout::{
        MENU_ITEM_HEIGHT, MENU_MIN_WIDTH, MENU_PADDING_Y, MENU_SEPARATOR_HEIGHT, MenuDirection,
        MenuRect, fit_root_bounds, fit_submenu_bounds, max_menu_height, submenu_item_bounds,
    },
    model::{MenuItem, MenuItemKind, MenuModel, MenuPosition},
    service::{MenuActionHandler, MenuService, SubmenuRequest, open_popup_window},
};

pub(super) enum MenuPlacement {
    Root {
        anchor: MenuPosition,
    },
    Submenu {
        parent_menu: MenuModel,
        parent_bounds: MenuRect,
        item_index: usize,
        preferred_direction: MenuDirection,
        scale_factor: f32,
        scroll_offset_y: f32,
    },
}

impl MenuPlacement {
    pub(super) fn anchor(&self) -> MenuPosition {
        match self {
            Self::Root { anchor } => *anchor,
            Self::Submenu {
                parent_bounds,
                scale_factor,
                ..
            } => MenuPosition::new(parent_bounds.x, parent_bounds.y, *scale_factor),
        }
    }
}

pub(super) struct MenuWindowView {
    generation: u64,
    level: usize,
    menu: MenuModel,
    placement: MenuPlacement,
    entrance: MenuPopupEntrance,
    resolved_direction: Cell<Option<MenuDirection>>,
    active_child: Cell<Option<usize>>,
    scroll_handle: ScrollHandle,
    on_action: MenuActionHandler,
    popup: window::BorderlessPopup,
    target_display: Option<window::DisplayPlacement>,
}

impl MenuWindowView {
    pub(super) fn new(
        generation: u64,
        level: usize,
        menu: MenuModel,
        placement: MenuPlacement,
        on_action: MenuActionHandler,
        popup: window::BorderlessPopup,
        target_display: Option<window::DisplayPlacement>,
        cx: &mut Context<Self>,
    ) -> Self {
        #[cfg(target_os = "windows")]
        cx.observe_global::<crate::services::updates::UpdateService>(|view, cx| {
            if crate::services::tray::refresh_update_menu(&mut view.menu, cx) {
                cx.notify();
            }
        })
        .detach();
        #[cfg(not(target_os = "windows"))]
        let _ = cx;
        Self {
            generation,
            level,
            menu,
            placement,
            entrance: MenuPopupEntrance::default(),
            resolved_direction: Cell::new(None),
            active_child: Cell::new(None),
            scroll_handle: ScrollHandle::new(),
            on_action,
            popup,
            target_display,
        }
    }

    fn display_scale_factor(&self, window: &Window) -> f32 {
        self.target_display
            .map(|display| display.scale_factor)
            .unwrap_or_else(|| window.scale_factor().max(1.0))
    }

    fn on_item_hover(&self, index: usize, hovered: bool, window: &Window, cx: &mut Context<Self>) {
        if !hovered {
            return;
        }
        let Some(item) = self.menu.items().get(index) else {
            return;
        };
        if item.has_children() && item.enabled {
            self.active_child.set(Some(index));
            self.open_child_menu(index, self.display_scale_factor(window), cx);
        } else {
            self.active_child.set(None);
            self.close_child_menu(cx);
        }
        cx.notify();
    }

    fn open_child_menu(&self, index: usize, scale_factor: f32, cx: &mut Context<Self>) {
        let Some(item) = self.menu.items().get(index) else {
            return;
        };
        let Some(parent_bounds) = self.entrance.target_bounds() else {
            return;
        };
        let preferred_direction = self
            .resolved_direction
            .get()
            .unwrap_or(MenuDirection::Right);
        let menu = MenuModel::new(item.children.clone());
        let parent_menu = self.menu.clone();
        let generation = self.generation;
        let level = self.level + 1;
        let on_action = self.on_action.clone();
        let scroll_offset_y = f32::from(self.scroll_handle.offset().y);
        cx.defer(move |cx| {
            MenuService::open_submenu(
                SubmenuRequest {
                    generation,
                    level,
                    menu,
                    parent_menu,
                    parent_bounds,
                    item_index: index,
                    preferred_direction,
                    scale_factor,
                    scroll_offset_y,
                    on_action,
                },
                cx,
            );
        });
    }

    fn close_child_menu(&self, cx: &mut Context<Self>) {
        let level = self.level + 1;
        let generation = self.generation;
        cx.defer(move |cx| {
            cx.update_global::<MenuService, _>(|service, cx| {
                if service.is_generation_current(generation) {
                    service.close_from_level(level, cx);
                }
            });
        });
    }

    fn click_item(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(item) = self.menu.items().get(index) else {
            return;
        };
        // action_submenu 既有子菜单又可点击：有 action id 的子菜单父项允许触发，
        // 纯子菜单（id 为 None）仍跳过。
        if !item.enabled || item.kind == MenuItemKind::Separator {
            return;
        }
        if item.has_children() && item.id.is_none() {
            return;
        }
        let Some(action_id) = item.id.clone() else {
            return;
        };
        let close_on_click = item.close_on_click;
        window.prevent_default();
        let on_action = self.on_action.clone();
        if close_on_click {
            window.remove_window();
            let generation = self.generation;
            cx.defer(move |cx| {
                MenuService::close_generation(cx, generation);
                on_action(action_id, cx);
            });
        } else {
            if let Some(item) = self.menu.item_mut(index) {
                item.check_state = item.check_state.toggled();
            }
            cx.notify();
            cx.defer(move |cx| on_action(action_id, cx));
        }
    }

    fn measure_content_size(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Size<gpui::Pixels> {
        let scale_factor = self.display_scale_factor(window);
        let max_height = logical_menu_max_height(window, cx, scale_factor, self.target_display);
        menu_shell(
            self.render_items(cx),
            crate::window_material::menu_window_colors(cx),
            max_height,
            self.level,
            &self.scroll_handle,
        )
        .into_any_element()
        .layout_as_root(
            size(AvailableSpace::MinContent, AvailableSpace::MinContent),
            window,
            cx,
        )
    }

    fn place_window(
        &self,
        measured_size: gpui::Size<gpui::Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let scale_factor = self.display_scale_factor(window);
        let screen = screen_bounds(window, cx, scale_factor, self.target_display);
        let size = (
            (f32::from(measured_size.width) * scale_factor).ceil(),
            (f32::from(measured_size.height) * scale_factor)
                .ceil()
                .min(max_menu_height(screen)),
        );
        let (content_bounds, entrance_direction) = match &self.placement {
            MenuPlacement::Root { anchor } => {
                let bounds = fit_root_bounds(*anchor, size, screen);
                (bounds, vertical_entrance_direction(anchor.y, bounds))
            }
            MenuPlacement::Submenu {
                parent_menu,
                parent_bounds,
                item_index,
                preferred_direction,
                scroll_offset_y,
                ..
            } => {
                let (bounds, direction) = fit_submenu_bounds(
                    *parent_bounds,
                    parent_menu,
                    *item_index,
                    size,
                    screen,
                    scale_factor * crate::config_runtime::appearance_config(cx).ui_scale(),
                    *scroll_offset_y / crate::config_runtime::appearance_config(cx).ui_scale(),
                    *preferred_direction,
                );
                self.resolved_direction.set(Some(direction));
                (
                    bounds,
                    match direction {
                        MenuDirection::Left => MenuEntranceDirection::Left,
                        MenuDirection::Right => MenuEntranceDirection::Right,
                    },
                )
            }
        };

        let initial_bounds = match &self.placement {
            MenuPlacement::Root { anchor } => {
                point_anchored_content_bounds(content_bounds, anchor.x, anchor.y, 0.0)
            }
            MenuPlacement::Submenu {
                parent_menu,
                parent_bounds,
                item_index,
                scale_factor,
                scroll_offset_y,
                ..
            } => {
                let item_bounds = submenu_item_bounds(
                    *parent_bounds,
                    parent_menu,
                    *item_index,
                    *scale_factor * crate::config_runtime::appearance_config(cx).ui_scale(),
                    *scroll_offset_y / crate::config_runtime::appearance_config(cx).ui_scale(),
                );
                submenu_content_bounds(content_bounds, item_bounds, entrance_direction, 0.0)
            }
        };
        self.entrance.update_window(
            self.popup,
            self.generation,
            self.level,
            content_bounds,
            initial_bounds,
            window,
            cx,
        );
    }

    fn render_items(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.menu
            .items()
            .iter()
            .enumerate()
            .map(|(index, item)| self.render_item(index, item, cx))
            .collect()
    }

    fn render_item(&self, index: usize, item: &MenuItem, cx: &mut Context<Self>) -> AnyElement {
        if item.kind == MenuItemKind::Separator {
            return div()
                .h(gpui::rems(MENU_SEPARATOR_HEIGHT / 13.0))
                .px(gpui::rems(3.0 / 13.0))
                .flex()
                .items_center()
                .child(
                    div()
                        .h(gpui::rems(1.0 / 13.0))
                        .w_full()
                        .bg(cx.theme().border),
                )
                .into_any_element();
        }

        let enabled = item.enabled;
        let colors = cx.theme().colors;
        let foreground = if enabled {
            if item.success {
                colors.success
            } else {
                colors.popover_foreground
            }
        } else {
            colors.muted_foreground
        };
        let is_active = self.active_child.get() == Some(index);
        let icon = item.icon.clone();
        let check_icon_path = item.check_state.icon_path();
        let icon_color = icon.as_ref().and_then(|icon| icon.color);
        let icon_path = icon.as_ref().map(|icon| icon.path.clone());
        let icon_group = format!("qcclip-menu-item-icon-{}-{index}", self.level);
        let label = item.label.clone();
        let has_children = item.has_children();
        div()
            .id(("qcclip-menu-item", self.level as u64 * 1000 + index as u64))
            .group(icon_group.clone())
            .w_full()
            .h(gpui::rems(MENU_ITEM_HEIGHT / 13.0))
            .rounded(px(4.0))
            .flex()
            .items_center()
            .cursor_pointer()
            .text_color(foreground)
            .when(is_active, |style| style.bg(colors.accent))
            .when(!enabled, |style| style.opacity(0.5))
            .hover(|style| style.bg(colors.accent))
            .on_hover(cx.listener(move |this, hovered, window, cx| {
                this.on_item_hover(index, *hovered, window, cx);
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.click_item(index, window, cx);
                }),
            )
            .when(!item.centered, |view| {
                view.child(
                    div()
                        .w(gpui::rems(28.0 / 13.0))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .when_some(icon_color, |view, color| {
                            view.child(
                                div()
                                    .size(gpui::rems(8.0 / 13.0))
                                    .rounded_full()
                                    .bg(rgb(color)),
                            )
                        })
                        .when(icon_color.is_none(), |view| {
                            view.when_some(icon_path, |view, path| {
                                // 绝对路径用 img() 渲染，文件名用内置 svg 图标。
                                let icon_element: gpui::AnyElement =
                                    if std::path::Path::new(path.as_ref()).is_absolute() {
                                        gpui::img(std::path::PathBuf::from(path.as_ref()))
                                            .size(gpui::rems(12.0 / 13.0))
                                            .object_fit(gpui::ObjectFit::Contain)
                                            .into_any_element()
                                    } else {
                                        hover_icon_path(
                                            icon_group.clone(),
                                            format!("assets/icons/{}", path),
                                            12.0,
                                            foreground,
                                        )
                                        .into_any_element()
                                    };
                                view.child(icon_element)
                            })
                        }),
                )
            })
            .child(
                div()
                    .whitespace_nowrap()
                    .when(item.centered, |view| view.flex_1().text_center().px_2())
                    .when(!item.centered, |view| view.flex_none())
                    .child(label),
            )
            .when(!item.centered, |view| {
                view.child(div().flex_1().min_w(gpui::rems(12.0 / 13.0)))
            })
            .when(!item.centered, |view| {
                view.child(
                    div()
                        .flex_none()
                        .w(gpui::rems(22.0 / 13.0))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .when_some(check_icon_path, |view, path| {
                            view.child(hover_icon_path(icon_group.clone(), path, 12.0, foreground))
                        }),
                )
            })
            .when(has_children, |view| {
                view.child(
                    div()
                        .flex_none()
                        .w(gpui::rems(22.0 / 13.0))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(hover_icon_path(
                            icon_group,
                            "assets/icons/chevron-right.svg",
                            12.0,
                            foreground,
                        )),
                )
            })
            .into_any_element()
    }
}

impl Render for MenuWindowView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(crate::ui::scale::standalone_rem_size(13.0, cx));
        let measured_size = self.measure_content_size(window, cx);
        self.place_window(measured_size, window, cx);
        let scale_factor = self.display_scale_factor(window);
        let max_height = logical_menu_max_height(window, cx, scale_factor, self.target_display);
        let colors = crate::window_material::menu_window_colors(cx);
        let target_bounds = self.entrance.target_bounds();
        let menu = menu_shell(
            self.render_items(cx),
            colors,
            max_height,
            self.level,
            &self.scroll_handle,
        )
        .when_some(target_bounds, |menu, bounds| {
            menu.w(px(bounds.width / scale_factor))
                .h(px(bounds.height / scale_factor))
        });
        let content = if let Some(target_bounds) = target_bounds {
            let window_bounds = self.entrance.current_bounds().unwrap_or(target_bounds);
            let left = (target_bounds.x - window_bounds.x) / scale_factor;
            let top = (target_bounds.y - window_bounds.y) / scale_factor;
            div()
                .id(("qcclip-menu-window", self.level as u64))
                .size_full()
                .relative()
                .overflow_hidden()
                .child(
                    div()
                        .absolute()
                        .left(px(left))
                        .top(px(top))
                        .w(px(target_bounds.width / scale_factor))
                        .h(px(target_bounds.height / scale_factor))
                        .child(menu),
                )
        } else {
            div()
                .id(("qcclip-menu-window", self.level as u64))
                .size_full()
                .child(menu)
        };
        crate::window_material::with_background(content, window, cx)
    }
}

fn menu_shell(
    items: Vec<AnyElement>,
    colors: ThemeColor,
    max_height: f32,
    level: usize,
    scroll_handle: &ScrollHandle,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(("qcclip-menu-shell", level as u64))
        .relative()
        .child(
            menu_surface(colors)
                .id(("qcclip-menu-surface", level as u64))
                .min_w(gpui::rems(MENU_MIN_WIDTH / 13.0))
                .max_h(px(max_height))
                .px(gpui::rems(5.0 / 13.0))
                .py(gpui::rems(MENU_PADDING_Y / 13.0))
                .text_size(gpui::rems(13.0 / 13.0))
                .text_color(colors.popover_foreground)
                .overflow_y_scroll()
                .track_scroll(scroll_handle)
                .children(items),
        )
        .vertical_scrollbar(scroll_handle)
}

pub(super) fn menu_surface(colors: ThemeColor) -> gpui::Div {
    div().w_full().bg(colors.popover)
}

fn screen_bounds(
    window: &Window,
    cx: &Context<MenuWindowView>,
    scale_factor: f32,
    target_display: Option<window::DisplayPlacement>,
) -> MenuRect {
    target_display
        .map(|display| {
            let bounds = display.bounds;
            MenuRect::new(bounds.x, bounds.y, bounds.width, bounds.height)
        })
        .or_else(|| {
            window
                .display(cx)
                .or_else(|| cx.primary_display())
                .map(|display| {
                    let bounds = display.bounds();
                    MenuRect::new(
                        f32::from(bounds.origin.x) * scale_factor,
                        f32::from(bounds.origin.y) * scale_factor,
                        f32::from(bounds.size.width) * scale_factor,
                        f32::from(bounds.size.height) * scale_factor,
                    )
                })
        })
        .unwrap_or_else(|| MenuRect::new(0.0, 0.0, 1920.0, 1080.0))
}

fn logical_menu_max_height(
    window: &Window,
    cx: &Context<MenuWindowView>,
    scale_factor: f32,
    target_display: Option<window::DisplayPlacement>,
) -> f32 {
    let screen = screen_bounds(window, cx, scale_factor, target_display);
    max_menu_height(screen) / scale_factor
}

pub(super) fn open_context_window(
    generation: u64,
    level: usize,
    menu: MenuModel,
    placement: MenuPlacement,
    on_action: MenuActionHandler,
    cx: &mut App,
) -> Option<AnyWindowHandle> {
    let anchor = placement.anchor();
    open_popup_window(
        anchor.x,
        anchor.y,
        anchor.scale_factor,
        cx,
        move |_, cx, popup, target_display| {
            cx.new(|cx| {
                MenuWindowView::new(
                    generation,
                    level,
                    menu,
                    placement,
                    on_action,
                    popup,
                    target_display,
                    cx,
                )
            })
        },
    )
}

use gpui::SharedString;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MenuPosition {
    pub x: f32,
    pub y: f32,
    pub scale_factor: f32,
    pub(crate) root_placement: RootMenuPlacement,
}

impl MenuPosition {
    pub fn new(x: f32, y: f32, scale_factor: f32) -> Self {
        Self {
            x,
            y,
            scale_factor: scale_factor.max(1.0),
            root_placement: RootMenuPlacement::AtPoint,
        }
    }

    pub fn tray(x: f32, y: f32, scale_factor: f32) -> Self {
        Self {
            x,
            y,
            scale_factor: scale_factor.max(1.0),
            root_placement: RootMenuPlacement::Tray,
        }
    }

    pub(crate) fn with_point(self, point: gpui::Point<f32>) -> Self {
        Self {
            x: point.x,
            y: point.y,
            ..self
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RootMenuPlacement {
    AtPoint,
    Tray,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DropdownAnchor {
    pub(crate) x: f32,
    pub(crate) y: f32,
    pub(crate) width: f32,
    pub(crate) height: f32,
    pub(crate) scale_factor: f32,
}

impl DropdownAnchor {
    pub fn new(x: f32, y: f32, width: f32, height: f32, scale_factor: f32) -> Self {
        Self {
            x,
            y,
            width: width.max(1.0),
            height: height.max(1.0),
            scale_factor: scale_factor.max(1.0),
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct MenuActionId(SharedString);

impl MenuActionId {
    pub fn new(value: impl Into<SharedString>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MenuModel {
    items: Vec<MenuItem>,
}

impl MenuModel {
    pub fn new(items: Vec<MenuItem>) -> Self {
        Self { items }
    }

    pub(crate) fn items(&self) -> &[MenuItem] {
        &self.items
    }

    pub(crate) fn item_mut(&mut self, index: usize) -> Option<&mut MenuItem> {
        self.items.get_mut(index)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DropdownItemId(SharedString);

impl DropdownItemId {
    pub fn new(value: impl Into<SharedString>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        self.0.as_ref()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropdownModel {
    items: Vec<DropdownItem>,
    pub(crate) font_family: Option<SharedString>,
}

impl DropdownModel {
    pub fn new(items: Vec<DropdownItem>) -> Self {
        Self {
            items,
            font_family: None,
        }
    }

    pub(crate) fn font_family(mut self, family: impl Into<SharedString>) -> Self {
        self.font_family = Some(family.into());
        self
    }

    pub(crate) fn items(&self) -> &[DropdownItem] {
        &self.items
    }

    pub(crate) fn selected_index(&self) -> Option<usize> {
        self.items
            .iter()
            .position(|item| item.selected && !item.separator)
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropdownItem {
    pub(crate) id: DropdownItemId,
    pub(crate) label: SharedString,
    pub(crate) enabled: bool,
    pub(crate) selected: bool,
    pub(crate) separator: bool,
}

impl DropdownItem {
    pub fn option(id: DropdownItemId, label: impl Into<SharedString>) -> Self {
        Self {
            id,
            label: label.into(),
            enabled: true,
            selected: false,
            separator: false,
        }
    }

    pub fn separator() -> Self {
        Self {
            id: DropdownItemId::new(""),
            label: SharedString::default(),
            enabled: false,
            selected: false,
            separator: true,
        }
    }

    pub fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MenuCheckState {
    Unchecked,
    Checked,
    Mixed,
}

impl MenuCheckState {
    pub(crate) fn toggled(self) -> Self {
        match self {
            Self::Checked => Self::Unchecked,
            Self::Unchecked | Self::Mixed => Self::Checked,
        }
    }

    pub(crate) fn icon_path(self) -> Option<&'static str> {
        match self {
            Self::Unchecked => None,
            Self::Checked => Some("assets/icons/check.svg"),
            Self::Mixed => Some("assets/icons/minus.svg"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MenuItem {
    pub(crate) kind: MenuItemKind,
    pub(crate) id: Option<MenuActionId>,
    pub(crate) label: SharedString,
    pub(crate) icon: Option<MenuIcon>,
    pub(crate) enabled: bool,
    pub(crate) check_state: MenuCheckState,
    pub(crate) close_on_click: bool,
    pub(crate) children: Vec<MenuItem>,
    pub(crate) centered: bool,
    pub(crate) success: bool,
}

impl MenuItem {
    pub fn action(id: MenuActionId, label: impl Into<SharedString>) -> Self {
        Self {
            kind: MenuItemKind::Action,
            id: Some(id),
            label: label.into(),
            icon: None,
            enabled: true,
            check_state: MenuCheckState::Unchecked,
            close_on_click: true,
            children: Vec::new(),
            centered: false,
            success: false,
        }
    }

    pub fn submenu(label: impl Into<SharedString>, children: Vec<MenuItem>) -> Self {
        Self {
            kind: MenuItemKind::Submenu,
            id: None,
            label: label.into(),
            icon: None,
            enabled: true,
            check_state: MenuCheckState::Unchecked,
            close_on_click: true,
            children,
            centered: false,
            success: false,
        }
    }

    /// 可直接点击触发 action、同时悬停展开子菜单的菜单项。
    /// 用于“在 <上次引擎> 中搜索”这类父项即默认操作、子项可切换的场景。
    pub fn action_submenu(
        id: MenuActionId,
        label: impl Into<SharedString>,
        children: Vec<MenuItem>,
    ) -> Self {
        Self {
            kind: MenuItemKind::Action,
            id: Some(id),
            label: label.into(),
            icon: None,
            enabled: true,
            check_state: MenuCheckState::Unchecked,
            close_on_click: true,
            children,
            centered: false,
            success: false,
        }
    }

    pub fn separator() -> Self {
        Self {
            kind: MenuItemKind::Separator,
            id: None,
            label: SharedString::default(),
            icon: None,
            enabled: false,
            check_state: MenuCheckState::Unchecked,
            close_on_click: true,
            children: Vec::new(),
            centered: false,
            success: false,
        }
    }

    pub fn with_icon(mut self, icon: MenuIcon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub fn centered(mut self) -> Self {
        self.centered = true;
        self
    }

    pub fn success(mut self) -> Self {
        self.success = true;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }

    pub fn checked(mut self) -> Self {
        self.check_state = MenuCheckState::Checked;
        self
    }

    pub fn indeterminate(mut self) -> Self {
        self.check_state = MenuCheckState::Mixed;
        self
    }

    pub fn keep_open_on_click(mut self) -> Self {
        self.close_on_click = false;
        self
    }

    pub(crate) fn has_children(&self) -> bool {
        !self.children.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropdown_selected_index_includes_separators_in_scroll_position() {
        let menu = DropdownModel::new(vec![
            DropdownItem::option(DropdownItemId::new("first"), "第一个选项"),
            DropdownItem::separator(),
            DropdownItem::option(DropdownItemId::new("selected"), "当前选项").selected(true),
        ]);

        assert_eq!(menu.selected_index(), Some(2));
    }

    #[test]
    fn dropdown_without_selection_has_no_initial_scroll_target() {
        assert_eq!(DropdownModel::new(vec![]).selected_index(), None);
        let menu = DropdownModel::new(vec![
            DropdownItem::option(DropdownItemId::new("first"), "第一个选项"),
            DropdownItem::separator().selected(true),
        ]);
        assert_eq!(menu.selected_index(), None);
    }

    #[test]
    fn dropdown_scrolls_to_first_selected_option_even_if_disabled() {
        let menu = DropdownModel::new(vec![
            DropdownItem::option(DropdownItemId::new("first"), "当前选项")
                .selected(true)
                .disabled(),
            DropdownItem::separator(),
            DropdownItem::option(DropdownItemId::new("second"), "另一组选项").selected(true),
        ]);

        assert_eq!(menu.selected_index(), Some(0));
    }

    #[test]
    fn action_closes_menu_by_default() {
        let item = MenuItem::action(MenuActionId::new("action"), "Action");

        assert!(item.close_on_click);
    }

    #[test]
    fn keep_open_action_stays_open() {
        let item = MenuItem::action(MenuActionId::new("action"), "Action").keep_open_on_click();

        assert!(!item.close_on_click);
    }

    #[test]
    fn mixed_tag_menu_state_fills_before_it_clears() {
        let item = MenuItem::action(MenuActionId::new("tag"), "混合标签")
            .indeterminate()
            .keep_open_on_click();

        assert_eq!(item.check_state.icon_path(), Some("assets/icons/minus.svg"));
        let checked = item.check_state.toggled();
        assert_eq!(checked, MenuCheckState::Checked);
        assert_eq!(checked.icon_path(), Some("assets/icons/check.svg"));
        assert_eq!(checked.toggled(), MenuCheckState::Unchecked);
        assert_eq!(checked.toggled().icon_path(), None);
        assert!(!item.close_on_click);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum MenuItemKind {
    Action,
    Submenu,
    Separator,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MenuIcon {
    pub(crate) path: SharedString,
    pub(crate) color: Option<u32>,
}

impl MenuIcon {
    pub fn svg(path: impl Into<SharedString>) -> Self {
        Self {
            path: path.into(),
            color: None,
        }
    }

    /// 以本地文件绝对路径作为图标，渲染时走 img() 而非内置 svg。
    pub fn file(path: impl Into<SharedString>) -> Self {
        Self {
            path: path.into(),
            color: None,
        }
    }

    pub fn color(color: u32) -> Self {
        Self {
            path: SharedString::default(),
            color: Some(color),
        }
    }
}

use super::*;

pub(crate) fn confirm_remove_from_favorites(
    main_window: gpui::Entity<MainWindow>,
    owner_window: AnyWindowHandle,
    favorite_id: i64,
    list_item_id: HistoryItemId,
    cx: &mut App,
) {
    if !crate::config_runtime::clipboard_config(cx).confirm_favorite_removal {
        main_window.update(cx, |this, cx| {
            this.remove_from_favorites(favorite_id, list_item_id, cx);
        });
        return;
    }
    let translator = i18n::translator(cx);
    let answer = match owner_window.update(cx, |_, window, cx| {
        let title = translator.translate("main.confirm.remove_favorite.title");
        let detail = translator.translate("main.confirm.remove_favorite.detail");
        let cancel = gpui::PromptButton::cancel(translator.translate("main.form.cancel"));
        let confirm = gpui::PromptButton::ok(translator.translate("main.form.confirm"));
        window.prompt(
            PromptLevel::Warning,
            &title,
            Some(&detail),
            &[cancel, confirm],
            cx,
        )
    }) {
        Ok(answer) => answer,
        Err(error) => {
            tracing::error!("打开取消收藏确认框失败：{error}");
            return;
        }
    };
    cx.spawn(async move |cx| {
        if answer.await.unwrap_or(0) != 1 {
            return;
        }
        main_window.update(cx, |this, cx| {
            this.remove_from_favorites(favorite_id, list_item_id, cx);
        });
    })
    .detach();
}

/// 右键菜单"图片文件"使用的虚拟格式标识，与 PasteService 中的约定一致。
pub(super) const IMAGE_FILE_FORMAT_KEY: &str = "qcclip-image-file";

pub(super) use crate::services::paste::{FILE_NAME_FORMAT_KEY, FILE_PATH_FORMAT_KEY};

/// 条目是否带有可恢复的文件列表，决定是否提供文件名称、文件路径纯文本粘贴。
pub(super) fn has_file_list(item: &ClipboardListItem) -> bool {
    item.all_format_options
        .iter()
        .any(|preview| preview.is_restorable && preview.role() == FormatRole::FileList)
}

/// 右键菜单"使用默认程序打开"的动作标识。
pub(super) const OPEN_WITH_DEFAULT_ACTION: &str = "open-with-default";

/// 右键菜单"在浏览器中打开"单条链接的动作标识前缀，后缀为链接下标。
pub(super) const OPEN_LINK_ACTION_PREFIX: &str = "open-link-";

/// 右键菜单"贴到屏幕"的动作标识。
pub(super) const PIN_TO_SCREEN_ACTION: &str = "pin-to-screen";

/// 右键菜单“拆词”的动作标识。
pub(super) const SPLIT_WORDS_ACTION: &str = "split-words";

/// 右键菜单"全部打开"的动作标识。
pub(super) const OPEN_ALL_LINKS_ACTION: &str = "open-all-links";

#[derive(Clone)]
pub(super) struct FormatAction {
    pub(super) format_namespace: String,
    pub(super) format_key: String,
}

pub(super) const TOGGLE_TAG_ACTION_PREFIX: &str = "toggle-tag-";

pub(super) fn parse_toggle_tag_action(action: &str) -> Option<i64> {
    action
        .strip_prefix(TOGGLE_TAG_ACTION_PREFIX)
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|tag_id| *tag_id > 0)
}

pub(super) fn tag_menu_item(tag: &Tag, is_selected: bool) -> MenuItem {
    let item = MenuItem::action(
        MenuActionId::new(format!("{TOGGLE_TAG_ACTION_PREFIX}{}", tag.id)),
        tag.name.clone(),
    )
    .keep_open_on_click()
    .with_icon(MenuIcon::color(tag_color_value(&tag.color)));
    if is_selected { item.checked() } else { item }
}

pub(super) fn tag_color_value(color: &str) -> u32 {
    let value = color.trim();
    let value = value.strip_prefix('#').unwrap_or(value);
    if value.len() == 6
        && value.chars().all(|character| character.is_ascii_hexdigit())
        && let Ok(value) = u32::from_str_radix(value, 16)
    {
        return value;
    }
    0x6B7280
}

pub(super) fn tags_menu_item(
    tags: &[Tag],
    item_tag_ids: &[i64],
    translator: &Translator,
) -> MenuItem {
    if tags.is_empty() {
        return MenuItem::action(
            MenuActionId::new("tags"),
            translator.translate("main.menu.tags"),
        )
        .with_icon(MenuIcon::svg("tags.svg"))
        .disabled();
    }

    let tag_items = tags
        .iter()
        .map(|tag| tag_menu_item(tag, item_tag_ids.contains(&tag.id)))
        .collect();
    MenuItem::submenu(translator.translate("main.menu.tags"), tag_items)
        .with_icon(MenuIcon::svg("tags.svg"))
}

pub(super) fn history_item_context_menu(
    item: &ClipboardListItem,
    tags: &[Tag],
    translator: &Translator,
    display_priority: ClipboardDisplayPriority,
    search_engines: &[SearchEngine],
    last_search_engine_template: Option<&str>,
    links: &[String],
) -> (MenuModel, Vec<FormatAction>) {
    let (paste, format_actions) = paste_menu_item(item, translator, display_priority);
    let mut items = vec![
        paste,
        MenuItem::action(
            MenuActionId::new("copy"),
            translator.translate("main.menu.copy"),
        )
        .with_icon(MenuIcon::svg("copy.svg")),
        pin_to_screen_menu_item(translator),
    ];
    if has_editable_text(item) {
        items.push(split_words_menu_item(translator));
    }
    items.push(note_menu_item(item, translator));
    if has_editable_text(item) {
        items.push(
            MenuItem::action(
                MenuActionId::new(EDIT_ITEM_ACTION),
                translator.translate("main.menu.edit"),
            )
            .with_icon(MenuIcon::svg("edit.svg")),
        );
    }
    if let Some(search_item) = search_menu_item(
        item,
        translator,
        search_engines,
        last_search_engine_template,
    ) {
        items.push(search_item);
    }
    if let Some(open_item) = open_with_default_menu_item(item, translator) {
        items.push(open_item);
    }
    if let Some(browser_item) = open_in_browser_menu_item(translator, links) {
        items.push(browser_item);
    }
    items.push(tags_menu_item(tags, &item.tag_ids, translator));
    items.push(pin_menu_item(item, translator));
    if item.favorite_id.is_some() {
        items.push(
            MenuItem::action(
                MenuActionId::new("remove-from-favorites"),
                translator.translate("main.menu.remove_from_favorites"),
            )
            .with_icon(MenuIcon::svg("star.svg")),
        );
    } else {
        items.push(
            MenuItem::action(
                MenuActionId::new(ADD_TO_FAVORITES_ACTION),
                translator.translate("main.menu.add_to_favorites"),
            )
            .with_icon(MenuIcon::svg("star.svg")),
        );
    }
    items.push(
        MenuItem::action(
            MenuActionId::new("delete"),
            translator.translate("main.menu.delete"),
        )
        .with_icon(MenuIcon::svg("clipboard-x.svg")),
    );
    (MenuModel::new(items), format_actions)
}

pub(super) fn favorite_item_context_menu(
    item: &ClipboardListItem,
    tags: &[Tag],
    translator: &Translator,
    display_priority: ClipboardDisplayPriority,
    search_engines: &[SearchEngine],
    last_search_engine_template: Option<&str>,
    links: &[String],
) -> (MenuModel, Vec<FormatAction>) {
    let (paste, format_actions) = paste_menu_item(item, translator, display_priority);
    let mut items = vec![
        paste,
        MenuItem::action(
            MenuActionId::new("copy"),
            translator.translate("main.menu.copy"),
        )
        .with_icon(MenuIcon::svg("copy.svg")),
        pin_to_screen_menu_item(translator),
    ];
    if has_editable_text(item) {
        items.push(split_words_menu_item(translator));
    }
    items.push(note_menu_item(item, translator));
    if has_editable_text(item) {
        items.push(
            MenuItem::action(
                MenuActionId::new(EDIT_ITEM_ACTION),
                translator.translate("main.menu.edit"),
            )
            .with_icon(MenuIcon::svg("edit.svg")),
        );
    }
    items.push(tags_menu_item(tags, &item.tag_ids, translator));
    items.push(pin_menu_item(item, translator));
    let mut insert_index = items.len();
    if let Some(search_item) = search_menu_item(
        item,
        translator,
        search_engines,
        last_search_engine_template,
    ) {
        items.insert(insert_index, search_item);
        insert_index += 1;
    }
    if let Some(open_item) = open_with_default_menu_item(item, translator) {
        // 插入到 copy 之后、remove-from-favorites 之前。
        items.insert(insert_index, open_item);
        insert_index += 1;
    }
    if let Some(browser_item) = open_in_browser_menu_item(translator, links) {
        items.insert(insert_index, browser_item);
    }
    items.push(
        MenuItem::action(
            MenuActionId::new("remove-from-favorites"),
            translator.translate("main.menu.remove_from_favorites"),
        )
        .with_icon(MenuIcon::svg("star.svg")),
    );
    (MenuModel::new(items), format_actions)
}

fn pin_menu_item(item: &ClipboardListItem, translator: &Translator) -> MenuItem {
    MenuItem::action(
        MenuActionId::new("toggle-pin"),
        translator.translate(if item.is_pinned {
            "main.menu.unpin"
        } else {
            "main.menu.pin"
        }),
    )
    .with_icon(MenuIcon::svg("pin.svg"))
}

fn pin_to_screen_menu_item(translator: &Translator) -> MenuItem {
    MenuItem::action(
        MenuActionId::new(PIN_TO_SCREEN_ACTION),
        translator.translate("main.menu.pin_to_screen"),
    )
    .with_icon(MenuIcon::svg("sticker.svg"))
}

fn split_words_menu_item(translator: &Translator) -> MenuItem {
    MenuItem::action(
        MenuActionId::new(SPLIT_WORDS_ACTION),
        translator.translate("main.menu.split_words"),
    )
    .with_icon(MenuIcon::svg("blocks.svg"))
}

fn note_menu_item(item: &ClipboardListItem, translator: &Translator) -> MenuItem {
    MenuItem::action(
        MenuActionId::new(EDIT_NOTE_ACTION),
        translator.translate(if item.note_text.is_empty() {
            "main.note.add"
        } else {
            "main.note.edit"
        }),
    )
    .with_icon(MenuIcon::svg("note.svg"))
}

pub(super) fn paste_menu_item(
    item: &ClipboardListItem,
    translator: &Translator,
    display_priority: ClipboardDisplayPriority,
) -> (MenuItem, Vec<FormatAction>) {
    let preferred_roles = display_priority_roles(display_priority);
    let previews =
        select_user_visible_previews_with_priority(&item.preview_options, &preferred_roles);
    let has_additional_formats = item.all_format_options.iter().any(|all_format| {
        !previews.iter().any(|preview| {
            preview.format_namespace == all_format.format_namespace
                && preview.format_key == all_format.format_key
        })
    });
    // 有真实图片且没有文件载荷时，额外提供“图片文件”选项。
    let has_image_file_option = previews
        .iter()
        .any(|preview| preview.is_restorable && preview.role() == FormatRole::Image)
        && !item.all_format_options.iter().any(|preview| {
            matches!(
                preview.role(),
                FormatRole::FileList | FormatRole::VirtualFile
            )
        });
    // 有可恢复的文件列表时，额外提供“文件名称”“文件路径”纯文本选项。
    let has_file_text_options = has_file_list(item);
    let mut format_actions = Vec::new();
    let paste = if previews.len() > 1
        || has_additional_formats
        || has_image_file_option
        || has_file_text_options
    {
        let mut formats = previews
            .iter()
            .map(|preview| {
                format_action_item(
                    preview,
                    format_label(preview, translator),
                    &mut format_actions,
                )
            })
            .collect::<Vec<_>>();
        if has_image_file_option {
            let index = format_actions.len();
            format_actions.push(FormatAction {
                format_namespace: "windows".to_owned(),
                format_key: IMAGE_FILE_FORMAT_KEY.to_owned(),
            });
            formats.push(MenuItem::action(
                MenuActionId::new(format!("paste-format-{index}")),
                translator.translate("main.format.image_file"),
            ));
        }
        if has_file_text_options {
            for (format_key, label_key) in [
                (FILE_NAME_FORMAT_KEY, "main.format.file_name"),
                (FILE_PATH_FORMAT_KEY, "main.format.file_path"),
            ] {
                let index = format_actions.len();
                format_actions.push(FormatAction {
                    format_namespace: "windows".to_owned(),
                    format_key: format_key.to_owned(),
                });
                formats.push(MenuItem::action(
                    MenuActionId::new(format!("paste-format-{index}")),
                    translator.translate(label_key),
                ));
            }
        }
        if has_additional_formats {
            formats.push(MenuItem::separator());
            let all_formats = item
                .all_format_options
                .iter()
                .map(|preview| {
                    format_action_item(
                        preview,
                        raw_format_label(preview, translator),
                        &mut format_actions,
                    )
                })
                .collect();
            formats.push(MenuItem::submenu(
                translator.translate_with_args(
                    "main.menu.all_formats",
                    &[("count", item.all_format_options.len().to_string())],
                ),
                all_formats,
            ));
        }
        MenuItem::submenu(translator.translate("main.menu.paste"), formats)
            .with_icon(MenuIcon::svg("clipboard.svg"))
    } else {
        let mut paste = MenuItem::action(
            MenuActionId::new("paste"),
            translator.translate("main.menu.paste"),
        )
        .with_icon(MenuIcon::svg("clipboard.svg"));
        if !previews
            .first()
            .is_some_and(|preview| preview.is_restorable)
        {
            paste = paste.disabled();
        }
        paste
    };

    (paste, format_actions)
}

/// 有纯文本内容且配置了搜索引擎时返回“在 … 中搜索”子菜单。
pub(super) fn search_menu_item(
    item: &ClipboardListItem,
    translator: &Translator,
    search_engines: &[SearchEngine],
    last_search_engine_template: Option<&str>,
) -> Option<MenuItem> {
    // 只要有可恢复的纯文本格式且预览文本非空即可搜索，不限定聚合类型
    // （Text/Link/含文本的 Mixed 都应有此菜单）。
    let has_text = !item.preview_text.trim().is_empty()
        && item
            .preview_options
            .iter()
            .any(|preview| preview.role() == FormatRole::PlainText && preview.is_restorable);
    if !has_text || search_engines.is_empty() {
        return None;
    }
    // 父菜单项直接展示上次使用的引擎并可点击搜索；没有记录时回落到第一个引擎。
    let last_engine_index = last_search_engine_template
        .and_then(|template| {
            search_engines
                .iter()
                .position(|engine| engine.url_template == template)
        })
        .unwrap_or(0)
        .min(search_engines.len() - 1);
    let last_engine = &search_engines[last_engine_index];
    let parent_label = translator.translate_with_args(
        "main.menu.search_with_engine",
        &[("engine", last_engine.name.clone())],
    );
    // 图标优先级：用户上传图标 > 内置品牌图标 > 通用 search 图标。
    let engine_icon = |engine: &SearchEngine| {
        engine
            .icon_path
            .as_ref()
            .map(|path| MenuIcon::file(path.clone()))
            .or_else(|| {
                engine
                    .builtin_icon_name()
                    .map(|name| MenuIcon::svg(format!("{name}.svg")))
            })
            .unwrap_or_else(|| MenuIcon::svg("search.svg"))
    };
    let parent_icon = engine_icon(last_engine);
    let children = search_engines
        .iter()
        .enumerate()
        .map(|(index, engine)| {
            let icon = engine_icon(engine);
            // 标记上次使用的引擎（子菜单里打勾提示当前默认）。
            let item = MenuItem::action(
                MenuActionId::new(format!("search-engine-{index}")),
                engine.name.clone(),
            )
            .with_icon(icon);
            if index == last_engine_index {
                item.checked()
            } else {
                item
            }
        })
        .collect();
    Some(
        MenuItem::action_submenu(
            MenuActionId::new(format!("search-engine-{last_engine_index}")),
            parent_label,
            children,
        )
        .with_icon(parent_icon),
    )
}
/// 有可打开内容（文件或图片）时返回“使用默认程序打开”菜单项。
pub(super) fn open_with_default_menu_item(
    item: &ClipboardListItem,
    translator: &Translator,
) -> Option<MenuItem> {
    can_open_with_default(item).then(|| {
        MenuItem::action(
            MenuActionId::new(OPEN_WITH_DEFAULT_ACTION),
            translator.translate("main.menu.open_with_default"),
        )
        .with_icon(MenuIcon::svg("external-link.svg"))
    })
}

/// 由调用方解析好的链接构建“在浏览器中打开”菜单项，使用系统默认浏览器。
/// 单个链接直接作为可点击项，多个链接展开为子菜单并附“全部打开”。
pub(super) fn open_in_browser_menu_item(
    translator: &Translator,
    links: &[String],
) -> Option<MenuItem> {
    if links.is_empty() {
        return None;
    }
    let label = translator.translate("main.menu.open_in_browser");
    if links.len() == 1 {
        return Some(
            MenuItem::action(
                MenuActionId::new(format!("{OPEN_LINK_ACTION_PREFIX}0")),
                label,
            )
            .with_icon(MenuIcon::svg("external-link.svg")),
        );
    }
    let mut children = links
        .iter()
        .enumerate()
        .map(|(index, link)| {
            MenuItem::action(
                MenuActionId::new(format!("{OPEN_LINK_ACTION_PREFIX}{index}")),
                truncate_link_label(link),
            )
        })
        .collect::<Vec<_>>();
    children.push(MenuItem::separator());
    children.push(
        MenuItem::action(
            MenuActionId::new(OPEN_ALL_LINKS_ACTION),
            translator.translate("main.menu.open_all_links"),
        )
        .with_icon(MenuIcon::svg("external-link.svg")),
    );
    Some(MenuItem::submenu(label, children).with_icon(MenuIcon::svg("external-link.svg")))
}

/// 菜单中链接标签过长时按字素截断，避免撑开菜单。
pub(super) fn truncate_link_label(link: &str) -> String {
    const MAX_GRAPHEMES: usize = 48;
    let mut graphemes = link.graphemes(true);
    let mut label: String = (&mut graphemes).take(MAX_GRAPHEMES).collect();
    if graphemes.next().is_some() {
        label.push('…');
    }
    label
}

pub(super) fn format_action_item(
    preview: &FormatPreview,
    label: String,
    format_actions: &mut Vec<FormatAction>,
) -> MenuItem {
    let index = format_actions.len();
    format_actions.push(FormatAction {
        format_namespace: preview.format_namespace.clone(),
        format_key: preview.format_key.clone(),
    });
    let item = MenuItem::action(MenuActionId::new(format!("paste-format-{index}")), label);
    if is_format_pastable(preview) {
        item
    } else {
        item.disabled()
    }
}

pub(super) fn is_format_pastable(preview: &FormatPreview) -> bool {
    preview.is_restorable && preview.role() != FormatRole::Internal
}

pub(super) fn raw_format_label(preview: &FormatPreview, translator: &Translator) -> String {
    if crate::domain::capture::is_custom_text_format(&preview.format_namespace, &preview.format_key)
    {
        return translator.translate("main.format.custom_text");
    }
    if preview.format_name.is_empty() {
        preview.format_key.clone()
    } else {
        preview.format_name.clone()
    }
}

pub(super) fn format_label(preview: &FormatPreview, translator: &Translator) -> String {
    if crate::domain::capture::is_custom_text_format(&preview.format_namespace, &preview.format_key)
    {
        return translator.translate("main.format.custom_text");
    }
    match preview.role() {
        FormatRole::PlainText => translator.translate("main.format.plain_text"),
        FormatRole::Rtf => translator.translate("main.format.rtf_rich_text"),
        FormatRole::Image => translator.translate("main.format.image"),
        FormatRole::VirtualFile | FormatRole::FileList => translator.translate("main.format.file"),
        FormatRole::Html => {
            if preview.content_kind == ContentKind::Image {
                translator.translate("main.format.image_html")
            } else {
                translator.translate("main.format.html_rich_text")
            }
        }
        FormatRole::Custom => {
            let name = preview.format_name.trim();
            if name.is_empty() {
                translator.translate("main.format.application")
            } else {
                translator.translate_with_args("main.format.custom", &[("name", name.to_owned())])
            }
        }
        FormatRole::Internal => translator.translate("main.format.internal"),
    }
}

#[cfg(test)]
mod item_display_tests {
    use super::*;
    use crate::i18n::Locale;

    #[test]
    fn both_context_menus_offer_pin_and_unpin_without_toolbar_controls() {
        let translator = Translator::builtin(Locale::zh_cn());
        let mut item = ClipboardListItem {
            history_id: 1,
            item_id: 1,
            favorite_id: Some(2),
            tag_ids: vec![],
            note_text: String::new(),
            preview_text: String::new(),
            preview_char_count: 0,
            preview_options: vec![],
            all_format_options: vec![],
            content_kind: ContentKind::Text,
            paste_count: 0,
            is_pinned: false,
            sort_order: 0,
            last_seen_at: 0,
            shortcut: String::new(),
            source_app_name: None,
            source_device_name: None,
            source_app_icon: None,
            is_selected: false,
        };
        for pinned in [false, true] {
            item.is_pinned = pinned;
            for build_menu in [history_item_context_menu, favorite_item_context_menu] {
                let (menu, _) = build_menu(
                    &item,
                    &[],
                    &translator,
                    ClipboardDisplayPriority::default(),
                    &[],
                    None,
                    &[],
                );
                let action = menu
                    .items()
                    .iter()
                    .find(|entry| {
                        entry
                            .id
                            .as_ref()
                            .is_some_and(|id| id.as_str() == "toggle-pin")
                    })
                    .expect("右键菜单缺少置顶操作");
                assert_eq!(
                    action.label.as_ref(),
                    if pinned { "取消置顶" } else { "置顶" }
                );
                assert!(action.enabled);
            }
        }
    }
}

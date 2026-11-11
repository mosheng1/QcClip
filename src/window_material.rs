use std::{collections::HashMap, path::PathBuf, sync::Arc};

use gpui::{
    AnyView, AnyWindowHandle, App, AppContext as _, BorrowAppContext as _, Context, Global, Hsla,
    IntoElement, ParentElement, Render, RenderImage, Styled, StyledImage, Window,
    WindowBackgroundAppearance, WindowId, div, img, px,
};
use gpui_component::{ActiveTheme as _, Root, Theme, ThemeColor};

use crate::{
    config::{
        AppearanceConfig, BACKGROUND_IMAGE_BLUR_DEFAULT, WINDOW_MATERIAL_BRIGHTNESS_DEFAULT,
        WINDOW_MATERIAL_BRIGHTNESS_MAX, WINDOW_MATERIAL_BRIGHTNESS_MIN,
        WINDOW_MATERIAL_OPACITY_MAX, WINDOW_MATERIAL_OPACITY_MIN, WindowMaterial,
    },
    platform::{
        os_version::{self, WindowsVersion},
        window as platform_window,
    },
};

/// `appearance.background_image_blur` 非有限值时的兜底。
const BACKGROUND_IMAGE_BLUR_FALLBACK: f32 = BACKGROUND_IMAGE_BLUR_DEFAULT;

const BACKGROUND_IMAGE_MAX_DIMENSION: u32 = 1080;

#[derive(Default)]
struct WindowMaterialRuntime {
    base_colors: [ThemeColor; 2],
    windows: HashMap<WindowId, AnyWindowHandle>,
    background_image: Option<Arc<RenderImage>>,
}

impl Global for WindowMaterialRuntime {}

pub(crate) fn install(cx: &mut App) {
    let appearance = crate::config_runtime::appearance_config(cx);
    let background_image = (appearance.window_material == WindowMaterial::CustomImage)
        .then(|| {
            appearance
                .background_image_path
                .map(PathBuf::from)
                .filter(|path| path.is_file())
        })
        .flatten()
        .as_deref()
        .and_then(load_background_image);
    cx.set_global(WindowMaterialRuntime {
        base_colors: [ThemeColor::default(); 2],
        windows: HashMap::new(),
        background_image,
    });
    cx.on_window_closed(|cx, window_id| {
        cx.update_global::<WindowMaterialRuntime, _>(|runtime, _| {
            runtime.windows.remove(&window_id);
        });
    })
    .detach();
}

pub(crate) fn new_root(
    view: impl Into<AnyView>,
    window: &mut Window,
    cx: &mut Context<Root>,
) -> Root {
    // 材质主遮罩由统一背景层绘制，Root 自身保持透明，避免重复覆盖。
    let background_view = cx.new(|_| BackgroundRoot { view: view.into() });
    Root::new(background_view, window, cx).bg(gpui::transparent_black())
}

struct BackgroundRoot {
    view: AnyView,
}

impl Render for BackgroundRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let background = background_element(window, cx);
        div()
            .relative()
            .size_full()
            .child(background)
            .child(self.view.clone())
            .children(
                Root::render_sheet_layer(window, cx)
                    .map(|layer| div().absolute().inset_0().size_full().child(layer)),
            )
            .children(
                Root::render_dialog_layer(window, cx)
                    .map(|layer| div().absolute().inset_0().size_full().child(layer)),
            )
            .children(
                Root::render_notification_layer(window, cx)
                    .map(|layer| div().absolute().inset_0().size_full().child(layer)),
            )
            .child(window_frame())
    }
}

/// 供不走 [`new_root`] 的窗口（如菜单弹窗）复用同一套背景。
pub(crate) fn with_background(
    view: impl IntoElement,
    window: &Window,
    cx: &App,
) -> impl IntoElement {
    let background = background_element(window, cx);
    div()
        .relative()
        .size_full()
        .font_family(cx.theme().font_family.clone())
        .child(background)
        .child(view)
        .child(window_frame())
}

/// 边框只参与绘制，不占布局空间，也不创建会拦截鼠标的命中区域。
pub(crate) fn window_frame() -> impl IntoElement {
    gpui::canvas(
        |_, window, _| crate::platform::window_appearance::frame_style(window),
        |bounds, style, window, cx| {
            let Some(style) = style else {
                return;
            };
            let color = cx.theme().window_border;
            let shortest_side = f32::from(bounds.size.width).min(f32::from(bounds.size.height));
            let radius = style.corner_radius.min(shortest_side / 2.0).max(0.0);
            window.paint_quad(gpui::quad(
                bounds,
                px(radius),
                gpui::transparent_black(),
                px(1.5),
                color,
                gpui::BorderStyle::Solid,
            ));
        },
    )
    .absolute()
    .inset_0()
}

pub(crate) fn background_element(_window: &Window, cx: &App) -> gpui::AnyElement {
    tinted_background_element(None, cx)
}

/// 窗口材质背景层；`tint` 指定主遮罩的色相（如便签颜色），为空时使用主题背景色。
/// 遮罩透明度始终跟随全局材质不透明度。
pub(crate) fn tinted_background_element(tint: Option<Hsla>, cx: &App) -> gpui::AnyElement {
    let background = backdrop_element(cx);
    let appearance = crate::config_runtime::appearance_config(cx);
    if supported_material(appearance.window_material) == WindowMaterial::Disabled {
        return background;
    }

    let base = excluded_window_theme(cx);
    let tint = material_tint_color(
        base.background,
        tint.unwrap_or(cx.theme().background),
        appearance.window_material_opacity,
    );
    div()
        .absolute()
        .inset_0()
        .child(background)
        .child(div().absolute().inset_0().bg(tint))
        .into_any_element()
}

fn backdrop_element(cx: &App) -> gpui::AnyElement {
    let appearance = crate::config_runtime::appearance_config(cx);
    let custom_image = uses_custom_image_background(cx);
    let blur_radius = if appearance.background_image_blur.is_finite() {
        appearance.background_image_blur
    } else {
        BACKGROUND_IMAGE_BLUR_FALLBACK
    };
    let brightness = effective_brightness(&appearance);
    let material = supported_material(appearance.window_material);
    if !custom_image {
        if material == WindowMaterial::Blur {
            let mut container = div().absolute().inset_0();
            if let Some(color) = brightness_overlay(brightness).map(gpui::rgba) {
                container = container.child(div().absolute().inset_0().bg(color));
            }
            return container.into_any_element();
        }
        return div().absolute().inset_0().into_any_element();
    }
    let Some(image) =
        cx.read_global::<WindowMaterialRuntime, _>(|runtime, _| runtime.background_image.clone())
    else {
        return div().absolute().inset_0().into_any_element();
    };
    let mut container = div().absolute().inset_0().overflow_hidden().child(
        img(image)
            .size_full()
            .object_fit(gpui::ObjectFit::Cover)
            .blur(px(blur_radius)),
    );
    if let Some(color) = brightness_overlay(brightness).map(gpui::rgba) {
        container = container.child(div().absolute().inset_0().bg(color));
    }
    container.into_any_element()
}

/// 图片材质通过窗口背景层的黑/白半透明遮罩实现。
fn brightness_overlay(brightness: f32) -> Option<u32> {
    if brightness < 1.0 {
        let alpha = ((1.0 - brightness).clamp(0.0, 1.0) * 255.0).round() as u32;
        Some(alpha)
    } else if brightness > 1.0 {
        let alpha = ((brightness - 1.0).clamp(0.0, 1.0) * 255.0).round() as u32;
        Some(0xFFFFFF00 | alpha)
    } else {
        None
    }
}

pub(crate) fn uses_custom_image_background(cx: &App) -> bool {
    crate::config_runtime::appearance_config(cx).window_material == WindowMaterial::CustomImage
        && cx.read_global::<WindowMaterialRuntime, _>(|runtime, _| {
            runtime.background_image.is_some()
        })
}

fn load_background_image(path: &std::path::Path) -> Option<Arc<RenderImage>> {
    // 以文件内容为准识别格式，避免图片扩展名与真实格式不一致时加载失败。
    let reader =
        match image::ImageReader::open(path).and_then(|reader| reader.with_guessed_format()) {
            Ok(reader) => reader,
            Err(error) => {
                tracing::warn!(%error, "加载背景图片失败，已使用纯色背景");
                return None;
            }
        };
    let image = match reader.decode() {
        Ok(image) => image.to_rgba8(),
        Err(error) => {
            tracing::warn!(%error, "加载背景图片失败，已使用纯色背景");
            return None;
        }
    };
    // 模糊交给 GPU 的 `blur()`，这里只降采样到最长边限制。
    let (w, h) = image.dimensions();
    let longest = w.max(h);
    let mut image = if longest > BACKGROUND_IMAGE_MAX_DIMENSION {
        let scale = BACKGROUND_IMAGE_MAX_DIMENSION as f32 / longest as f32;
        image::imageops::resize(
            &image,
            (w as f32 * scale).round().max(1.0) as u32,
            (h as f32 * scale).round().max(1.0) as u32,
            image::imageops::FilterType::Triangle,
        )
    } else {
        image
    };
    for pixel in image.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new(vec![image::Frame::new(image)])))
}

pub(crate) fn refresh_background_image(cx: &mut App) {
    let configured_path = crate::config_runtime::appearance_config(cx)
        .background_image_path
        .map(PathBuf::from);
    if configured_path.as_ref().is_some_and(|path| !path.is_file()) {
        tracing::warn!("背景图片文件不存在，已使用纯色背景");
    }
    let image = configured_path
        .filter(|path| path.is_file())
        .as_deref()
        .and_then(load_background_image);
    cx.update_global::<WindowMaterialRuntime, _>(|runtime, _| {
        runtime.background_image = image;
    });
    cx.refresh_windows();
}

pub(crate) fn register_window(window: &Window, cx: &mut App) {
    let window_id = window.window_handle().window_id();
    let handle = window.window_handle().into();
    cx.update_global::<WindowMaterialRuntime, _>(|runtime, _| {
        runtime.windows.insert(window_id, handle);
    });
    apply_to_window(window, cx);
}

pub(crate) fn apply_from_config(cx: &mut App) {
    if !cx.has_global::<WindowMaterialRuntime>() {
        return;
    }

    let windows = cx.read_global::<WindowMaterialRuntime, _>(|runtime, _| {
        runtime.windows.values().copied().collect::<Vec<_>>()
    });
    let retry = apply_material_to_windows(cx, windows);
    if !retry.is_empty() {
        // 触发变更的窗口此刻仍在处理自身事件，窗口对象被借出后无法重入更新，
        // 延后到本轮 effect 结束再补一次，避免它的材质要等下次打开才生效。
        cx.defer(move |cx| {
            apply_material_to_windows(cx, retry);
            cx.refresh_windows();
        });
    }
    cx.refresh_windows();
}

/// 向给定窗口下发材质，返回因为窗口正在被更新而没能处理的窗口。
fn apply_material_to_windows(cx: &mut App, windows: Vec<AnyWindowHandle>) -> Vec<AnyWindowHandle> {
    let mut pending = Vec::new();
    for handle in windows {
        if handle
            .update(cx, |_, window, cx| apply_to_window(window, cx))
            .is_err()
        {
            pending.push(handle);
        }
    }
    pending
}

/// 记录当前模式未经材质处理的主题色，供材质推导与黑名单窗口回溯使用。
pub(crate) fn capture_base_colors(cx: &mut App) {
    if !cx.has_global::<WindowMaterialRuntime>() {
        return;
    }
    let mode = cx.read_global::<Theme, _>(|theme, _| theme.mode);
    let base_colors = cx.read_global::<Theme, _>(|theme, _| theme.colors);
    cx.update_global::<WindowMaterialRuntime, _>(|runtime, _| {
        runtime.base_colors[usize::from(mode.is_dark())] = base_colors;
    });
    apply_theme_colors(cx);
}

pub(crate) fn excluded_window_theme(cx: &impl gpui::AppContext) -> ThemeColor {
    let mode = cx.read_global::<Theme, _>(|theme, _| theme.mode);
    cx.read_global::<WindowMaterialRuntime, _>(|runtime, _| {
        runtime.base_colors[usize::from(mode.is_dark())]
    })
}

pub(crate) fn menu_window_colors(cx: &App) -> ThemeColor {
    let material = supported_material(crate::config_runtime::appearance_config(cx).window_material);
    menu_colors(cx.theme().colors, material)
}

fn menu_colors(mut colors: ThemeColor, material: WindowMaterial) -> ThemeColor {
    if material != WindowMaterial::Disabled {
        // 独立菜单已有窗口主遮罩，只沿用普通表面的 Alpha，保留菜单自己的色相。
        colors.popover = with_alpha(colors.popover, colors.background.alpha);
    }
    colors
}

/// 当前系统可选的窗口材质，用于设置页下拉选项。
pub(crate) fn available_materials() -> Vec<WindowMaterial> {
    let version = os_version::current();
    WindowMaterial::ALL
        .into_iter()
        .filter(|material| is_supported(*material, version))
        .collect()
}

/// 把当前系统不支持的材质回退成可用材质，避免窗口失去背景。
pub(crate) fn supported_material(material: WindowMaterial) -> WindowMaterial {
    supported_material_for_version(material, os_version::current())
}

fn supported_material_for_version(
    material: WindowMaterial,
    version: WindowsVersion,
) -> WindowMaterial {
    if is_supported(material, version) {
        return material;
    }
    if version.supports_blur() {
        WindowMaterial::Blur
    } else {
        WindowMaterial::Acrylic
    }
}

fn is_supported(material: WindowMaterial, version: WindowsVersion) -> bool {
    match material {
        WindowMaterial::Disabled | WindowMaterial::Transparent => true,
        WindowMaterial::Acrylic => version.supports_acrylic(),
        WindowMaterial::Blur => version.supports_blur(),
        WindowMaterial::CustomImage => true,
    }
}

fn apply_to_window(window: &Window, cx: &mut App) {
    let appearance = crate::config_runtime::appearance_config(cx);
    let material = supported_material(appearance.window_material);
    let mode = cx.read_global::<Theme, _>(|theme, _| theme.mode);
    let brightness = if matches!(
        material,
        WindowMaterial::Acrylic | WindowMaterial::Blur | WindowMaterial::CustomImage
    ) {
        effective_brightness(&appearance)
    } else {
        1.0
    };
    let system_material = matches!(material, WindowMaterial::Acrylic | WindowMaterial::Blur);
    // 先移除旧系统的圆角遮罩，再启用系统材质，避免圆角外出现材质残留。
    if system_material
        && let Err(error) = crate::platform::window_appearance::apply(window, material)
    {
        tracing::warn!("应用窗口外观失败：{error}");
    }
    window.set_background_appearance(background_appearance(material));
    platform_window::apply_window_material(window, material, brightness, !mode.is_dark());
    if !system_material
        && let Err(error) = crate::platform::window_appearance::apply(window, material)
    {
        tracing::warn!("应用窗口外观失败：{error}");
    }
}

fn background_appearance(material: WindowMaterial) -> WindowBackgroundAppearance {
    match material {
        WindowMaterial::Disabled | WindowMaterial::Transparent => {
            WindowBackgroundAppearance::Transparent
        }
        WindowMaterial::Acrylic => WindowBackgroundAppearance::Blurred,
        WindowMaterial::Blur => {
            #[cfg(target_os = "windows")]
            {
                WindowBackgroundAppearance::Transparent
            }
            #[cfg(not(target_os = "windows"))]
            {
                WindowBackgroundAppearance::Blurred
            }
        }
        WindowMaterial::CustomImage => WindowBackgroundAppearance::Transparent,
    }
}

fn apply_theme_colors(cx: &mut App) {
    let appearance = crate::config_runtime::appearance_config(cx);
    let material = supported_material(appearance.window_material);
    let mode = cx.read_global::<Theme, _>(|theme, _| theme.mode);
    let base_colors = cx.read_global::<WindowMaterialRuntime, _>(|runtime, _| {
        runtime.base_colors[usize::from(mode.is_dark())]
    });
    let colors = if material == WindowMaterial::Disabled {
        base_colors
    } else {
        material_colors(base_colors)
    };

    cx.update_global::<Theme, _>(|theme, _| {
        theme.colors = colors;
        theme.tokens = colors.into();
    });
    Theme::sync_base(cx);
}

fn material_window_color(base: Hsla, window_opacity: f32) -> Hsla {
    let opacity = window_opacity.clamp(WINDOW_MATERIAL_OPACITY_MIN, WINDOW_MATERIAL_OPACITY_MAX);
    with_alpha(base, base.alpha * opacity)
}

/// 浮于列表之上的毛玻璃表面：开启材质时与窗口色调一致并跟随材质不透明度，
/// 关闭材质时使用调用方给出的颜色。
pub(crate) fn overlay_surface_color(cx: &App, fallback: Hsla) -> Hsla {
    let appearance = crate::config_runtime::appearance_config(cx);
    if supported_material(appearance.window_material) == WindowMaterial::Disabled {
        return fallback;
    }
    material_tint_color(
        excluded_window_theme(cx).background,
        cx.theme().background,
        appearance.window_material_opacity,
    )
}

/// 背景模糊跟随窗口材质，关闭材质时不创建模糊滤镜。
pub(crate) fn with_backdrop_blur<E: Styled>(element: E, cx: &App) -> E {
    let material = supported_material(crate::config_runtime::appearance_config(cx).window_material);
    if material == WindowMaterial::Disabled {
        element
    } else {
        element.backdrop_blur(px(18.0))
    }
}

/// 设置页抽屉表面：开启材质时与浮层一致（窗口色调 + 背景模糊），关闭材质时完全不透明。
pub(crate) fn sheet_surface<E: Styled>(element: E, cx: &App) -> E {
    let fallback = excluded_window_theme(cx).background;
    with_backdrop_blur(element.bg(overlay_surface_color(cx, fallback)), cx)
}

pub(crate) fn material_tint_color(
    base_background: Hsla,
    theme_background: Hsla,
    window_opacity: f32,
) -> Hsla {
    let tint = material_window_color(base_background, window_opacity);
    with_alpha(theme_background, tint.alpha)
}

/// 普通表面轻覆盖，交互状态保留辨识度，浮层独立保证可读性。
fn material_colors(base: ThemeColor) -> ThemeColor {
    let mut colors = base;

    let surface = |color: Hsla, opacity| with_alpha(color, color.alpha * opacity);
    let hover = |color: Hsla| with_alpha(color, color.alpha.min(0.20));
    let active = |color: Hsla| with_alpha(color, color.alpha.min(0.28));

    colors.background = surface(base.background, 0.06);
    colors.title_bar = surface(base.title_bar, 0.08);
    colors.sidebar = surface(base.sidebar, 0.08);
    colors.tab_bar = surface(base.tab_bar, 0.08);
    colors.secondary = surface(base.secondary, 0.08);
    colors.muted = surface(base.muted, 0.08);
    colors.list = surface(base.list, 0.04);
    colors.list_even = surface(base.list_even, 0.06);
    colors.list_head = surface(base.list_head, 0.08);
    colors.table = surface(base.table, 0.04);
    colors.table_even = surface(base.table_even, 0.06);
    colors.table_head = surface(base.table_head, 0.08);
    colors.table_foot = surface(base.table_foot, 0.08);
    colors.tab = surface(base.tab, 0.06);
    colors.tab_bar_segmented = surface(base.tab_bar_segmented, 0.08);
    colors.accordion = surface(base.accordion, 0.06);
    colors.group_box = surface(base.group_box, 0.06);
    colors.description_list_label = surface(base.description_list_label, 0.06);
    colors.status_bar = surface(base.status_bar, 0.08);
    colors.tiles = surface(base.tiles, 0.06);
    colors.button = surface(base.button, 0.12);
    colors.button_secondary = surface(base.button_secondary, 0.12);
    colors.skeleton = surface(base.skeleton, 0.12);
    colors.progress_bar = surface(base.progress_bar, 0.24);
    colors.slider_bar = surface(base.slider_bar, 0.24);

    // 已有低 Alpha 的交互色不再缩淡，防止悬停与选中反馈消失。
    colors.accent = hover(base.accent);
    colors.sidebar_accent = hover(base.sidebar_accent);
    colors.secondary_hover = hover(base.secondary_hover);
    colors.secondary_active = active(base.secondary_active);
    colors.button_hover = hover(base.button_hover);
    colors.button_active = active(base.button_active);
    colors.button_secondary_hover = hover(base.button_secondary_hover);
    colors.button_secondary_active = active(base.button_secondary_active);
    colors.list_hover = hover(base.list_hover);
    colors.list_active = active(base.list_active);
    colors.table_hover = hover(base.table_hover);
    colors.table_active = active(base.table_active);
    colors.tab_active = active(base.tab_active);

    // 弹出层会覆盖窗口内的文字，不能与普通表面一起变淡。
    colors.popover = with_alpha(base.popover, 0.92);

    colors
}

pub(crate) fn effective_brightness(appearance: &AppearanceConfig) -> f32 {
    appearance
        .window_material_brightness
        .filter(|brightness| brightness.is_finite())
        .unwrap_or(WINDOW_MATERIAL_BRIGHTNESS_DEFAULT)
        .clamp(
            WINDOW_MATERIAL_BRIGHTNESS_MIN,
            WINDOW_MATERIAL_BRIGHTNESS_MAX,
        )
}

pub(crate) fn current_brightness(cx: &App) -> f32 {
    let appearance = crate::config_runtime::appearance_config(cx);
    effective_brightness(&appearance)
}

fn with_alpha(mut color: Hsla, alpha: f32) -> Hsla {
    color.alpha = alpha.clamp(0.0, 1.0);
    color
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_with_blur_exclude_acrylic_and_fall_back_to_blur() {
        for version in [WindowsVersion::Windows10, WindowsVersion::Earlier] {
            assert!(!is_supported(WindowMaterial::Acrylic, version));
            assert_eq!(
                supported_material_for_version(WindowMaterial::Acrylic, version),
                WindowMaterial::Blur
            );
        }
        assert!(is_supported(
            WindowMaterial::Acrylic,
            WindowsVersion::Windows11
        ));
        assert_eq!(
            supported_material_for_version(WindowMaterial::Blur, WindowsVersion::Windows11),
            WindowMaterial::Acrylic
        );
    }

    #[test]
    fn disabled_material_keeps_a_transparent_gpui_window() {
        assert_eq!(
            background_appearance(WindowMaterial::Disabled),
            WindowBackgroundAppearance::Transparent
        );
    }

    #[test]
    fn acrylic_uses_gpui_blurred_appearance() {
        assert_eq!(
            background_appearance(WindowMaterial::Acrylic),
            WindowBackgroundAppearance::Blurred
        );
    }

    #[test]
    fn material_surfaces_preserve_theme_hues_and_existing_transparency() {
        let base = ThemeColor {
            background: gpui::hsla(0.6, 0.4, 0.3, 0.8),
            secondary: gpui::hsla(0.2, 0.5, 0.6, 0.5),
            tab: gpui::transparent_black(),
            ..ThemeColor::default()
        };
        let colors = material_colors(base);

        assert_eq!(colors.background, with_alpha(base.background, 0.8 * 0.06));
        assert_eq!(colors.secondary, with_alpha(base.secondary, 0.5 * 0.08));
        assert_eq!(colors.tab.alpha, 0.0);
    }

    #[test]
    fn nested_material_surfaces_keep_the_backdrop_visible() {
        let base = ThemeColor::default();
        let colors = material_colors(base);
        let window = material_window_color(base.background, 0.5);
        let combined_alpha = 1.0
            - (1.0 - window.alpha)
                * (1.0 - colors.background.alpha)
                * (1.0 - colors.secondary.alpha)
                * (1.0 - colors.background.alpha);

        assert!((0.5..0.6).contains(&combined_alpha));
        assert!(colors.popover.alpha >= 0.9);
    }

    #[test]
    fn standalone_menus_do_not_stack_a_dense_popover_over_the_window_tint() {
        let base = ThemeColor {
            popover: gpui::hsla(0.6, 0.4, 0.3, 1.0),
            ..ThemeColor::default()
        };
        let colors = material_colors(base);
        for material in [
            WindowMaterial::Transparent,
            WindowMaterial::Acrylic,
            WindowMaterial::Blur,
            WindowMaterial::CustomImage,
        ] {
            let menu = menu_colors(colors, material);
            let window = material_window_color(base.background, 0.5);
            let combined_alpha = 1.0 - (1.0 - window.alpha) * (1.0 - menu.popover.alpha);

            assert!((0.5..0.6).contains(&combined_alpha));
            assert_eq!(
                menu.popover,
                with_alpha(base.popover, menu.background.alpha)
            );
            assert_eq!(menu.popover_foreground, colors.popover_foreground);
        }
        assert!(colors.popover.alpha >= 0.9);
    }

    #[test]
    fn standalone_menus_preserve_the_original_surface_when_material_is_disabled() {
        let base = ThemeColor {
            background: gpui::hsla(0.1, 0.2, 0.3, 0.8),
            popover: gpui::hsla(0.6, 0.4, 0.3, 0.7),
            ..ThemeColor::default()
        };
        let menu = menu_colors(base, WindowMaterial::Disabled);

        assert_eq!(menu.popover, base.popover);
        assert_eq!(menu.background, base.background);
    }

    #[test]
    fn material_preserves_text_borders_and_semantic_controls() {
        let base = ThemeColor::default();
        let colors = material_colors(base);

        assert_eq!(colors.foreground, base.foreground);
        assert_eq!(colors.muted_foreground, base.muted_foreground);
        assert_eq!(colors.input, base.input);
        assert_eq!(colors.border, base.border);
        assert_eq!(colors.window_border, base.window_border);
        assert_eq!(colors.primary, base.primary);
        assert_eq!(colors.button_primary, base.button_primary);
        assert_eq!(colors.switch, base.switch);
        assert_eq!(colors.scrollbar_thumb, base.scrollbar_thumb);
    }

    #[test]
    fn material_interaction_states_keep_authored_low_alpha() {
        let base = ThemeColor {
            list_hover: gpui::hsla(0.6, 0.5, 0.5, 0.16),
            list_active: gpui::hsla(0.6, 0.5, 0.5, 0.12),
            ..ThemeColor::default()
        };
        let colors = material_colors(base);

        assert_eq!(colors.list_hover, base.list_hover);
        assert_eq!(colors.list_active, base.list_active);
        assert!(colors.button_active.alpha > colors.button_hover.alpha);
        assert!(colors.button_hover.alpha > colors.button.alpha);
    }

    #[test]
    fn window_opacity_only_controls_the_main_tint() {
        let base = gpui::hsla(0.6, 0.4, 0.3, 1.0);
        assert_eq!(material_window_color(base, 0.5), with_alpha(base, 0.5));
        assert_eq!(material_window_color(base, 1.0), base);
        assert_eq!(material_window_color(with_alpha(base, 0.8), 0.5).alpha, 0.4);
    }

    #[test]
    fn material_tint_keeps_theme_hue_and_global_opacity() {
        let base_background = gpui::hsla(0.2, 0.3, 0.4, 0.8);
        let theme_background = gpui::hsla(0.8, 0.9, 0.6, 0.06);

        assert_eq!(
            material_tint_color(base_background, theme_background, 0.5),
            with_alpha(theme_background, 0.4)
        );
    }

    #[test]
    fn material_opacity_is_clamped_to_the_configured_range() {
        let base = gpui::hsla(0.6, 0.4, 0.3, 1.0);
        let lowest = material_window_color(base, 0.0);
        assert_eq!(lowest.alpha, WINDOW_MATERIAL_OPACITY_MIN);

        let highest = material_window_color(base, 10.0);
        assert_eq!(highest.alpha, WINDOW_MATERIAL_OPACITY_MAX);
    }

    #[test]
    fn background_image_uses_file_content_when_extension_is_wrong() {
        let directory = tempfile::tempdir().expect("应创建临时目录");
        let path = directory.path().join("背景 (3).png");
        image::DynamicImage::new_rgb8(1, 1)
            .save_with_format(&path, image::ImageFormat::Jpeg)
            .expect("应写入测试图片");

        assert!(load_background_image(&path).is_some());
    }
}

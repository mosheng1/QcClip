use super::*;
use gpui::{IntoElement, Render, VisualTestContext, div, px};

struct EmptyPanel;

impl Render for EmptyPanel {
    fn render(&mut self, _: &mut gpui::Window, _: &mut gpui::Context<Self>) -> impl IntoElement {
        div()
    }
}

#[gpui::test]
fn main_panel_close_is_intercepted_in_every_position(cx: &mut gpui::TestAppContext) {
    let directory = tempfile::tempdir().unwrap();
    let config_path = directory.path().join("config.json");
    cx.update(|cx| {
        crate::config_runtime::ConfigRuntime::install(config_path.clone(), cx);
        cx.set_global(MainWindowService::default());
    });
    let window = cx.open_window(gpui::size(px(360.0), px(520.0)), |window, cx| {
        window.on_window_should_close(cx, main_window_should_close);
        EmptyPanel
    });
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    for position in crate::config::ClipboardWindowPosition::ALL {
        visual.update(|_, cx| {
            let mut clipboard = config_runtime::clipboard_config(cx);
            clipboard.window_position = position;
            config_runtime::set_clipboard_config(clipboard, cx).unwrap();
        });
        assert!(
            !visual.simulate_close(),
            "关闭面板不应销毁窗口：{position:?}"
        );
        visual.run_until_parked();
        window.update(&mut visual, |_, _, _| ()).unwrap();
    }
}

use std::{ffi::c_void, mem::size_of, sync::atomic::Ordering};

use windows::Win32::{
    Foundation::LPARAM,
    UI::{
        Input::{
            GetRawInputData, HRAWINPUT, MOUSE_MOVE_ABSOLUTE, MOUSE_VIRTUAL_DESKTOP, RAWINPUT,
            RAWINPUTHEADER, RID_INPUT, RIM_TYPEKEYBOARD, RIM_TYPEMOUSE,
        },
        WindowsAndMessaging::{
            RI_MOUSE_BUTTON_1_DOWN, RI_MOUSE_BUTTON_1_UP, RI_MOUSE_BUTTON_2_DOWN,
            RI_MOUSE_BUTTON_2_UP, RI_MOUSE_BUTTON_3_DOWN, RI_MOUSE_BUTTON_3_UP,
            RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP, RI_MOUSE_BUTTON_5_DOWN,
            RI_MOUSE_BUTTON_5_UP, RI_MOUSE_HWHEEL, RI_MOUSE_WHEEL, RI_KEY_BREAK, RI_KEY_E0,
            RI_KEY_E1,
        },
    },
};

use crate::{
    dispatcher::{CALLBACKS, NEXT_ID, Status, Subscriber, dispatch, remove_all},
    event::{Event, MouseButton, Point},
    platform::{
        DisplayImpl, ListenImpl, PlatformDisplay, PlatformListen,
        windows::common::{
            IS_LISTEN_RUNNING, LISTEN_FLAG, LISTEN_KEYBOARD, LISTEN_MOUSE_BUTTON,
            LISTEN_MOUSE_MOVE, LISTEN_MOUSE_WHEEL, LISTENS_ALL, update_state,
        },
        windows::{SIMULATED_KEYBOARD_EXTRA_INFO, get_win_key},
    },
    subscription::SubscriptionHandle,
};

impl ListenImpl for PlatformListen {
    fn start() {
        if Self::is_run() {
            return;
        }

        LISTEN_FLAG.store(LISTENS_ALL, Ordering::SeqCst);
    }

    fn is_runing() -> bool {
        IS_LISTEN_RUNNING.load(Ordering::SeqCst)
    }

    fn pause() {
        IS_LISTEN_RUNNING.store(false, Ordering::SeqCst);
    }

    fn resume() {
        IS_LISTEN_RUNNING.store(true, Ordering::SeqCst);
    }

    fn stop() {
        LISTEN_FLAG.store(0, Ordering::SeqCst);
        Self::pause();
        Self::unsubscribe_all();
    }

    fn mouse_move(enable: bool) {
        update_state(&LISTEN_FLAG, LISTEN_MOUSE_MOVE, enable);
    }

    fn mouse_wheel(enable: bool) {
        update_state(&LISTEN_FLAG, LISTEN_MOUSE_WHEEL, enable);
    }

    fn mouse_button(enable: bool) {
        update_state(&LISTEN_FLAG, LISTEN_MOUSE_BUTTON, enable);
    }

    fn keyboard(enable: bool) {
        update_state(&LISTEN_FLAG, LISTEN_KEYBOARD, enable);
    }

    fn subscribe<F>(callback: F) -> SubscriptionHandle
    where
        F: Fn(Event) + Send + Sync + 'static,
    {
        let id = NEXT_ID.fetch_add(1, Ordering::SeqCst);
        CALLBACKS.insert(
            id,
            Subscriber {
                status: Status::Active,
                callback: Box::new(callback),
            },
        );
        SubscriptionHandle { id }
    }

    fn unsubscribe_all() {
        remove_all();
    }
}

impl PlatformListen {
    fn is_run() -> bool {
        IS_LISTEN_RUNNING
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
    }

    pub(crate) fn handle_raw_input(lparam: LPARAM) -> bool {
        if !IS_LISTEN_RUNNING.load(Ordering::Relaxed) {
            return false;
        }

        let state = LISTEN_FLAG.load(Ordering::Relaxed);
        if state == 0 {
            return false;
        }

        let Some(raw) = raw_input_from_message(lparam) else {
            return false;
        };
        match raw.header.dwType {
            value if value == RIM_TYPEMOUSE.0 => handle_mouse(raw, state),
            value if value == RIM_TYPEKEYBOARD.0 => handle_keyboard(raw, state),
            _ => return false,
        }
        true
    }
}

fn raw_input_from_message(lparam: LPARAM) -> Option<RAWINPUT> {
    let h_raw_input = HRAWINPUT(lparam.0 as *mut c_void);
    let mut raw = RAWINPUT::default();
    let mut size = size_of::<RAWINPUT>() as u32;
    let raw_size = unsafe {
        GetRawInputData(
            h_raw_input,
            RID_INPUT,
            Some(&mut raw as *mut _ as *mut _),
            &mut size,
            size_of::<RAWINPUTHEADER>() as u32,
        )
    };
    (raw_size != u32::MAX && raw_size != 0).then_some(raw)
}

fn handle_mouse(raw: RAWINPUT, state: u32) {
    let mouse = unsafe { raw.data.mouse };

    if state & LISTEN_MOUSE_MOVE != 0 {
        if mouse.usFlags.0 & MOUSE_MOVE_ABSOLUTE.0 != 0 {
            let bounds = if mouse.usFlags.0 & MOUSE_VIRTUAL_DESKTOP.0 != 0 {
                PlatformDisplay::get_virtual_screen_bounds()
            } else {
                let (width, height) = PlatformDisplay::get_primary_screen_size();
                (0.0, 0.0, width, height)
            };
            // 绝对坐标的 (0, 0) 也是有效位置，首个事件同样需要分发。
            dispatch(Event::MouseMoveAbsolute {
                position: absolute_mouse_position(mouse.lLastX, mouse.lLastY, bounds),
            });
        } else {
            let dx = mouse.lLastX as f64;
            let dy = mouse.lLastY as f64;
            if dx != 0.0 || dy != 0.0 {
                dispatch(Event::MouseMove {
                    delta: Point { x: dx, y: dy },
                });
            }
        }
    }

    let button_data = unsafe { mouse.Anonymous.Anonymous };
    let button_flags = button_data.usButtonFlags as u32;
    if state & LISTEN_MOUSE_BUTTON != 0 {
        dispatch_button(button_flags, RI_MOUSE_BUTTON_1_DOWN, RI_MOUSE_BUTTON_1_UP, MouseButton::Left);
        dispatch_button(button_flags, RI_MOUSE_BUTTON_2_DOWN, RI_MOUSE_BUTTON_2_UP, MouseButton::Right);
        dispatch_button(button_flags, RI_MOUSE_BUTTON_3_DOWN, RI_MOUSE_BUTTON_3_UP, MouseButton::Middle);
        dispatch_button(button_flags, RI_MOUSE_BUTTON_4_DOWN, RI_MOUSE_BUTTON_4_UP, MouseButton::Back);
        dispatch_button(button_flags, RI_MOUSE_BUTTON_5_DOWN, RI_MOUSE_BUTTON_5_UP, MouseButton::Forward);
    }

    if state & LISTEN_MOUSE_WHEEL != 0 {
        if button_flags & RI_MOUSE_WHEEL != 0 {
            dispatch(Event::MouseWheel {
                delta: Point {
                    x: 0.0,
                    y: button_data.usButtonData as i16 as f64 / 120.0,
                },
            });
        }
        if button_flags & RI_MOUSE_HWHEEL != 0 {
            dispatch(Event::MouseWheel {
                delta: Point {
                    x: button_data.usButtonData as i16 as f64 / 120.0,
                    y: 0.0,
                },
            });
        }
    }
}

fn absolute_mouse_position(x: i32, y: i32, bounds: (f64, f64, f64, f64)) -> Point {
    let (left, top, width, height) = bounds;
    // Raw Input 将 [0, 65535] 映射到屏幕像素，虚拟桌面的原点可能为负数。
    Point {
        x: left + (f64::from(x) * width / 65536.0).floor(),
        y: top + (f64::from(y) * height / 65536.0).floor(),
    }
}

fn handle_keyboard(raw: RAWINPUT, state: u32) {
    if state & LISTEN_KEYBOARD == 0 {
        return;
    }

    // SAFETY: RAWINPUT 的类型已确认是键盘，联合体中的 keyboard 字段有效。
    let keyboard = unsafe { raw.data.keyboard };
    if keyboard.ExtraInformation == SIMULATED_KEYBOARD_EXTRA_INFO {
        return;
    }
    let flags = keyboard.Flags as u32;
    let scan_code = match flags & (RI_KEY_E0 | RI_KEY_E1) {
        RI_KEY_E0 => 0xE000 | u32::from(keyboard.MakeCode),
        RI_KEY_E1 => 0xE100 | u32::from(keyboard.MakeCode),
        _ => u32::from(keyboard.MakeCode),
    };
    let key = get_win_key(u32::from(keyboard.VKey), scan_code);
    let event = if flags & RI_KEY_BREAK != 0 {
        Event::KeyUp {
            key,
            code: Some(u32::from(keyboard.VKey)),
        }
    } else {
        Event::KeyDown {
            key,
            code: Some(u32::from(keyboard.VKey)),
        }
    };
    dispatch(event);
}

fn dispatch_button(flags: u32, down_flag: u32, up_flag: u32, button: MouseButton) {
    if flags & down_flag != 0 {
        dispatch(Event::MouseDown { button });
    }
    if flags & up_flag != 0 {
        dispatch(Event::MouseUp { button });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use serial_test::serial;
    use windows::Win32::UI::Input::{MOUSE_STATE, RAWMOUSE};

    use super::*;

    #[test]
    #[serial]
    fn absolute_mouse_input_dispatches_screen_positions() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let received = events.clone();
        let handle = PlatformListen::subscribe(move |event| received.lock().unwrap().push(event));
        let (width, height) = PlatformDisplay::get_primary_screen_size();
        let (left, top, virtual_width, virtual_height) =
            PlatformDisplay::get_virtual_screen_bounds();
        for (flags, coordinate, expected) in [
            (MOUSE_MOVE_ABSOLUTE, 0, Point { x: 0.0, y: 0.0 }),
            (
                MOUSE_MOVE_ABSOLUTE,
                65535,
                Point {
                    x: width - 1.0,
                    y: height - 1.0,
                },
            ),
            (
                MOUSE_STATE(MOUSE_MOVE_ABSOLUTE.0 | MOUSE_VIRTUAL_DESKTOP.0),
                65535,
                Point {
                    x: left + virtual_width - 1.0,
                    y: top + virtual_height - 1.0,
                },
            ),
        ] {
            let mut raw = RAWINPUT::default();
            raw.data.mouse = RAWMOUSE {
                usFlags: flags,
                lLastX: coordinate,
                lLastY: coordinate,
                ..Default::default()
            };
            handle_mouse(raw, LISTEN_MOUSE_MOVE);
            assert_eq!(
                events.lock().unwrap().pop(),
                Some(Event::MouseMoveAbsolute { position: expected })
            );
            handle_mouse(raw, LISTEN_MOUSE_BUTTON);
            assert!(events.lock().unwrap().is_empty());
        }
        handle.unsubscribe();
        assert_eq!(
            absolute_mouse_position(0, 32768, (-1920.0, -1080.0, 3840.0, 2160.0)),
            Point { x: -1920.0, y: 0.0 }
        );
    }
}

use std::mem::size_of;

use windows::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
    KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, MOUSEEVENTF_ABSOLUTE,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN,
    MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP,
    MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT,
    SendInput, VIRTUAL_KEY,
};

use crate::{
    Event, Key, MouseButton,
    platform::{
        DisplayImpl, PlatformDisplay, PlatformSimulate, SimulateImpl,
        windows::{SIMULATED_KEYBOARD_EXTRA_INFO, keycode::get_win_codes},
    },
};

impl SimulateImpl for PlatformSimulate {
    fn simulate(event: Event) {
        InputBuilder::new().add_event(event).send();
    }

    fn mouse_move(dx: f64, dy: f64) {
        InputBuilder::new().add_mouse_move(dx, dy).send();
    }

    fn mouse_move_to(x: f64, y: f64) {
        InputBuilder::new().add_mouse_move_to(x, y).send();
    }

    fn mouse_wheel(dx: f64, dy: f64) {
        InputBuilder::new().add_mouse_wheel(dx, dy).send();
    }

    fn mouse_button(button: MouseButton, down: bool) {
        InputBuilder::new().add_mouse_button(button, down).send();
    }

    fn keyboard(key: Key, down: bool) {
        InputBuilder::new().add_keyboard(key, down).send();
    }
}
struct InputBuilder {
    inputs: Vec<INPUT>,
}
impl InputBuilder {
    fn new() -> Self {
        Self { inputs: Vec::new() }
    }

    fn add_event(self, event: Event) -> Self {
        match event {
            Event::MouseMove { delta, .. } => self.add_mouse_move(delta.x, delta.y),
            Event::MouseMoveAbsolute { position } => {
                self.add_mouse_move_to_physical(position.x, position.y)
            }
            Event::MouseWheel { delta, .. } => self.add_mouse_wheel(delta.x, delta.y),
            Event::MouseDown { button, .. } => self.add_mouse_button(button, true),
            Event::MouseUp { button, .. } => self.add_mouse_button(button, false),
            Event::KeyDown { key, .. } => self.add_keyboard(key, true),
            Event::KeyUp { key, .. } => self.add_keyboard(key, false),
        }
    }

    fn add_mouse_move(mut self, dx: f64, dy: f64) -> Self {
        self.push_mouse(MOUSEINPUT {
            dx: dx as i32,
            dy: dy as i32,
            dwFlags: MOUSEEVENTF_MOVE,
            ..Default::default()
        });
        self
    }

    /// Adds absolute mouse movement.
    fn add_mouse_move_to(self, x: f64, y: f64) -> Self {
        let scale_factor = PlatformDisplay::get_scale_factor();
        self.add_mouse_move_to_physical(x * scale_factor, y * scale_factor)
    }

    fn add_mouse_move_to_physical(mut self, x: f64, y: f64) -> Self {
        // Get the boundary of the entire virtual desktop (multi-monitor support).
        let (vx, vy, vw, vh) = PlatformDisplay::get_virtual_screen_bounds();

        if vw <= 1.0 || vh <= 1.0 {
            return self;
        }

        // 使用目标像素对应归一化区间的中点，保证负坐标及屏幕边缘可正确回放。
        let dx = ((x - vx + 0.5) * 65536.0 / vw).floor().clamp(0.0, 65535.0) as i32;
        let dy = ((y - vy + 0.5) * 65536.0 / vh).floor().clamp(0.0, 65535.0) as i32;

        self.push_mouse(MOUSEINPUT {
            dx,
            dy,
            // MOUSEEVENTF_VIRTUALDESK ensures correct mapping across all monitors.
            dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
            ..Default::default()
        });

        self
    }

    fn add_mouse_button(mut self, button: MouseButton, down: bool) -> Self {
        let (flags, data) = match (button, down) {
            (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, 1),
            (MouseButton::Back, false) => (MOUSEEVENTF_XUP, 1),
            (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, 2),
            (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, 2),
        };
        self.push_mouse(MOUSEINPUT {
            mouseData: data,
            dwFlags: flags,
            ..Default::default()
        });
        self
    }

    fn add_mouse_wheel(mut self, dx: f64, dy: f64) -> Self {
        if dy.abs() > f64::EPSILON {
            self.push_mouse(MOUSEINPUT {
                mouseData: (dy * 120.0) as i32 as u32,
                dwFlags: MOUSEEVENTF_WHEEL,
                ..Default::default()
            });
        }
        if dx.abs() > f64::EPSILON {
            self.push_mouse(MOUSEINPUT {
                mouseData: (dx * 120.0) as i32 as u32,
                dwFlags: MOUSEEVENTF_HWHEEL,
                ..Default::default()
            });
        }
        self
    }

    /// Adds a keyboard event to the input queue.
    fn add_keyboard(mut self, key: Key, down: bool) -> Self {
        // 1. Get Windows-specific Virtual Key and Scan Code from cross-platform Key enum.
        let (vk, scancode) = match get_win_codes(key) {
            Some(codes) => codes,
            None => return self,
        };

        // 2. Determine whether to use Scan Code or Virtual Key mode.
        // Referencing logic: scancode mode is preferred for better compatibility with physical layouts.
        let (w_vk, w_scan, mut flags) = if scancode != 0 {
            // Scan code mode: VK is set to 0.
            (0u16, scancode as u16, KEYEVENTF_SCANCODE.0)
        } else {
            // Virtual key mode: Scancode is set to 0.
            (vk as u16, 0u16, 0u32)
        };

        // 3. Handle Extended Key flags.
        // If the scan code has an 0xE0 or 0xE1 prefix, the KEYEVENTF_EXTENDEDKEY flag must be set.
        if (w_scan >> 8) == 0xE0 || (w_scan >> 8) == 0xE1 {
            flags |= KEYEVENTF_EXTENDEDKEY.0;
        }

        // 4. Handle Key Up flag.
        if !down {
            flags |= KEYEVENTF_KEYUP.0;
        }

        self.push_keyboard(KEYBDINPUT {
            wVk: VIRTUAL_KEY(w_vk),
            wScan: w_scan,
            dwFlags: KEYBD_EVENT_FLAGS(flags),
            dwExtraInfo: SIMULATED_KEYBOARD_EXTRA_INFO as usize,
            ..Default::default()
        });
        self
    }

    fn push_mouse(&mut self, mi: MOUSEINPUT) {
        self.inputs.push(INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 { mi },
        });
    }

    fn push_keyboard(&mut self, ki: KEYBDINPUT) {
        self.inputs.push(INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 { ki },
        });
    }

    fn send(self) {
        if self.inputs.is_empty() {
            return;
        }
        unsafe {
            SendInput(&self.inputs, size_of::<INPUT>() as i32);
        }
    }
}

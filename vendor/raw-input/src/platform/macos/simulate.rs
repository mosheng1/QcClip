use crate::platform::macos::keycode::key_to_code;
use crate::platform::{PlatformSimulate, SimulateImpl};
use crate::{Event, Key, MouseButton};
use core_graphics::event::{CGEvent, CGEventType, CGKeyCode, CGMouseButton, ScrollEventUnit};
use core_graphics::event_source::{CGEventSource, CGEventSourceStateID};

impl SimulateImpl for PlatformSimulate {
    fn simulate(event: Event) {
        match event {
            Event::MouseMove { delta, .. } => Self::mouse_move(delta.x, delta.y),
            Event::MouseMoveAbsolute { position } => Self::mouse_move_to(position.x, position.y),
            Event::MouseWheel { delta, .. } => Self::mouse_wheel(delta.x, delta.y),
            Event::MouseDown { button, .. } => Self::mouse_button(button, true),
            Event::MouseUp { button, .. } => Self::mouse_button(button, false),
            Event::KeyDown { key, .. } => Self::keyboard(key, true),
            Event::KeyUp { key, .. } => Self::keyboard(key, false),
        }
    }

    fn mouse_move(dx: f64, dy: f64) {
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).unwrap();
        if let Ok(event) = CGEvent::new(source) {
            let cur_pos = event.location();
            let new_x = cur_pos.x + dx;
            let new_y = cur_pos.y + dy;
            Self::mouse_move_to(new_x, new_y);
        }
    }

    fn mouse_move_to(x: f64, y: f64) {
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).unwrap();
        let pos = core_graphics::geometry::CGPoint::new(x, y);

        if let Ok(event) = CGEvent::new_mouse_event(
            source,
            CGEventType::MouseMoved,
            pos,
            CGMouseButton::Left, // This parameter is ignored when moving
        ) {
            event.post(core_graphics::event::CGEventTapLocation::HID);
        }
    }

    fn mouse_wheel(dx: f64, dy: f64) {
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).unwrap();
        if let Ok(event) =
            CGEvent::new_scroll_event(source, ScrollEventUnit::PIXEL, 2, dy as i32, dx as i32, 0)
        {
            event.post(core_graphics::event::CGEventTapLocation::HID);
        }
    }

    fn mouse_button(button: MouseButton, down: bool) {
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).unwrap();

        let cur_event = CGEvent::new(source.clone()).unwrap();
        let pos = cur_event.location();

        let (event_type, cg_button) = match (button, down) {
            (MouseButton::Left, true) => (CGEventType::LeftMouseDown, CGMouseButton::Left),
            (MouseButton::Left, false) => (CGEventType::LeftMouseUp, CGMouseButton::Left),
            (MouseButton::Right, true) => (CGEventType::RightMouseDown, CGMouseButton::Right),
            (MouseButton::Right, false) => (CGEventType::RightMouseUp, CGMouseButton::Right),
            (MouseButton::Middle, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
            (MouseButton::Middle, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
            // For Back and Forward, use the OtherMouseDown/Up and Center button types consistently.
            (MouseButton::Back, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
            (MouseButton::Back, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
            (MouseButton::Forward, true) => (CGEventType::OtherMouseDown, CGMouseButton::Center),
            (MouseButton::Forward, false) => (CGEventType::OtherMouseUp, CGMouseButton::Center),
        };

        if let Ok(event) = CGEvent::new_mouse_event(source, event_type, pos, cg_button) {
            // If it's a back/forward button, you need to set the specific button number
            if matches!(button, MouseButton::Back | MouseButton::Forward) {
                let btn_num = if button == MouseButton::Back { 3 } else { 4 };
                event.set_integer_value_field(
                    core_graphics::event::EventField::MOUSE_EVENT_BUTTON_NUMBER,
                    btn_num,
                );
            }
            event.post(core_graphics::event::CGEventTapLocation::HID);
        }
    }

    fn keyboard(key: Key, down: bool) {
        let source = CGEventSource::new(CGEventSourceStateID::CombinedSessionState).unwrap();

        let key_code = match key_to_code(key) {
            Some(code) => code as CGKeyCode,
            None => return,
        };

        if let Ok(event) = CGEvent::new_keyboard_event(source, key_code, down) {
            event.post(core_graphics::event::CGEventTapLocation::HID);
        }
    }
}

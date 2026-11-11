#[cfg(feature = "serialize")]
use serde::{Deserialize, Serialize};

use crate::key::{Key, KeyCode};

/// Represents the standard buttons on a mouse.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serialize", derive(Serialize, Deserialize))]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// A simple coordinate point using integers, typically for pixel positions.
#[derive(Debug, Copy, Clone, PartialEq)]
#[cfg_attr(feature = "serialize", derive(Serialize, Deserialize))]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// The main event enum containing all possible input actions.
///
/// # Example
/// ```
/// use raw_input::{Event, Key};
///
/// fn handle_event(event: Event) {
///     match event {
///         Event::KeyDown { key: Key::Escape } => println!("Escape pressed!"),
///         _ => {}
///     }
/// }
/// ```
#[derive(Debug, Copy, Clone, PartialEq)]
#[cfg_attr(feature = "serialize", derive(Serialize, Deserialize))]
pub enum Event {
    MouseMove { delta: Point },
    /// 鼠标绝对移动，position 为全局屏幕坐标（Windows 下为物理像素）。
    MouseMoveAbsolute { position: Point },
    MouseWheel { delta: Point },
    MouseDown { button: MouseButton },
    MouseUp { button: MouseButton },
    /// Keyboard key press.
    ///
    /// `code` is a platform-specific raw key identifier (e.g. scancode or virtual key).
    /// It is optional and may be unavailable on some platforms or synthetic events.
    KeyDown { key: Key, code: Option<KeyCode> },
    /// Keyboard key release.
    ///
    /// `code` is a platform-specific raw key identifier (e.g. scancode or virtual key).
    /// It is optional and may be unavailable on some platforms or synthetic events.
    KeyUp { key: Key, code: Option<KeyCode> },
}

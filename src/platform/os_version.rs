use std::sync::OnceLock;

/// 当前系统所属的版本档位，用于决定可用的窗口材质。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowsVersion {
    /// Windows 11 22H2（build 22621）及以上。
    Windows11,
    /// Windows 10（build 10240）到 Windows 11 21H2。
    Windows10,
    /// Windows 10 之前的系统。
    Earlier,
    /// 非 Windows 平台。
    #[cfg(not(target_os = "windows"))]
    NotWindows,
}

impl WindowsVersion {
    /// Windows 上与传统模糊互斥，非 Windows 保留原有支持。
    pub(crate) const fn supports_acrylic(self) -> bool {
        match self {
            #[cfg(not(target_os = "windows"))]
            Self::NotWindows => true,
            _ => !self.supports_blur(),
        }
    }

    /// 是否支持传统的窗口背景模糊。
    pub(crate) const fn supports_blur(self) -> bool {
        !matches!(self, Self::Windows11)
    }
}

/// 读取当前系统版本档位，结果在进程内缓存。
pub(crate) fn current() -> WindowsVersion {
    static CACHED: OnceLock<WindowsVersion> = OnceLock::new();
    *CACHED.get_or_init(detect)
}

/// 读取系统 build number，读取失败或非 Windows 平台时返回 `None`。
#[cfg(target_os = "windows")]
pub(crate) fn build_number() -> Option<u32> {
    use windows::Wdk::System::SystemServices::RtlGetVersion;

    let mut version = unsafe { std::mem::zeroed() };
    // SAFETY: RtlGetVersion 只写入传入的 OSVERSIONINFOW，指针指向本栈帧上的有效结构。
    let status = unsafe { RtlGetVersion(&mut version) };
    status.is_ok().then_some(version.dwBuildNumber)
}

#[cfg(not(target_os = "windows"))]
pub(crate) fn build_number() -> Option<u32> {
    None
}

#[cfg(target_os = "windows")]
fn detect() -> WindowsVersion {
    // Windows 11 移除了 blur-behind 的模糊能力，只有亚克力可用，按 build 号分档。
    match build_number() {
        Some(22621..) => WindowsVersion::Windows11,
        Some(10240..) => WindowsVersion::Windows10,
        Some(_) => WindowsVersion::Earlier,
        None => WindowsVersion::Windows10,
    }
}

#[cfg(not(target_os = "windows"))]
fn detect() -> WindowsVersion {
    WindowsVersion::NotWindows
}

#[cfg(test)]
mod tests {
    use super::WindowsVersion;

    #[test]
    fn windows11_supports_acrylic_only() {
        let version = WindowsVersion::Windows11;
        assert!(version.supports_acrylic());
        assert!(!version.supports_blur());
    }

    #[test]
    fn windows10_supports_blur_only() {
        let version = WindowsVersion::Windows10;
        assert!(!version.supports_acrylic());
        assert!(version.supports_blur());
    }

    #[test]
    fn earlier_windows_supports_blur_only() {
        let version = WindowsVersion::Earlier;
        assert!(!version.supports_acrylic());
        assert!(version.supports_blur());
    }
}

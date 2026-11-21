//! Windows 当前用户登录自启。

#[cfg(target_os = "windows")]
mod windows_impl {
    use std::os::windows::process::CommandExt;
    use std::{env, io, path::Path, process::Command};

    use winreg::{RegKey, enums::HKEY_CURRENT_USER};

    const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE_NAME: &str = "QcClip";
    const TASK_NAME: &str = "QcClip";
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    pub(crate) fn set_enabled(enabled: bool, run_as_administrator: bool) -> io::Result<()> {
        let current_user = RegKey::predef(HKEY_CURRENT_USER);
        let (run_key, _) = current_user.create_subkey(RUN_KEY_PATH)?;
        if enabled {
            create_task(run_as_administrator)?;
            delete_run_value(&run_key)
        } else {
            delete_task()?;
            delete_run_value(&run_key)?;
            Ok(())
        }
    }

    fn create_task(run_as_administrator: bool) -> io::Result<()> {
        let executable = env::current_exe()?;
        let command = startup_command(&executable);
        let run_level = if run_as_administrator {
            "HIGHEST"
        } else {
            "LIMITED"
        };
        let status = Command::new("schtasks.exe")
            .args([
                "/create", "/sc", "onlogon", "/tn", TASK_NAME, "/tr", &command, "/rl", run_level,
                "/it", "/f",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "创建登录任务失败，退出码：{}",
                status.code().unwrap_or(-1)
            )))
        }
    }

    fn delete_task() -> io::Result<()> {
        let query = Command::new("schtasks.exe")
            .args(["/query", "/tn", TASK_NAME])
            .creation_flags(CREATE_NO_WINDOW)
            .status()?;
        if !query.success() {
            return Ok(());
        }
        let status = Command::new("schtasks.exe")
            .args(["/delete", "/tn", TASK_NAME, "/f"])
            .creation_flags(CREATE_NO_WINDOW)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "删除登录任务失败，退出码：{}",
                status.code().unwrap_or(-1)
            )))
        }
    }

    fn delete_run_value(run_key: &winreg::RegKey) -> io::Result<()> {
        match run_key.delete_value(VALUE_NAME) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn startup_command(executable: &Path) -> String {
        format!(r#""{}""#, executable.display())
    }
}

#[cfg(target_os = "windows")]
pub(crate) use windows_impl::set_enabled;

#[cfg(not(target_os = "windows"))]
pub(crate) fn set_enabled(_enabled: bool, _run_as_administrator: bool) -> std::io::Result<()> {
    Ok(())
}

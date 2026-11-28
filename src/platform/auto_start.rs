//! Windows 当前用户登录自启。

#[cfg(target_os = "windows")]
mod windows_impl {
    use std::{env, io, path::Path};

    use windows::{
        Win32::{
            Foundation::{ERROR_FILE_NOT_FOUND, RPC_E_CHANGED_MODE},
            System::{
                Com::{
                    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance,
                    CoInitializeEx, CoUninitialize,
                },
                TaskScheduler::{
                    ITaskService, TASK_CREATE_OR_UPDATE, TASK_LOGON_INTERACTIVE_TOKEN,
                    TaskScheduler,
                },
                Variant::VARIANT,
            },
        },
        core::{BSTR, HRESULT},
    };
    use winreg::{RegKey, enums::HKEY_CURRENT_USER};

    const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
    const VALUE_NAME: &str = "QcClip";
    const TASK_NAME: &str = "QcClip";

    pub(crate) fn set_enabled(enabled: bool, run_as_administrator: bool) -> io::Result<()> {
        let current_user = RegKey::predef(HKEY_CURRENT_USER);
        let (run_key, _) = current_user.create_subkey(RUN_KEY_PATH)?;
        sync_task(TASK_NAME, enabled, run_as_administrator)
            .map_err(|error| io::Error::other(format!("同步登录任务失败：{error}")))?;
        match run_key.delete_value(VALUE_NAME) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    struct ComGuard(bool);

    impl Drop for ComGuard {
        fn drop(&mut self) {
            if self.0 {
                // 只释放本次调用取得的 COM 初始化计数。
                unsafe { CoUninitialize() };
            }
        }
    }

    fn sync_task(
        task_name: &str,
        enabled: bool,
        run_as_administrator: bool,
    ) -> windows::core::Result<()> {
        // GPUI 可能已经初始化 COM；已有另一种线程模型时直接复用。
        let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if initialized.is_err() && initialized != RPC_E_CHANGED_MODE {
            initialized.ok()?;
        }
        let _com = ComGuard(initialized.is_ok());
        // COM 接口均在当前线程使用，并先于 _com 释放。
        let service: ITaskService =
            unsafe { CoCreateInstance(&TaskScheduler, None, CLSCTX_INPROC_SERVER) }?;
        let empty = VARIANT::default();
        unsafe { service.Connect(&empty, &empty, &empty, &empty) }?;
        let folder = unsafe { service.GetFolder(&BSTR::from("\\")) }?;
        let name = BSTR::from(task_name);
        if !enabled {
            return match unsafe { folder.DeleteTask(&name, 0) } {
                Err(error) if error.code() == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) => Ok(()),
                result => result,
            };
        }
        let user = unsafe { service.ConnectedUser() }?.to_string();
        let domain = unsafe { service.ConnectedDomain() }?.to_string();
        let user = if domain.is_empty() {
            user
        } else {
            format!(r"{domain}\{user}")
        };
        let executable = env::current_exe()?;
        let xml = task_xml(&user, &executable, run_as_administrator);
        unsafe {
            folder.RegisterTask(
                &name,
                &BSTR::from(xml),
                TASK_CREATE_OR_UPDATE.0,
                &VARIANT::from(user.as_str()),
                &empty,
                TASK_LOGON_INTERACTIVE_TOKEN,
                &empty,
            )
        }?;
        Ok(())
    }

    fn task_xml(user: &str, executable: &Path, run_as_administrator: bool) -> String {
        let user = escape_xml(user);
        let executable = escape_xml(&executable.to_string_lossy());
        let run_level = if run_as_administrator {
            "HighestAvailable"
        } else {
            "LeastPrivilege"
        };
        format!(
            r#"<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers>
    <LogonTrigger><UserId>{user}</UserId></LogonTrigger>
  </Triggers>
  <Principals>
    <Principal id="CurrentUser">
      <UserId>{user}</UserId>
      <LogonType>InteractiveToken</LogonType>
      <RunLevel>{run_level}</RunLevel>
    </Principal>
  </Principals>
  <Settings>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
  </Settings>
  <Actions Context="CurrentUser">
    <Exec><Command>{executable}</Command></Exec>
  </Actions>
</Task>"#
        )
    }

    fn escape_xml(value: &str) -> String {
        value
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn task_definition_uses_current_user_permissions_and_allows_battery_power() {
            let user = r"电脑\用户&测试";
            let executable = Path::new(r"C:\剪贴板 & 工具\QcClip.exe");
            for (administrator, run_level) in
                [(false, "LeastPrivilege"), (true, "HighestAvailable")]
            {
                let xml = task_xml(user, executable, administrator);
                let document = roxmltree::Document::parse(&xml).unwrap();
                let value = |name| {
                    document
                        .descendants()
                        .find(|node| node.has_tag_name(name))
                        .unwrap()
                        .text()
                        .unwrap()
                };
                let users: Vec<_> = document
                    .descendants()
                    .filter(|node| node.has_tag_name("UserId"))
                    .map(|node| node.text().unwrap())
                    .collect();
                assert_eq!(users, [user, user]);
                assert_eq!(value("LogonType"), "InteractiveToken");
                assert_eq!(value("RunLevel"), run_level);
                assert_eq!(value("Command"), executable.to_str().unwrap());
                assert_eq!(value("DisallowStartIfOnBatteries"), "false");
                assert_eq!(value("StopIfGoingOnBatteries"), "false");
                assert_eq!(value("ExecutionTimeLimit"), "PT0S");
            }
        }

        #[test]
        #[ignore = "需要访问 Windows 任务计划程序，创建并清理独立测试任务"]
        fn current_user_can_create_update_and_delete_login_task() {
            let name = format!("QcClip-自启测试-{}", std::process::id());
            struct Cleanup(String);
            impl Drop for Cleanup {
                fn drop(&mut self) {
                    sync_task(&self.0, false, false).expect("清理测试任务失败");
                }
            }
            let _cleanup = Cleanup(name.clone());
            sync_task(&name, true, false).expect("创建普通登录任务失败");
            sync_task(&name, true, false).expect("更新普通登录任务失败");
            if crate::platform::elevation::is_elevated().unwrap() {
                sync_task(&name, true, true).expect("创建管理员登录任务失败");
                sync_task(&name, true, false).expect("切换回普通登录任务失败");
            } else {
                assert!(
                    sync_task(&name, true, true).is_err(),
                    "普通进程不能注册最高权限任务"
                );
            }
            sync_task(&name, false, false).expect("删除登录任务失败");
            sync_task(&name, false, false).expect("重复删除应当成功");
        }
    }
}

#[cfg(target_os = "windows")]
pub(crate) use windows_impl::set_enabled;

#[cfg(not(target_os = "windows"))]
pub(crate) fn set_enabled(_enabled: bool, _run_as_administrator: bool) -> std::io::Result<()> {
    Ok(())
}

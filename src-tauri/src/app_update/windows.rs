// Use the same NSIS quiet/update/restart switches as Tauri's updater plugin.
// /D must be last and unquoted: NSIS consumes the entire remainder as the path.
fn shell_path(path: &str) -> Result<String, String> {
    let path = if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(path).to_string()
    };
    if path.is_empty() || path.contains(['\0', '"', '\r', '\n']) {
        return Err("应用更新路径无效，无法自动更新".into());
    }
    Ok(path)
}

fn installer_parameters(install_dir: &str) -> Result<String, String> {
    Ok(format!("/S /UPDATE /R /D={}", shell_path(install_dir)?))
}

#[cfg(windows)]
pub(super) fn prepare_install_and_restart(
    installer: &std::path::Path,
    root: &std::path::Path,
) -> Result<(), String> {
    use windows_sys::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOW};

    if !installer
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|name| name.to_ascii_lowercase().ends_with("-setup.exe"))
    {
        return Err("Windows 应用内更新需要 NSIS 安装包（-setup.exe），请重新下载".into());
    }
    let exe = std::env::current_exe().map_err(|e| format!("读取应用安装位置失败: {e}"))?;
    let install_dir = exe
        .parent()
        .and_then(|p| p.to_str())
        .ok_or("无法确定当前应用安装目录")?;
    let parameters = installer_parameters(install_dir)?;
    // canonicalize() returns extended paths on Windows; ShellExecute/NSIS need normal paths.
    let file = shell_path(installer.to_str().ok_or("更新安装包路径无法识别")?)?;
    let file: Vec<u16> = file.encode_utf16().chain(Some(0)).collect();
    let parameters: Vec<u16> = parameters.encode_utf16().chain(Some(0)).collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();

    // Complete fallible bookkeeping before launching; a failed launch keeps the app open.
    super::mark_updates_cleanup_pending(root)?;
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            parameters.as_ptr(),
            std::ptr::null(),
            SW_SHOW,
        )
    } as isize;
    if result <= 32 {
        let _ = std::fs::remove_file(root.join(super::CLEANUP_MARKER_FILE));
        return Err(format!(
            "启动自动更新失败（Windows 错误 {result}），应用仍保持打开，请重试"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_update_restarts_in_the_existing_directory_with_spaces_and_unicode() {
        assert_eq!(
            installer_parameters(r"C:\Users\大龙\Apps\Qoder Switch").unwrap(),
            r"/S /UPDATE /R /D=C:\Users\大龙\Apps\Qoder Switch"
        );
    }

    #[test]
    fn extended_drive_and_unc_paths_use_nsis_compatible_paths() {
        assert_eq!(
            shell_path(r"\\?\C:\Users\大龙\updates\app-setup.exe").unwrap(),
            r"C:\Users\大龙\updates\app-setup.exe"
        );
        assert_eq!(
            installer_parameters(r"\\?\C:\Apps\Qoder Switch").unwrap(),
            r"/S /UPDATE /R /D=C:\Apps\Qoder Switch"
        );
        assert_eq!(
            installer_parameters(r"\\?\UNC\server\share\Qoder Switch").unwrap(),
            r"/S /UPDATE /R /D=\\server\share\Qoder Switch"
        );
    }

    #[test]
    fn invalid_install_directories_fail_before_launch() {
        for path in ["", "C:\\bad\0path", "C:\\bad\"path", "C:\\bad\npath"] {
            assert!(installer_parameters(path).is_err());
        }
    }
}

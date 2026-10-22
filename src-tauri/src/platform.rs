//! OS integration kept outside account and protocol logic.
use crate::qcore::{self, Edition};
use std::path::PathBuf;

pub fn data_dir(ed: &Edition) -> PathBuf {
    #[cfg(windows)]
    {
        let key = if ed.key == "cn" {
            "QODER_CN_DATA_DIR"
        } else {
            "QODER_DATA_DIR"
        };
        if let Some(path) = std::env::var_os(key) {
            return PathBuf::from(path);
        }
        let root = dirs::config_dir().unwrap_or_else(qcore::home_dir);
        let names = if ed.key == "cn" {
            ["com.qodercn.app.stable", "Qoder CN"]
        } else {
            ["com.qoder.app.stable", "Qoder"]
        };
        // Prefer the current native auth/session layout over legacy IDE data.
        return names
            .iter()
            .map(|n| root.join(n))
            .find(|p| {
                p.join("auth.v1.dat").is_file()
                    || p.join("main.sqlite").is_file()
                    || p.join("Local State").is_file()
            })
            .unwrap_or_else(|| root.join(names[0]));
    }
    #[cfg(not(windows))]
    qcore::home_dir().join(ed.data_dir)
}

pub fn app_executable(ed: &Edition) -> Option<PathBuf> {
    #[cfg(windows)]
    let override_key = if ed.key == "cn" {
        "QODER_CN_PATH"
    } else {
        "QODER_PATH"
    };
    #[cfg(windows)]
    if let Some(path) = std::env::var_os(override_key)
        .map(PathBuf::from)
        .filter(|p| p.is_file())
    {
        return Some(path);
    }
    #[cfg(windows)]
    {
        let names: &[&str] = if ed.key == "cn" {
            &["Qoder CN", "QoderCN"]
        } else {
            &["Qoder", "Qoder IDE"]
        };
        let mut roots = Vec::new();
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            roots.push(PathBuf::from(&local).join("Programs"));
            roots.push(PathBuf::from(local));
        }
        for key in ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"] {
            if let Some(root) = std::env::var_os(key) {
                roots.push(PathBuf::from(root));
            }
        }
        for root in roots {
            for name in names {
                let path = root.join(name).join(format!("{name}.exe"));
                if path.is_file() {
                    return Some(path);
                }
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        let path = PathBuf::from(ed.app_path)
            .join("Contents/MacOS")
            .join(ed.app_name);
        path.is_file().then_some(path)
    }
}

pub fn resources_dir(ed: &Edition) -> Option<PathBuf> {
    let exe = app_executable(ed)?;
    #[cfg(windows)]
    return Some(exe.parent()?.join("resources"));
    #[cfg(not(windows))]
    Some(exe.parent()?.parent()?.join("Resources"))
}

#[cfg(windows)]
fn powershell(ed: &Edition, action: &str) -> Option<std::process::Output> {
    // Pass paths through the environment, never interpolate them into PowerShell code.
    let exe = app_executable(ed)?;
    let script = format!(
        "$ErrorActionPreference='Stop'; try {{ $p=@(Get-Process | Where-Object {{ $_.Path -eq $env:QODER_SWITCH_EXE }}); {action} }} catch {{ exit 2 }}"
    );
    let mut command = std::process::Command::new("powershell.exe");
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x08000000);
    command
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &script,
        ])
        .env("QODER_SWITCH_EXE", exe);
    qcore::command_timeout(command, 8000)
}

#[cfg(windows)]
pub fn app_running(ed: &Edition) -> bool {
    if app_executable(ed).is_none() {
        return false;
    }
    // Inspection errors must not allow account/session writes into a running app.
    powershell(ed, "if($p.Count -gt 0){exit 0}else{exit 1}")
        .is_none_or(|o| o.status.code() != Some(1))
}

#[cfg(windows)]
pub fn quit_app(ed: &Edition) -> bool {
    if powershell(ed, "$p | ForEach-Object { [void]$_.CloseMainWindow() }")
        .is_none_or(|o| !o.status.success())
    {
        return false;
    }
    for _ in 0..20 {
        if !app_running(ed) {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    // Force only processes whose executable matches the selected installation.
    powershell(ed, "$p | Stop-Process -Force; exit 0").is_some_and(|o| o.status.success())
        && !app_running(ed)
}

pub fn open_url(url: &str) -> Result<(), String> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err("仅允许打开 HTTP(S) 地址".into());
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = std::ffi::OsStr::new(url)
            .encode_wide()
            .chain(Some(0))
            .collect();
        let result = unsafe {
            windows_sys::Win32::UI::Shell::ShellExecuteW(
                std::ptr::null_mut(),
                std::ptr::null(),
                wide.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                1,
            )
        };
        if result as isize <= 32 {
            return Err("无法打开默认浏览器".into());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        qcore::run_cmd_timeout("open", &[url], 8000)
            .filter(|o| o.status.success())
            .map(|_| ())
            .ok_or("无法打开默认浏览器".into())
    }
}

#[cfg(windows)]
pub fn protect(input: &[u8], decrypt: bool) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::{Foundation::LocalFree, Security::Cryptography::*};
    let source = CRYPT_INTEGER_BLOB {
        cbData: input.len().try_into().map_err(|_| "登录态过大")?,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        if decrypt {
            CryptUnprotectData(
                &source,
                std::ptr::null_mut(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptProtectData(
                &source,
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if ok == 0 {
        return Err("Windows 登录态加解密失败，请在当前 Windows 用户下重新登录或采集账号".into());
    }
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec() };
    unsafe {
        LocalFree(output.pbData as *mut _);
    }
    Ok(bytes)
}

// Electron/Chromium Windows safeStorage v10 is AES-256-GCM with a
// DPAPI-wrapped key in Local State, not macOS v10 CBC or a raw DPAPI blob.
#[cfg(windows)]
fn auth_key(ed: &Edition) -> Result<Vec<u8>, String> {
    use base64::Engine;
    let state = std::fs::read(data_dir(ed).join("Local State"))
        .map_err(|_| "请先启动对应 Qoder 并在当前 Windows 用户下登录一次")?;
    let state: serde_json::Value =
        serde_json::from_slice(&state).map_err(|_| "Local State 格式无效")?;
    let key = state
        .pointer("/os_crypt/encrypted_key")
        .and_then(|v| v.as_str())
        .ok_or("Local State 缺少登录态密钥")?;
    let key = base64::engine::general_purpose::STANDARD
        .decode(key)
        .map_err(|_| "登录态密钥格式无效")?;
    let key = key
        .strip_prefix(b"DPAPI")
        .ok_or("不支持此 Windows 登录态密钥格式")?;
    protect(key, true)
}

#[cfg(windows)]
fn crypt_auth(key: &[u8], input: &[u8], nonce: Option<&[u8; 12]>) -> Result<Vec<u8>, String> {
    use aes_gcm::{aead::Aead, Aes256Gcm, KeyInit, Nonce};
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| "Windows 登录态密钥长度无效")?;
    if let Some(nonce) = nonce {
        let encrypted = cipher
            .encrypt(Nonce::from_slice(nonce), input)
            .map_err(|_| "Windows 登录态加密失败")?;
        Ok([b"v10".as_slice(), nonce.as_slice(), &encrypted].concat())
    } else {
        if !input.starts_with(b"v10") || input.len() < 31 {
            return Err("不支持此 Windows 登录态格式".into());
        }
        cipher
            .decrypt(Nonce::from_slice(&input[3..15]), &input[15..])
            .map_err(|_| "Windows 登录态解密失败".into())
    }
}

#[cfg(windows)]
pub fn decrypt_auth(ed: &Edition, raw: &[u8]) -> Result<Vec<u8>, String> {
    if raw.starts_with(b"v10") {
        crypt_auth(&auth_key(ed)?, raw, None)
    } else {
        protect(raw, true)
    }
}

#[cfg(windows)]
pub fn encrypt_auth(ed: &Edition, plain: &[u8]) -> Result<Vec<u8>, String> {
    let mut nonce = [0; 12];
    getrandom::fill(&mut nonce).map_err(|_| "系统随机源不可用")?;
    crypt_auth(&auth_key(ed)?, plain, Some(&nonce))
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn chromium_v10_roundtrip_and_tamper() {
        let key = [7; 32];
        let mut encrypted = super::crypt_auth(&key, b"synthetic", Some(&[8; 12])).unwrap();
        assert!(encrypted.starts_with(b"v10"));
        assert_eq!(
            super::crypt_auth(&key, &encrypted, None).unwrap(),
            b"synthetic"
        );
        encrypted[16] ^= 1;
        assert!(super::crypt_auth(&key, &encrypted, None).is_err());
        assert!(super::crypt_auth(&key, b"v10", None).is_err());
    }
    #[test]
    fn dpapi_roundtrip_and_invalid_blob() {
        let plain = br#"{"token":"synthetic-test-only"}"#;
        let encrypted = super::protect(plain, false).unwrap();
        assert_ne!(encrypted, plain);
        assert_eq!(super::protect(&encrypted, true).unwrap(), plain);
        assert!(super::protect(b"invalid", true).is_err());
    }
}

//! qoder-switch 核心逻辑：safeStorage 解密、设备标识、Qoder API、账号库、切换与领取。
//! 数据目录与 Python 版共用 ~/.qoder-switch/，账号库格式兼容。
#![allow(non_snake_case)]

use aes::Aes128;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha1::Sha1;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

// ───────────────────────── 常量 ─────────────────────────

pub struct Edition {
    pub key: &'static str,
    pub label: &'static str,
    pub app_name: &'static str,
    pub app_path: &'static str,
    pub data_dir: &'static str, // 相对 ~
    pub home_dir: &'static str, // 相对 ~
    pub keychain_service: &'static str,
    pub api_base: &'static str,
    pub auth_base: &'static str, // 浏览器登录站点
    pub client_id: &'static str, // 官方 device flow clientId（两版相同）
    pub biz_variant: &'static str,
}

pub const EDITIONS: &[Edition] = &[
    Edition {
        key: "intl",
        label: "Qoder 国际版",
        app_name: "Qoder",
        app_path: "/Applications/Qoder.app",
        data_dir: "Library/Application Support/com.qoder.app.stable",
        home_dir: ".qoder",
        keychain_service: "Qoder App Safe Storage",
        api_base: "https://openapi.qoder.sh",
        auth_base: "https://qoder.com",
        client_id: "732aef47-9cf2-46a2-95fe-4cebb5d0d1fa",
        biz_variant: "qoder",
    },
    Edition {
        key: "cn",
        label: "Qoder CN 国内版",
        app_name: "Qoder CN",
        app_path: "/Applications/Qoder CN.app",
        data_dir: "Library/Application Support/com.qodercn.app.stable",
        home_dir: ".qoder-cn",
        keychain_service: "Qoder CN App Safe Storage",
        api_base: "https://openapi.qoder.com.cn",
        auth_base: "https://qoder.cn",
        client_id: "732aef47-9cf2-46a2-95fe-4cebb5d0d1fa",
        biz_variant: "qoder",
    },
];

/// 带超时执行外部命令：osascript/pkill/open 等可能因系统授权弹窗（被遮挡时）无限阻塞，
/// 导致切换/复制流程永远卡在"正在切换账号…"，这里统一加超时兜底。
pub fn run_cmd_timeout(prog: &str, args: &[&str], timeout_ms: u64) -> Option<std::process::Output> {
    let mut command = Command::new(prog);
    command.args(args);
    command_timeout(command, timeout_ms)
}

pub fn command_timeout(mut command: Command, timeout_ms: u64) -> Option<std::process::Output> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let poll = 100u64;
    let mut waited = 0u64;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) => {
                if waited >= timeout_ms {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(Duration::from_millis(poll));
                waited += poll;
            }
            Err(_) => return None,
        }
    }
}

pub fn edition(key: &str) -> Option<&'static Edition> {
    EDITIONS.iter().find(|e| e.key == key)
}

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"))
}

pub fn home_dir() -> PathBuf {
    home()
}

pub fn switch_dir() -> PathBuf {
    home().join(".qoder-switch")
}

fn accounts_file() -> PathBuf {
    switch_dir().join("accounts.json")
}

fn claim_log_file() -> PathBuf {
    switch_dir().join("claim-log.json")
}

fn settings_file() -> PathBuf {
    switch_dir().join("settings.json")
}

// ───────────────────────── 加解密 ─────────────────────────

fn keychain_password(service: &str) -> Result<String, String> {
    // 加超时：GUI 应用里 security CLI 可能因授权问题卡住，5 秒超时兜底
    let mut child = Command::new("security")
        .args(["find-generic-password", "-s", service, "-w"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    for _ in 0..50 {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = child.wait_with_output().map_err(|e| e.to_string())?;
                if !status.success() {
                    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
                    return Err(format!("钥匙串读取失败({}): {}", service, err));
                }
                return Ok(String::from_utf8_lossy(&out.stdout).trim().to_string());
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(e) => return Err(e.to_string()),
        }
    }
    let _ = child.kill();
    Err(format!(
        "钥匙串读取超时（{}）：如弹出授权框请点「始终允许」",
        service
    ))
}

fn pbkdf2_hmac_sha1(password: &[u8], salt: &[u8], iterations: u32, out: &mut [u8]) {
    type H = Hmac<Sha1>;
    let blocks = (out.len() + 19) / 20;
    for i in 1..=blocks {
        let mut mac = <H as Mac>::new_from_slice(password).expect("hmac key");
        mac.update(salt);
        mac.update(&(i as u32).to_be_bytes());
        let mut u = mac.finalize().into_bytes();
        let mut t = u;
        for _ in 1..iterations {
            let mut mac = <H as Mac>::new_from_slice(password).expect("hmac key");
            mac.update(&u);
            u = mac.finalize().into_bytes();
            for (tb, ub) in t.iter_mut().zip(u.iter()) {
                *tb ^= *ub;
            }
        }
        let start = (i - 1) * 20;
        let end = (start + 20).min(out.len());
        out[start..end].copy_from_slice(&t[..end - start]);
    }
}

fn aes_key(service: &str) -> Result<[u8; 16], String> {
    let pw = keychain_password(service)?;
    let mut key = [0u8; 16];
    pbkdf2_hmac_sha1(pw.as_bytes(), b"saltysalt", 1003, &mut key);
    Ok(key)
}

type Aes128CbcDec = cbc::Decryptor<Aes128>;
type Aes128CbcEnc = cbc::Encryptor<Aes128>;

fn decrypt_v10(key: &[u8; 16], raw: &[u8]) -> Result<Vec<u8>, String> {
    if raw.len() < 3 || &raw[..3] != b"v10" {
        return Err("auth.v1.dat 格式不是 v10".into());
    }
    let iv = [b' '; 16];
    Aes128CbcDec::new(key.into(), &iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(&raw[3..])
        .map_err(|e| format!("AES 解密失败: {:?}", e))
}

fn encrypt_v10(key: &[u8; 16], pt: &[u8]) -> Vec<u8> {
    let iv = [b' '; 16];
    let mut ct = Aes128CbcEnc::new(key.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(pt);
    let mut out = b"v10".to_vec();
    out.append(&mut ct);
    out
}

pub fn auth_dat_path(ed: &Edition) -> PathBuf {
    crate::platform::data_dir(ed).join("auth.v1.dat")
}

/// 解密当前客户端 auth.v1.dat → JSON session
pub fn decrypt_auth_dat(ed: &Edition) -> Option<Value> {
    let raw = fs::read(auth_dat_path(ed)).ok()?;
    #[cfg(windows)]
    let pt = crate::platform::decrypt_auth(ed, &raw).ok()?;
    #[cfg(not(windows))]
    let pt = decrypt_v10(&aes_key(ed.keychain_service).ok()?, &raw).ok()?;
    serde_json::from_slice(&pt).ok()
}

pub fn encrypt_session(ed: &Edition, session: &Value) -> Result<Vec<u8>, String> {
    let pt = serde_json::to_vec(session).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    return crate::platform::encrypt_auth(ed, &pt);
    #[cfg(not(windows))]
    Ok(encrypt_v10(&aes_key(ed.keychain_service)?, &pt))
}

/// 由账号 tokens 构造客户端登录态（auth.v1.dat 内容，base64）。
/// OAuth 设备流添加的账号从未在本客户端登录过，磁盘上没有属于它的 auth.v1.dat，
/// 切换时必须用 tokens 现场构造会话 JSON（schema 与客户端解密结果一致）并用钥匙串密钥加密。
pub fn build_auth_blob(ed: &Edition, acc: &Account) -> Result<String, String> {
    if acc.token.is_empty() {
        return Err("账号缺少 token，无法构造登录态".into());
    }
    let session = json!({
        "schemaVersion": 1,
        "token": acc.token,
        "refreshToken": acc.refreshToken,
        "expiresAt": acc.expiresAt,
        "refreshTokenExpiresAt": acc.refreshTokenExpiresAt,
        "user": {
            "id": acc.uid,
            "name": acc.name,
            "email": acc.email,
            "phone": acc.phone,
            "avatarUrl": acc.avatarUrl,
        },
    });
    let blob = encrypt_session(ed, &session)?;
    Ok(B64.encode(blob))
}

fn write_auth_dat(ed: &Edition, session: &Value) -> Result<(), String> {
    let blob = encrypt_session(ed, session)?;
    write_auth_blob(ed, &blob)
}

fn write_auth_blob(ed: &Edition, blob: &[u8]) -> Result<(), String> {
    let path = auth_dat_path(ed);
    let tmp = path.with_extension("qoderswitch-tmp");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::write(&tmp, blob).map_err(|e| e.to_string())?;
    fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(())
}

// ───────────────────────── 设备标识 ─────────────────────────

fn run_runtime_info(ed: &Edition) -> Value {
    let Some(resources) = crate::platform::resources_dir(ed) else {
        return json!({});
    };
    let exe = resources.join("umid").join(if cfg!(windows) {
        "runtime-info.exe"
    } else {
        "runtime-info"
    });
    if !exe.exists() {
        return json!({});
    }
    let mut command = Command::new(&exe);
    command.arg("--account-stdin");
    match command_timeout(command, 8000) {
        Some(o) => {
            let text = String::from_utf8_lossy(&o.stdout);
            let last = text
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            serde_json::from_str(last).unwrap_or(json!({}))
        }
        None => json!({}),
    }
}

pub fn device_headers(ed: &Edition) -> Vec<(String, String)> {
    // umid：优先 ~/.qoder[-cn]/umid-cache.json，其次 runtime-info
    let mut umid = json!({});
    let cache = home().join(ed.home_dir).join("umid-cache.json");
    if let Ok(text) = fs::read_to_string(&cache) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            if v.pointer("/info/machineToken")
                .and_then(|x| x.as_str())
                .is_some()
            {
                umid = v.get("info").cloned().unwrap_or(json!({}));
            }
        }
    }
    if umid
        .get("machineToken")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .is_empty()
    {
        umid = run_runtime_info(ed);
    }

    let mut machine_id = String::new();
    for p in [
        crate::platform::data_dir(ed).join("auth.machine-id"),
        home().join(ed.home_dir).join(".auth").join("machine_id"),
    ] {
        if let Ok(s) = fs::read_to_string(&p) {
            let s = s.trim().to_string();
            if !s.is_empty() {
                machine_id = s;
                break;
            }
        }
    }

    let mut version = String::new();
    let mf = crate::platform::resources_dir(ed).map(|p| p.join("build-manifest.json"));
    if let Some(text) = mf.and_then(|p| fs::read_to_string(p).ok()) {
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            version = v
                .get("productVersion")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
        }
    }

    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    let hostname = hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();

    let g = |k: &str| {
        umid.get(k)
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    };
    let mut h = vec![
        ("Cosy-ClientType".to_string(), "10".to_string()),
        (
            "Cosy-MachineOS".to_string(),
            format!(
                "{}_{}",
                arch,
                if cfg!(windows) { "windows" } else { "darwin" }
            ),
        ),
        ("Cosy-MachineHostname".to_string(), hostname),
    ];
    if !machine_id.is_empty() {
        h.push(("Cosy-MachineId".to_string(), machine_id));
    }
    for (k, hk) in [
        ("machineToken", "Cosy-MachineToken"),
        ("machineCode", "Cosy-MachineCode"),
        ("machineType", "Cosy-MachineType"),
    ] {
        let v = g(k);
        if !v.is_empty() {
            h.push((hk.to_string(), v));
        }
    }
    if !version.is_empty() {
        h.push(("Cosy-Version".to_string(), version));
    }
    h
}

// ───────────────────────── HTTP ─────────────────────────

pub fn api_call(
    ed: &Edition,
    path: &str,
    method: &str,
    token: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let url = format!("{}{}", ed.api_base, path);
    let mut req = if method == "POST" {
        ureq::post(&url)
    } else {
        ureq::get(&url)
    };
    req = req
        .set("Accept", "application/json")
        .set("User-Agent", "Qoder/switch")
        .timeout(Duration::from_secs(30));
    for (k, v) in device_headers(ed) {
        req = req.set(&k, &v);
    }
    if !token.is_empty() {
        req = req.set("Authorization", &format!("Bearer {}", token));
    }
    let resp = if method == "POST" {
        req.set("Content-Type", "application/json")
            .send_string(&body.unwrap_or(json!({})).to_string())
    } else {
        req.call()
    };
    match resp {
        Ok(r) => {
            let status = r.status();
            let v = r.into_json::<Value>().unwrap_or(json!({}));
            (status, v)
        }
        Err(ureq::Error::Status(code, r)) => {
            let v = r.into_json::<Value>().unwrap_or(json!({}));
            (code, v)
        }
        Err(e) => (0, json!({ "error": e.to_string() })),
    }
}

// ───────────────────────── 账号库 ─────────────────────────

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Account {
    pub edition: String,
    pub uid: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub phone: String,
    #[serde(default)]
    pub note: String,
    #[serde(default)]
    pub avatarUrl: String,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub refreshToken: String,
    #[serde(default)]
    pub expiresAt: String,
    #[serde(default)]
    pub refreshTokenExpiresAt: String,
    #[serde(default)]
    pub authBlob: String,
    #[serde(default)]
    pub savedAt: String,
}

impl Account {
    pub fn id(&self) -> String {
        format!("{}:{}", self.edition, self.uid)
    }
}

pub fn load_accounts() -> Vec<Account> {
    let text = match fs::read_to_string(accounts_file()) {
        Ok(t) => t,
        Err(_) => return vec![],
    };
    let v: Value = serde_json::from_str(&text).unwrap_or(json!({}));
    serde_json::from_value(v.get("accounts").cloned().unwrap_or(json!([]))).unwrap_or_default()
}

pub fn save_accounts(accounts: &[Account]) -> Result<(), String> {
    fs::create_dir_all(switch_dir()).map_err(|e| e.to_string())?;
    let doc = json!({ "accounts": accounts });
    let tmp = accounts_file().with_extension("tmp");
    fs::write(
        &tmp,
        serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    fs::rename(&tmp, accounts_file()).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn find_account<'a>(
    accounts: &'a mut [Account],
    edition_key: &str,
    uid: &str,
) -> Option<&'a mut Account> {
    accounts
        .iter_mut()
        .find(|a| a.edition == edition_key && a.uid == uid)
}

/// 采集某版本当前登录账号 → (account, is_new)
pub fn capture_current(edition_key: &str) -> Result<(Account, bool), String> {
    let ed = edition(edition_key).ok_or("未知版本")?;
    let sess = decrypt_auth_dat(ed).ok_or(format!("{} 当前未登录或读取失败", ed.label))?;
    let token = sess.get("token").and_then(|x| x.as_str()).unwrap_or("");
    if token.is_empty() {
        return Err(format!("{} 当前未登录", ed.label));
    }
    let user = sess.get("user").cloned().unwrap_or(json!({}));
    let uid = user
        .get("id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    let blob = B64.encode(fs::read(auth_dat_path(ed)).map_err(|e| e.to_string())?);

    let mut accounts = load_accounts();
    let idx = accounts
        .iter()
        .position(|a| a.edition == edition_key && a.uid == uid);
    let is_new = idx.is_none();
    if is_new {
        accounts.push(Account {
            edition: edition_key.to_string(),
            uid: uid.clone(),
            ..Default::default()
        });
    }
    let acc = {
        let i = idx.unwrap_or(accounts.len() - 1);
        &mut accounts[i]
    };
    acc.name = user
        .get("name")
        .and_then(|x| x.as_str())
        .or(user.get("email").and_then(|x| x.as_str()))
        .or(user.get("phone").and_then(|x| x.as_str()))
        .unwrap_or(&uid[..uid.len().min(8)])
        .to_string();
    acc.email = user
        .get("email")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    acc.phone = user
        .get("phone")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    acc.avatarUrl = user
        .get("avatarUrl")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    acc.token = token.to_string();
    acc.refreshToken = sess
        .get("refreshToken")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    acc.expiresAt = sess
        .get("expiresAt")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    acc.refreshTokenExpiresAt = sess
        .get("refreshTokenExpiresAt")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    acc.authBlob = blob;
    acc.savedAt = chrono::Utc::now().to_rfc3339();
    let result = acc.clone();
    save_accounts(&accounts)?;
    Ok((result, is_new))
}

pub fn live_uid(edition_key: &str) -> String {
    edition(edition_key)
        .and_then(|ed| decrypt_auth_dat(ed))
        .and_then(|s| {
            s.pointer("/user/id")
                .and_then(|x| x.as_str())
                .map(|s| s.to_string())
        })
        .unwrap_or_default()
}

// ───────────────────────── Token 刷新 ─────────────────────────

fn token_expiring(acc: &Account, hours: i64) -> bool {
    let exp = chrono::DateTime::parse_from_rfc3339(&acc.expiresAt);
    match exp {
        Ok(t) => {
            t.with_timezone(&chrono::Utc) < chrono::Utc::now() + chrono::Duration::hours(hours)
        }
        Err(_) => false,
    }
}

pub fn refresh_account_token(acc: &mut Account) -> bool {
    if acc.refreshToken.is_empty() {
        return false;
    }
    let ed = match edition(&acc.edition) {
        Some(e) => e,
        None => return false,
    };
    let (status, data) = api_call(
        ed,
        "/api/v1/deviceToken/refresh",
        "POST",
        "",
        Some(json!({ "refresh_token": acc.refreshToken })),
    );
    if status != 200 {
        return false;
    }
    // 官方响应为 snake_case：{"device_token":"dt-..","refresh_token":"drt-..",
    // "expires_at":"..","refresh_token_expires_at":".."}，兼容驼峰变体
    let tok = data
        .get("device_token")
        .and_then(|x| x.as_str())
        .or(data.get("deviceToken").and_then(|x| x.as_str()))
        .or(data.get("token").and_then(|x| x.as_str()))
        .or(data.get("accessToken").and_then(|x| x.as_str()))
        .or(data.get("access_token").and_then(|x| x.as_str()))
        .unwrap_or("");
    if tok.is_empty() {
        return false;
    }
    acc.token = tok.to_string();
    let new_rt = data
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .or(data.get("refreshToken").and_then(|x| x.as_str()))
        .map(|s| s.to_string())
        .unwrap_or_else(|| acc.refreshToken.clone());
    acc.refreshToken = new_rt;
    let mut got_expiry = false;
    if let Some(v) = data
        .get("expires_at")
        .and_then(|x| x.as_str())
        .or_else(|| data.get("expiresAt").and_then(|x| x.as_str()))
    {
        acc.expiresAt = v.to_string();
        got_expiry = true;
    }
    if let Some(v) = data
        .get("refresh_token_expires_at")
        .and_then(|x| x.as_str())
        .or_else(|| data.get("refreshTokenExpiresAt").and_then(|x| x.as_str()))
    {
        acc.refreshTokenExpiresAt = v.to_string();
        got_expiry = true;
    }
    if !got_expiry {
        if let Some(secs) = data.get("expiresIn").and_then(|x| x.as_i64()) {
            acc.expiresAt = (chrono::Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339();
        }
    }
    true
}

/// 刷新后持久化 + 若该账号正登录客户端则回写 auth.v1.dat
pub fn persist_and_writeback(acc: &Account) {
    let mut accounts = load_accounts();
    if let Some(stored) = find_account(&mut accounts, &acc.edition, &acc.uid) {
        stored.token = acc.token.clone();
        stored.refreshToken = acc.refreshToken.clone();
        stored.expiresAt = acc.expiresAt.clone();
        stored.refreshTokenExpiresAt = acc.refreshTokenExpiresAt.clone();
    }
    let _ = save_accounts(&accounts);

    if live_uid(&acc.edition) == acc.uid && !acc.uid.is_empty() {
        if let Some(ed) = edition(&acc.edition) {
            if let Some(mut sess) = decrypt_auth_dat(ed) {
                sess["token"] = json!(acc.token);
                sess["refreshToken"] = json!(acc.refreshToken);
                if !acc.expiresAt.is_empty() {
                    sess["expiresAt"] = json!(acc.expiresAt);
                }
                if !acc.refreshTokenExpiresAt.is_empty() {
                    sess["refreshTokenExpiresAt"] = json!(acc.refreshTokenExpiresAt);
                }
                if write_auth_dat(ed, &sess).is_ok() {
                    if let Ok(raw) = fs::read(auth_dat_path(ed)) {
                        let mut accounts = load_accounts();
                        if let Some(stored) = find_account(&mut accounts, &acc.edition, &acc.uid) {
                            stored.authBlob = B64.encode(raw);
                            let _ = save_accounts(&accounts);
                        }
                    }
                }
            }
        }
    }
}

pub fn ensure_token(acc: &mut Account) -> Result<(), String> {
    if acc.token.is_empty() && acc.refreshToken.is_empty() {
        return Err("无 token，请先在客户端登录后采集账号".into());
    }
    if token_expiring(acc, 72) || acc.token.is_empty() {
        if refresh_account_token(acc) {
            persist_and_writeback(acc);
            return Ok(());
        }
        if !acc.token.is_empty() && !token_expiring(acc, 0) {
            return Ok(());
        }
        return Err("token 已过期且刷新失败，请重新登录客户端后采集".into());
    }
    Ok(())
}

// ───────────────────────── 积分活动 / 领取 / 统计 ─────────────────────────

pub fn get_campaigns(acc: &Account) -> (u16, Value) {
    let ed = match edition(&acc.edition) {
        Some(e) => e,
        None => return (0, json!({"error": "未知版本"})),
    };
    api_call(ed, "/sash/api/v1/me/campaigns", "GET", &acc.token, None)
}

pub fn claimable_campaigns(data: &Value) -> Vec<Value> {
    data.get("campaigns")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter(|c| {
                    c.get("actionType").and_then(|x| x.as_str()) == Some("CLAIM_BENEFIT")
                        && c.get("claimStatus").and_then(|x| x.as_str()) == Some("CLAIMABLE")
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

pub fn claim_one(acc: &Account, cid: &str) -> (u16, Value) {
    let ed = match edition(&acc.edition) {
        Some(e) => e,
        None => return (0, json!({"error": "未知版本"})),
    };
    api_call(
        ed,
        &format!("/sash/api/v1/me/campaigns/{}/claim", cid),
        "POST",
        &acc.token,
        Some(json!({})),
    )
}

pub fn claim_account(acc: &mut Account, dry_run: bool) -> Value {
    let mut r = json!({
        "name": if acc.name.is_empty() { &acc.uid[..acc.uid.len().min(8)] } else { &acc.name },
        "edition": acc.edition,
        "uid": acc.uid,
        "phase": "error",
        "msg": "",
        "credits": 0.0,
        "claimed": [],
    });

    if let Err(e) = ensure_token(acc) {
        r["phase"] = json!("login_required");
        r["msg"] = json!(e);
        return r;
    }

    let (mut status, mut data) = get_campaigns(acc);
    if status == 401 && refresh_account_token(acc) {
        persist_and_writeback(acc);
        let (s2, d2) = get_campaigns(acc);
        status = s2;
        data = d2;
    }
    if status == 401 {
        r["phase"] = json!("login_required");
        r["msg"] = json!("token 无效且刷新失败，请重新登录客户端后采集");
        return r;
    }
    if status != 200 {
        r["msg"] = json!(format!(
            "查询活动失败 HTTP {}: {}",
            status,
            data.to_string().chars().take(150).collect::<String>()
        ));
        return r;
    }

    let todo = claimable_campaigns(&data);
    if todo.is_empty() {
        r["phase"] = json!("nothing");
        r["msg"] = json!("今日无可领积分（已领过或未刷新）");
        return r;
    }

    if dry_run {
        r["phase"] = json!("dry_run");
        r["claimed"] = json!(todo
            .iter()
            .map(|c| json!({
                "campaignId": c.get("campaignId"),
                "amount": c.pointer("/benefit/amount"),
            }))
            .collect::<Vec<_>>());
        r["msg"] = json!(format!("干跑：可领 {} 项", todo.len()));
        return r;
    }

    let mut got = 0.0f64;
    let mut fail = 0;
    let mut claimed = vec![];
    for c in &todo {
        let cid = c.get("campaignId").and_then(|x| x.as_str()).unwrap_or("");
        let (s2, d2) = claim_one(acc, cid);
        let dd = if d2.get("data").is_some() {
            d2.get("data").cloned().unwrap_or(json!({}))
        } else {
            d2.clone()
        };
        let st = dd.get("status").and_then(|x| x.as_str()).unwrap_or("");
        let ok = s2 == 200 && (st == "CLAIMED" || st == "REPLAYED");
        if ok {
            let amt = dd
                .pointer("/benefit/amount")
                .and_then(|x| x.as_f64())
                .or(c.pointer("/benefit/amount").and_then(|x| x.as_f64()))
                .unwrap_or(0.0);
            got += amt;
            claimed.push(json!({
                "campaignId": cid,
                "amount": amt,
                "replayed": st == "REPLAYED",
            }));
        } else {
            fail += 1;
        }
    }
    r["claimed"] = json!(claimed);
    if fail > 0 && got == 0.0 {
        r["phase"] = json!("error");
        r["msg"] = json!(format!("领取失败（{} 项）", fail));
    } else {
        r["phase"] = json!(if got > 0.0 { "claimed" } else { "nothing" });
        r["credits"] = json!(got);
        r["msg"] = json!(if got > 0.0 {
            format!("领取成功 +{} Credits", got)
        } else {
            "服务端返回已领过".to_string()
        });
    }
    r
}

pub fn claim_all(edition_filter: Option<&str>, dry_run: bool) -> Vec<Value> {
    let mut accounts = load_accounts();
    let mut results = vec![];
    let mut changed = false;
    for acc in accounts.iter_mut() {
        if let Some(f) = edition_filter {
            if acc.edition != f {
                continue;
            }
        }
        let before = (acc.token.clone(), acc.refreshToken.clone());
        let res = claim_account(acc, dry_run);
        results.push(res);
        if (acc.token.clone(), acc.refreshToken.clone()) != before {
            changed = true;
        }
    }
    if changed {
        let _ = save_accounts(&accounts);
    }
    if !dry_run {
        append_claim_log(&results);
    }
    results
}

pub fn append_claim_log(results: &[Value]) {
    let _ = fs::create_dir_all(switch_dir());
    let mut log: Vec<Value> = fs::read_to_string(claim_log_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let slim: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "name": r.get("name"), "edition": r.get("edition"),
                "phase": r.get("phase"), "msg": r.get("msg"),
                "credits": r.get("credits"), "claimed": r.get("claimed"),
            })
        })
        .collect();
    log.push(json!({ "time": now, "results": slim }));
    if log.len() > 500 {
        log = log.split_off(log.len() - 500);
    }
    let tmp = claim_log_file().with_extension("tmp");
    if let Ok(s) = serde_json::to_string_pretty(&log) {
        if fs::write(&tmp, s).is_ok() {
            let _ = fs::rename(&tmp, claim_log_file());
        }
    }
}

pub fn load_claim_log() -> Vec<Value> {
    fs::read_to_string(claim_log_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

pub fn write_claim_log(log: &Value) {
    let _ = fs::create_dir_all(switch_dir());
    let tmp = claim_log_file().with_extension("tmp");
    if let Ok(s) = serde_json::to_string_pretty(log) {
        if fs::write(&tmp, s).is_ok() {
            let _ = fs::rename(&tmp, claim_log_file());
        }
    }
}

pub fn credits_summary(acc: &Account) -> (u16, Value) {
    let ed = match edition(&acc.edition) {
        Some(e) => e,
        None => return (0, json!({"error": "未知版本"})),
    };
    api_call(
        ed,
        "/sash/api/v1/ai-conversations/credits-summary",
        "GET",
        &acc.token,
        None,
    )
}

// ───────────────────────── 应用控制 / 切换 ─────────────────────────

#[cfg(not(windows))]
pub fn app_running(edition_key: &str) -> bool {
    let ed = match edition(edition_key) {
        Some(e) => e,
        None => return false,
    };
    // 锚定主程序进程：路径 + 应用名结尾。避免把 native-messaging-host 等
    // 复用主程序二进制的子进程误判为"应用仍在运行"。
    Command::new("pgrep")
        .args([
            "-f",
            &format!("{}/Contents/MacOS/{}$", ed.app_path, ed.app_name),
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(not(windows))]
pub fn quit_app(edition_key: &str) -> bool {
    let t0 = std::time::Instant::now();
    let ed = match edition(edition_key) {
        Some(e) => e,
        None => return false,
    };
    let osa = run_cmd_timeout(
        "osascript",
        &[
            "-e",
            &format!("tell application \"{}\" to quit", ed.app_name),
        ],
        8000,
    );
    eprintln!(
        "[quit_app +{:?}] osascript 完成（{:?}）",
        t0.elapsed(),
        osa.is_some()
    );
    for _ in 0..20 {
        if !app_running(edition_key) {
            eprintln!("[quit_app +{:?}] 已退出", t0.elapsed());
            return true;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    eprintln!("[quit_app +{:?}] 优雅退出超时，开始 pkill", t0.elapsed());
    for round in 0..6 {
        let force = round >= 3; // 后 3 轮用 SIGKILL 强杀
        let flag: &str = if force { "-9" } else { "-TERM" };
        let _ = run_cmd_timeout("pkill", &[flag, "-x", ed.app_name], 3000);
        let _ = run_cmd_timeout(
            "pkill",
            &[flag, "-f", &format!("{}/Contents/MacOS", ed.app_path)],
            3000,
        );
        std::thread::sleep(Duration::from_secs(1));
        if !app_running(edition_key) {
            eprintln!("[quit_app +{:?}] pkill({}) 后已退出", t0.elapsed(), flag);
            return true;
        }
    }
    eprintln!("[quit_app +{:?}] 仍未退出，返回 false", t0.elapsed());
    !app_running(edition_key)
}

#[cfg(windows)]
pub fn app_running(key: &str) -> bool {
    edition(key).is_some_and(crate::platform::app_running)
}
#[cfg(windows)]
pub fn quit_app(key: &str) -> bool {
    edition(key).is_some_and(crate::platform::quit_app)
}

pub fn launch_app(edition_key: &str) -> Result<(), String> {
    let ed = edition(edition_key).ok_or("未知版本")?;
    let _exe = crate::platform::app_executable(ed)
        .ok_or("未找到 Qoder，请安装对应版本或设置 QODER_PATH / QODER_CN_PATH")?;
    {
        #[cfg(not(windows))]
        run_cmd_timeout("open", &["-a", ed.app_path], 8000)
            .filter(|o| o.status.success())
            .ok_or("无法启动 Qoder")?;
        #[cfg(windows)]
        Command::new(_exe).spawn().map_err(|_| "无法启动 Qoder")?;
    }
    Ok(())
}

pub fn switch_account(account_id: &str, steps: &mut Vec<String>) -> Result<String, String> {
    let accounts = load_accounts();
    let target = accounts
        .iter()
        .find(|a| a.id() == account_id)
        .cloned()
        .ok_or_else(|| format!("账号不存在：{}", account_id))?;
    let ed = edition(&target.edition).ok_or("未知版本")?;

    let cur_uid = live_uid(&target.edition);
    if cur_uid == target.uid {
        return Ok(format!("当前已是该账号（{}）", target.name));
    }

    steps.push("正在保存当前账号登录态…".into());
    if !cur_uid.is_empty() {
        let _ = capture_current(&target.edition);
    }

    if app_running(&target.edition) {
        steps.push(format!("正在退出 {}…", ed.label));
        if !quit_app(&target.edition) {
            return Err(format!("无法退出 {}，请手动关闭后重试", ed.label));
        }
    }

    steps.push("正在备份当前登录文件…".into());
    let dat = auth_dat_path(ed);
    let backup_dir = switch_dir().join("backups");
    let _ = fs::create_dir_all(&backup_dir);
    if dat.exists() {
        let name = format!(
            "{}-auth.v1-{}.dat",
            target.edition,
            chrono::Local::now().format("%Y%m%d-%H%M%S")
        );
        let _ = fs::copy(&dat, backup_dir.join(name));
    }

    steps.push(format!("正在写入目标账号（{}）登录态…", target.name));
    // Prefer rebuilding OS/user-bound ciphertext from the token, including imported accounts.
    let blob = if !target.token.is_empty() {
        let b64 = build_auth_blob(ed, &target)?;
        let mut accounts = load_accounts();
        if let Some(a) = accounts.iter_mut().find(|a| a.id() == account_id) {
            a.authBlob = b64.clone();
        }
        let _ = save_accounts(&accounts);
        steps.push("已使用本机密钥重新构造登录态".into());
        B64.decode(b64).map_err(|_| "登录态构造失败".to_string())?
    } else {
        B64.decode(&target.authBlob)
            .map_err(|_| "账号登录态数据损坏，请重新采集该账号".to_string())?
    };
    write_auth_blob(ed, &blob)?;

    steps.push(format!("正在启动 {}…", ed.label));
    launch_app(&target.edition)?;
    Ok(format!("已切换到 {}（{}）", target.name, ed.label))
}

// ───────────────────────── 设置 / 导入导出 ─────────────────────────

pub fn load_settings() -> Value {
    fs::read_to_string(settings_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or(json!({ "autoClaim": true }))
}

pub fn save_settings(s: &Value) {
    let _ = fs::create_dir_all(switch_dir());
    let tmp = settings_file().with_extension("tmp");
    if let Ok(text) = serde_json::to_string(s) {
        if fs::write(&tmp, text).is_ok() {
            let _ = fs::rename(&tmp, settings_file());
        }
    }
}

pub fn export_accounts(path: &str) -> Result<usize, String> {
    let accounts = load_accounts();
    let n = accounts.len();
    let doc = json!({
        "format": "qoder-switch-accounts",
        "version": 1,
        "exportedAt": chrono::Utc::now().to_rfc3339(),
        "accounts": accounts,
    });
    fs::write(
        path,
        serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(n)
}

pub fn import_accounts(path: &str) -> Result<usize, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&text).map_err(|_| "文件格式不正确".to_string())?;
    let new: Vec<Account> = if v.is_array() {
        serde_json::from_value(v).map_err(|e| e.to_string())?
    } else {
        serde_json::from_value(v.get("accounts").cloned().unwrap_or(json!([])))
            .map_err(|e| e.to_string())?
    };
    let mut accounts = load_accounts();
    let mut n = 0;
    for a in new {
        if a.uid.is_empty() || a.edition.is_empty() {
            continue;
        }
        if let Some(exist) = find_account(&mut accounts, &a.edition, &a.uid) {
            *exist = a;
        } else {
            accounts.push(a);
        }
        n += 1;
    }
    save_accounts(&accounts)?;
    Ok(n)
}

// ───────────────────────── OAuth 设备登录（添加账号） ─────────────────────────
//
// 复刻 Qoder 客户端官方登录链路（从 app.asar 还原）：
//   1) verifier = 64 字符随机串（字母表 A-Za-z0-9-._~）
//   2) challenge = base64url(sha256(verifier))，nonce = uuid v4
//   3) 登录页 {auth_base}/users/sign-in?biz_variant=qoder&oauth_callback=
//      {auth_base}/device/selectAccounts?challenge=..&challenge_method=S256&nonce=..&machine_id=..&client_id=..
//   4) 轮询 GET {api_base}/api/v1/deviceToken/poll?nonce=..&verifier=..&challenge_method=S256
//      404 = 未完成；200 = {token, refresh_token, expires_at?}
//   5) GET {api_base}/api/v1/userinfo 拉用户信息

fn b64url_no_pad(data: &[u8]) -> String {
    use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64U;
    B64U.encode(data)
}

fn urandom_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0; n];
    getrandom::fill(&mut buf).expect("OS random source unavailable");
    buf
}

fn gen_verifier() -> String {
    const ALPHA: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
    urandom_bytes(64)
        .iter()
        .map(|b| ALPHA[(*b % 64) as usize] as char)
        .collect()
}

fn gen_nonce() -> String {
    let mut b = [0u8; 16];
    b.copy_from_slice(&urandom_bytes(16));
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{:02x}", x)).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

fn machine_id(ed: &Edition) -> String {
    fs::read_to_string(crate::platform::data_dir(ed).join("auth.machine-id"))
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            hostname::get()
                .map(|h| h.to_string_lossy().to_string())
                .unwrap_or_else(|_| "unknown-machine".into())
        })
}

fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

#[derive(Serialize, Deserialize, Clone)]
pub struct LoginAttempt {
    pub edition: String,
    pub nonce: String,
    pub verifier: String,
}

/// 发起登录：返回 (登录链接, LoginAttempt)。前端用系统浏览器打开链接后轮询。
pub fn login_begin(edition_key: &str) -> Result<(LoginAttempt, String), String> {
    let ed = edition(edition_key).ok_or("未知版本")?;
    let verifier = gen_verifier();
    use sha2::{Digest, Sha256};
    let challenge = b64url_no_pad(&Sha256::digest(verifier.as_bytes()));
    let nonce = gen_nonce();
    let mid = machine_id(ed);
    let device_url = format!(
        "{}/device/selectAccounts?challenge={}&challenge_method=S256&nonce={}&machine_id={}&client_id={}",
        ed.auth_base, challenge, nonce, mid, ed.client_id
    );
    let login_url = format!(
        "{}/users/sign-in?biz_variant={}&oauth_callback={}",
        ed.auth_base,
        ed.biz_variant,
        urlencode(&device_url)
    );
    Ok((
        LoginAttempt {
            edition: edition_key.to_string(),
            nonce,
            verifier,
        },
        login_url,
    ))
}

/// 轮询一次：Ok(None) = 继续等待；Ok(Some(session)) = 登录成功
pub fn login_poll(attempt: &LoginAttempt) -> Result<Option<Value>, String> {
    let Some(ed) = edition(&attempt.edition) else {
        return Err("未知版本".into());
    };
    let path = format!(
        "/api/v1/deviceToken/poll?nonce={}&verifier={}&challenge_method=S256",
        attempt.nonce, attempt.verifier
    );
    let (status, data) = api_call(ed, &path, "GET", "", None);
    if status == 404 || status == 0 {
        return Ok(None); // 尚未授权 / 网络抖动，继续等
    }
    if status != 200 {
        let msg = data
            .get("errorMessage")
            .and_then(|x| x.as_str())
            .unwrap_or("登录失败");
        return Err(format!("{} (HTTP {})", msg, status));
    }
    let token = data
        .get("token")
        .and_then(|x| x.as_str())
        .or_else(|| data.get("device_token").and_then(|x| x.as_str()))
        .unwrap_or("");
    let refresh = data
        .get("refresh_token")
        .and_then(|x| x.as_str())
        .or_else(|| data.get("refreshToken").and_then(|x| x.as_str()))
        .unwrap_or("");
    if token.is_empty() || refresh.is_empty() {
        return Ok(None);
    }
    // 用户信息（容错：字段名多形态兼容）
    let (_s, raw_user) = api_call(ed, "/api/v1/userinfo", "GET", token, None);
    let pick = |o: &Value, keys: &[&str]| -> String {
        for k in keys {
            if let Some(v) = o.get(*k).and_then(|x| x.as_str()) {
                if !v.is_empty() {
                    return v.to_string();
                }
            }
        }
        String::new()
    };
    let uid = pick(&raw_user, &["id", "user_id", "uid"]);
    let user = json!({
        "id": if uid.is_empty() { gen_nonce() } else { uid },
        "name": pick(&raw_user, &["name", "username", "user_name", "email", "security_mobile"]),
        "email": pick(&raw_user, &["email"]),
        "phone": pick(&raw_user, &["security_mobile", "phone"]),
        "avatarUrl": pick(&raw_user, &["avatar_url", "avatar", "image_url"]),
    });
    let expires_at = data
        .get("expires_at")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            let secs = data
                .get("expires_in")
                .and_then(|x| x.as_i64())
                .unwrap_or(3600);
            (chrono::Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339()
        });
    let rt_expires = data
        .get("refresh_token_expires_at")
        .and_then(|x| x.as_str())
        .map(|s| s.to_string())
        .unwrap_or_default();
    Ok(Some(json!({
        "token": token,
        "refreshToken": refresh,
        "expiresAt": expires_at,
        "refreshTokenExpiresAt": rt_expires,
        "user": user,
    })))
}

/// 将 OAuth 登录结果保存进账号库 → (account, is_new)
pub fn save_login_account(edition_key: &str, sess: &Value) -> Result<(Account, bool), String> {
    let _ed = edition(edition_key).ok_or("未知版本")?;
    let token = sess.get("token").and_then(|x| x.as_str()).unwrap_or("");
    if token.is_empty() {
        return Err("登录结果缺少 token".into());
    }
    let user = sess.get("user").cloned().unwrap_or(json!({}));
    let uid = user
        .get("id")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    if uid.is_empty() {
        return Err("登录结果缺少用户 ID".into());
    }
    let mut accounts = load_accounts();
    let idx = accounts
        .iter()
        .position(|a| a.edition == edition_key && a.uid == uid);
    let is_new = idx.is_none();
    if is_new {
        accounts.push(Account {
            edition: edition_key.to_string(),
            uid: uid.clone(),
            ..Default::default()
        });
    }
    let acc = {
        let i = idx.unwrap_or(accounts.len() - 1);
        &mut accounts[i]
    };
    acc.name = user
        .get("name")
        .and_then(|x| x.as_str())
        .filter(|s| !s.is_empty())
        .or(user.get("email").and_then(|x| x.as_str()))
        .or(user.get("phone").and_then(|x| x.as_str()))
        .unwrap_or(&uid[..uid.len().min(8)])
        .to_string();
    acc.email = user
        .get("email")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    acc.phone = user
        .get("phone")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    acc.avatarUrl = user
        .get("avatarUrl")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    acc.token = token.to_string();
    acc.refreshToken = sess
        .get("refreshToken")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    acc.expiresAt = sess
        .get("expiresAt")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    acc.refreshTokenExpiresAt = sess
        .get("refreshTokenExpiresAt")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    acc.savedAt = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    // OAuth 登录的账号客户端磁盘上没有它的 auth.v1.dat，必须现场构造登录态供切换使用
    acc.authBlob = build_auth_blob(_ed, acc)?;
    let saved = acc.clone();
    save_accounts(&accounts)?;
    // 若该账号正登录客户端，顺带回写登录态
    if live_uid(edition_key) == uid {
        let _ = persist_and_writeback(&saved);
    }
    Ok((saved, is_new))
}

/// 配额信息：GET /sash/api/v2/me/usage（套餐 + 加量包余量）
pub fn quota_info(acc: &Account) -> (u16, Value) {
    let Some(ed) = edition(&acc.edition) else {
        return (0, json!({}));
    };
    api_call(ed, "/sash/api/v2/me/usage", "GET", &acc.token, None)
}

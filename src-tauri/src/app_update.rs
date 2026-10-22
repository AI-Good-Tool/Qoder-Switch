use semver::Version;
use serde::{Deserialize, Serialize};
#[cfg(windows)]
use std::process::Command;
use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{AppHandle, Emitter, Manager, State};

#[cfg(target_os = "macos")]
mod macos;

const LATEST_RELEASE_API: &str =
    "https://api.github.com/repos/AI-Good-Tool/Qoder-Switch/releases/latest";
const GITHUB_USER_AGENT: &str = "Qoder-Switch-App-Updater";
const DOWNLOAD_PROGRESS_EVENT: &str = "qoder-switch-app-update-download-progress";
const MAX_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;
const CLEANUP_MARKER_FILE: &str = ".cleanup-after-install";

#[derive(Default)]
pub struct AppUpdateDownloadState(Mutex<DownloadControl>);

#[derive(Default)]
struct DownloadControl {
    active_id: Option<String>,
    cancel_requested: bool,
    installing: bool,
    completed: Option<(PathBuf, String)>,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
    #[serde(default)]
    body: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    digest: Option<String>,
}

#[derive(Debug, Clone, Copy)]
struct UpdateTarget {
    os: &'static str,
    arch: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateInfo {
    current_version: String,
    latest_version: String,
    release_url: String,
    target: String,
    asset_name: Option<String>,
    asset_size: Option<u64>,
    has_update: bool,
    can_download: bool,
    release_notes: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateDownloadResult {
    version: String,
    asset_name: String,
    path: String,
    size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppUpdateDownloadProgressEvent {
    status: String,
    version: String,
    asset_name: String,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    message: Option<String>,
}

#[tauri::command]
pub fn app_update_metadata(app: AppHandle) -> serde_json::Value {
    serde_json::json!({"version":app.package_info().version.to_string(),"target":target_label(current_target()),"autoCheckUpdates":crate::qcore::load_settings()["autoCheckUpdates"].as_bool().unwrap_or(true)})
}

#[tauri::command]
pub async fn app_update_check(app: AppHandle) -> Result<AppUpdateInfo, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (_, info) = latest_update(&app)?;
        Ok(info)
    })
    .await
    .map_err(|error| format!("应用更新检查任务失败: {error}"))?
}

#[tauri::command]
pub async fn app_update_download(app: AppHandle) -> Result<AppUpdateDownloadResult, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let download = app.state::<AppUpdateDownloadState>();
        app_update_download_impl(&app, &download)
    })
    .await
    .map_err(|error| format!("应用更新下载任务失败: {error}"))?
}

fn app_update_download_impl(
    app: &AppHandle,
    download: &State<'_, AppUpdateDownloadState>,
) -> Result<AppUpdateDownloadResult, String> {
    let download_id = begin_download(download)?;
    let result = (|| {
        emit_progress(app, "checking", "", "", 0, None, Some("正在获取最新版本"));
        let (release, info) = latest_update(app)?;
        if !info.has_update {
            return Err(format!("当前已经是最新版本 v{}", info.current_version));
        }
        let target = current_target();
        let asset = select_release_asset(&release.assets, target)
            .ok_or_else(|| format!("没有适用于 {} 的在线安装包", target_label(target)))?;
        download_release_asset(app, download, &download_id, &info.latest_version, asset)
    })();

    if let Err(error) = &result {
        let status = if error == "下载已取消" {
            "cancelled"
        } else {
            "failed"
        };
        emit_progress(app, status, "", "", 0, None, Some(error));
    }
    clear_download(download, &download_id);
    result
}

#[tauri::command]
pub fn app_update_cancel_download(
    download: State<'_, AppUpdateDownloadState>,
) -> Result<(), String> {
    let mut guard = download
        .0
        .lock()
        .map_err(|_| "应用更新下载状态锁已损坏".to_string())?;
    if guard.active_id.is_some() {
        guard.cancel_requested = true;
    }
    Ok(())
}

#[cfg(windows)]
fn app_update_open_installer(app: AppHandle, path: String) -> Result<(), String> {
    let download = app.state::<AppUpdateDownloadState>();
    let update_guard = download
        .0
        .lock()
        .map_err(|_| "应用更新下载状态锁已损坏".to_string())?;
    if update_guard.active_id.is_some() || update_guard.installing {
        return Err("应用更新仍在下载，请等待下载完成后再安装".to_string());
    }

    let canonical_installer = verified_installer(&path, update_guard.completed.as_ref())?;
    open_installer_with_system(&canonical_installer)?;
    mark_updates_cleanup_pending(&updates_dir()?)?;
    drop(update_guard);
    Ok(())
}

fn validated_installer_path(path: &str, allowed_root: &Path) -> Result<PathBuf, String> {
    let installer = PathBuf::from(path);
    if !installer.is_file() {
        return Err("更新安装包不存在，请重新下载".to_string());
    }
    let canonical_root = allowed_root
        .canonicalize()
        .map_err(|error| format!("读取更新目录失败: {error}"))?;
    let canonical_installer = installer
        .canonicalize()
        .map_err(|error| format!("读取更新安装包失败: {error}"))?;
    if !canonical_installer.starts_with(&canonical_root) {
        return Err("只能打开 Qoder Switch 更新目录中的安装包".to_string());
    }

    Ok(canonical_installer)
}

#[tauri::command]
pub async fn app_update_install_and_restart(app: AppHandle, path: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        {
            let state = app.state::<AppUpdateDownloadState>();
            let mut guard = state
                .0
                .lock()
                .map_err(|_| "应用更新状态锁已损坏".to_string())?;
            if guard.active_id.is_some() || guard.installing {
                return Err("已有应用更新正在进行".to_string());
            }
            guard.installing = true;
        }
        let worker_app = app.clone();
        let result = tauri::async_runtime::spawn_blocking(move || {
            let root = updates_dir()?;
            let expected = worker_app
                .state::<AppUpdateDownloadState>()
                .0
                .lock()
                .map_err(|_| "更新状态不可用")?
                .completed
                .clone();
            let installer = verified_installer(&path, expected.as_ref())?;
            macos::prepare_install_and_restart(&worker_app, &installer, &root)
        })
        .await
        .map_err(|error| format!("应用更新安装任务失败: {error}"))
        .and_then(|result| result);
        if result.is_ok() {
            // The detached helper waits for this process to exit before touching the bundle.
            app.state::<std::sync::Arc<crate::qproxy::Gateway>>().stop();
            app.exit(0);
        } else if let Ok(mut guard) = app.state::<AppUpdateDownloadState>().0.lock() {
            guard.installing = false;
        }
        result
    }
    #[cfg(target_os = "windows")]
    {
        app_update_open_installer(app.clone(), path)?;
        app.state::<std::sync::Arc<crate::qproxy::Gateway>>().stop();
        app.exit(0);
        Ok(())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let _ = (app, path);
        Err("当前平台暂不支持应用内安装".to_string())
    }
}

fn install_error_path() -> Result<PathBuf, String> {
    Ok(updates_dir()?.with_file_name(".last-app-update-error"))
}

#[tauri::command]
pub fn app_update_take_install_error() -> Result<Option<String>, String> {
    let path = install_error_path()?;
    match fs::read_to_string(&path) {
        Ok(message) => {
            fs::remove_file(path).map_err(|error| format!("清理更新错误提示失败: {error}"))?;
            Ok(Some(message))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("读取更新错误提示失败: {error}")),
    }
}

pub fn confirm_update_restart(app: &AppHandle) {
    #[cfg(target_os = "macos")]
    if let Err(error) = macos::confirm_update_restart(app) {
        eprintln!("App update restart confirmation failed: {error}");
    }
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

pub fn cleanup_pending_update_artifacts_on_startup(app: &AppHandle) -> Result<(), String> {
    let root = updates_dir()?;
    if !root.join(CLEANUP_MARKER_FILE).exists() {
        return Ok(());
    }
    cleanup_updates_dir_contents_if_idle(app, &root)
}

fn cleanup_pending_update_artifacts_at(root: &Path) -> Result<(), String> {
    if !root.join(CLEANUP_MARKER_FILE).exists() {
        return Ok(());
    }
    cleanup_updates_dir_contents(root)
}

fn latest_update(app: &AppHandle) -> Result<(GitHubRelease, AppUpdateInfo), String> {
    let release = fetch_latest_release()?;
    let current = parse_version(&app.package_info().version.to_string())?;
    let info = build_update_info(&release, current, current_target())?;
    Ok((release, info))
}

fn build_update_info(
    release: &GitHubRelease,
    current: Version,
    target: UpdateTarget,
) -> Result<AppUpdateInfo, String> {
    let latest = parse_version(&release.tag_name)?;
    let asset = select_release_asset(&release.assets, target);
    let has_update = latest > current;
    Ok(AppUpdateInfo {
        current_version: current.to_string(),
        latest_version: latest.to_string(),
        release_url: if release.html_url.trim().is_empty() {
            "https://github.com/AI-Good-Tool/Qoder-Switch/releases".to_string()
        } else {
            release.html_url.clone()
        },
        target: target_label(target),
        asset_name: asset.map(|item| item.name.clone()),
        asset_size: asset.map(|item| item.size),
        has_update,
        can_download: has_update && asset.is_some_and(|a| asset_digest(a).is_ok()),
        release_notes: release.body.clone(),
    })
}

fn fetch_latest_release() -> Result<GitHubRelease, String> {
    reqwest::blocking::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(15))
        .user_agent(GITHUB_USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| format!("初始化更新客户端失败: {error}"))?
        .get(LATEST_RELEASE_API)
        .header("Accept", "application/vnd.github+json")
        .send()
        .map_err(|error| format!("获取最新版本失败: {error}"))?
        .error_for_status()
        .map_err(|error| format!("获取最新版本失败: {error}"))?
        .json::<GitHubRelease>()
        .map_err(|error| format!("解析最新版本失败: {error}"))
}

fn download_release_asset(
    app: &AppHandle,
    download: &State<'_, AppUpdateDownloadState>,
    download_id: &str,
    version: &str,
    asset: &GitHubAsset,
) -> Result<AppUpdateDownloadResult, String> {
    if asset.size > MAX_INSTALLER_BYTES {
        return Err(format!(
            "更新安装包超过安全大小限制（最大 {} MiB）",
            MAX_INSTALLER_BYTES / 1024 / 1024
        ));
    }
    let expected_digest = asset_digest(asset)?;
    let file_name = safe_file_name(&asset.name)?;
    let version_dir = updates_dir()?.join(version);
    fs::create_dir_all(&version_dir).map_err(|error| format!("创建更新目录失败: {error}"))?;
    let installer_path = version_dir.join(&file_name);
    let temp_path = version_dir.join(format!("{file_name}.download"));
    let _ = fs::remove_file(&temp_path);

    emit_progress(
        app,
        "starting",
        version,
        &asset.name,
        0,
        size_option(asset.size),
        Some("正在连接下载服务器"),
    );
    let download_url = validate_release_download_url(&asset.browser_download_url)?;
    let mut response = reqwest::blocking::Client::builder()
        .https_only(true)
        .connect_timeout(Duration::from_secs(15))
        .user_agent(GITHUB_USER_AGENT)
        .timeout(Duration::from_secs(180))
        .build()
        .map_err(|error| format!("初始化下载客户端失败: {error}"))?
        .get(download_url)
        .header("Accept", "application/octet-stream")
        .send()
        .map_err(|error| format!("下载安装包失败: {error}"))?
        .error_for_status()
        .map_err(|error| format!("下载安装包失败: {error}"))?;

    let total_bytes = response
        .content_length()
        .or_else(|| size_option(asset.size));
    if total_bytes.is_some_and(|size| size > MAX_INSTALLER_BYTES) {
        return Err(format!(
            "更新安装包超过安全大小限制（最大 {} MiB）",
            MAX_INSTALLER_BYTES / 1024 / 1024
        ));
    }
    emit_progress(
        app,
        "downloading",
        version,
        &asset.name,
        0,
        total_bytes,
        None,
    );

    let download_result = (|| {
        let mut file =
            File::create(&temp_path).map_err(|error| format!("创建下载临时文件失败: {error}"))?;
        use sha2::{Digest, Sha256};
        let mut hash = Sha256::new();
        let mut downloaded_bytes = 0_u64;
        let mut last_emitted_bytes = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            if is_download_cancelled(download, download_id)? {
                return Err("下载已取消".to_string());
            }
            let read = response
                .read(&mut buffer)
                .map_err(|error| format!("读取更新安装包失败: {error}"))?;
            if read == 0 {
                break;
            }
            downloaded_bytes = downloaded_bytes.saturating_add(read as u64);
            if downloaded_bytes > MAX_INSTALLER_BYTES {
                return Err(format!(
                    "更新安装包超过安全大小限制（最大 {} MiB）",
                    MAX_INSTALLER_BYTES / 1024 / 1024
                ));
            }
            hash.update(&buffer[..read]);
            file.write_all(&buffer[..read])
                .map_err(|error| format!("写入更新安装包失败: {error}"))?;
            if downloaded_bytes == total_bytes.unwrap_or(0)
                || downloaded_bytes.saturating_sub(last_emitted_bytes) >= 256 * 1024
            {
                emit_progress(
                    app,
                    "downloading",
                    version,
                    &asset.name,
                    downloaded_bytes,
                    total_bytes,
                    None,
                );
                last_emitted_bytes = downloaded_bytes;
            }
        }
        file.sync_all()
            .map_err(|error| format!("保存更新安装包失败: {error}"))?;
        if asset.size > 0 && downloaded_bytes != asset.size {
            return Err(format!(
                "更新安装包大小不完整，预期 {} 字节，实际 {} 字节",
                asset.size, downloaded_bytes
            ));
        }
        let digest = format!("{:x}", hash.finalize());
        if digest != expected_digest {
            return Err("更新安装包 SHA-256 校验失败，请重新下载".into());
        }
        if installer_path.exists() {
            fs::remove_file(&installer_path)
                .map_err(|error| format!("替换旧安装包失败: {error}"))?;
        }
        fs::rename(&temp_path, &installer_path)
            .map_err(|error| format!("保存更新安装包失败: {error}"))?;
        download.0.lock().map_err(|_| "更新状态不可用")?.completed = Some((
            installer_path.canonicalize().map_err(|e| e.to_string())?,
            digest,
        ));
        emit_progress(
            app,
            "completed",
            version,
            &asset.name,
            downloaded_bytes,
            Some(downloaded_bytes),
            Some("更新安装包下载完成"),
        );
        Ok(AppUpdateDownloadResult {
            version: version.to_string(),
            asset_name: asset.name.clone(),
            path: installer_path.to_string_lossy().into_owned(),
            size_bytes: downloaded_bytes,
        })
    })();

    if download_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    download_result
}

fn asset_digest(asset: &GitHubAsset) -> Result<String, String> {
    let digest = asset
        .digest
        .as_deref()
        .and_then(|s| s.strip_prefix("sha256:"))
        .filter(|s| s.len() == 64 && s.bytes().all(|c| c.is_ascii_hexdigit()))
        .ok_or("此发布包缺少 SHA-256 校验信息，请从发布页下载")?;
    Ok(digest.to_ascii_lowercase())
}

fn verified_installer(path: &str, expected: Option<&(PathBuf, String)>) -> Result<PathBuf, String> {
    verify_installer_at(path, &updates_dir()?, expected)
}

fn verify_installer_at(
    path: &str,
    root: &Path,
    expected: Option<&(PathBuf, String)>,
) -> Result<PathBuf, String> {
    use sha2::{Digest, Sha256};
    let installer = validated_installer_path(path, root)?;
    let (known, digest) = expected.ok_or("请先在应用内下载并校验更新")?;
    if &installer != known {
        return Err("安装包与本次下载不一致".into());
    }
    let mut file = File::open(&installer).map_err(|e| e.to_string())?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    if format!("{:x}", hash.finalize()) != *digest {
        return Err("安装包已改变，请重新下载".into());
    }
    Ok(installer)
}

fn parse_version(value: &str) -> Result<Version, String> {
    Version::parse(value.trim().trim_start_matches(['v', 'V']))
        .map_err(|error| format!("版本号无效 {value}: {error}"))
}

fn current_target() -> UpdateTarget {
    UpdateTarget {
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
    }
}

fn target_label(target: UpdateTarget) -> String {
    format!("{}-{}", target.os, target.arch)
}

fn select_release_asset(assets: &[GitHubAsset], target: UpdateTarget) -> Option<&GitHubAsset> {
    assets
        .iter()
        .filter_map(|asset| asset_score(&asset.name, target).map(|score| (score, asset)))
        .max_by_key(|(score, _)| *score)
        .map(|(_, asset)| asset)
}

fn asset_score(name: &str, target: UpdateTarget) -> Option<i32> {
    let lower = name.to_ascii_lowercase();
    let mut score = match target.os {
        "macos" if lower.ends_with(".dmg") => 40,
        "windows" if lower.ends_with(".exe") => 40,
        "windows" if lower.ends_with(".msi") => 30,
        _ => return None,
    };

    if lower.contains("universal") {
        score += 80;
        return Some(score);
    }

    let aliases = arch_aliases(target.arch);
    if aliases.iter().any(|alias| lower.contains(alias)) {
        score += 100;
    } else if contains_known_architecture(&lower) {
        return None;
    } else {
        return None;
    }
    Some(score)
}

fn arch_aliases(arch: &str) -> &'static [&'static str] {
    match arch {
        "aarch64" => &["aarch64", "arm64"],
        "x86_64" => &["x86_64", "x64", "amd64"],
        "x86" => &["i686", "x86"],
        _ => &[],
    }
}

fn contains_known_architecture(name: &str) -> bool {
    ["aarch64", "arm64", "x86_64", "x64", "amd64", "i686"]
        .iter()
        .any(|alias| name.contains(alias))
}

fn updates_dir() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".qoder-switch").join("updates"))
        .ok_or_else(|| "无法确定用户主目录".to_string())
}

#[cfg(windows)]
fn mark_updates_cleanup_pending(root: &Path) -> Result<(), String> {
    fs::create_dir_all(root).map_err(|error| format!("创建更新清理标记目录失败: {error}"))?;
    fs::write(root.join(CLEANUP_MARKER_FILE), b"pending")
        .map_err(|error| format!("写入更新清理标记失败: {error}"))
}

fn cleanup_updates_dir_contents_if_idle(app: &AppHandle, root: &Path) -> Result<(), String> {
    let download = app.state::<AppUpdateDownloadState>();
    let guard = download
        .0
        .lock()
        .map_err(|_| "应用更新下载状态锁已损坏".to_string())?;
    if guard.active_id.is_some() || guard.installing {
        return Ok(());
    }
    cleanup_pending_update_artifacts_at(root)
}

fn cleanup_updates_dir_contents(root: &Path) -> Result<(), String> {
    if !root.exists() {
        return Ok(());
    }
    if !root.is_dir() {
        return Err("更新目录不是有效文件夹".to_string());
    }

    let marker = root.join(CLEANUP_MARKER_FILE);
    let mut errors = Vec::new();
    let entries = fs::read_dir(root).map_err(|error| format!("读取更新目录失败: {error}"))?;
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!("读取更新目录项失败: {error}"));
                continue;
            }
        };
        let path = entry.path();
        if path == marker {
            continue;
        }
        let remove_result = match entry.file_type() {
            Ok(file_type) if file_type.is_dir() => fs::remove_dir_all(&path),
            Ok(_) => fs::remove_file(&path),
            Err(error) => {
                errors.push(format!("读取更新文件类型失败: {error}"));
                continue;
            }
        };
        if let Err(error) = remove_result {
            errors.push(format!("删除更新内容失败 {}: {error}", path.display()));
        }
    }

    if errors.is_empty() && marker.exists() {
        if let Err(error) = fs::remove_file(&marker) {
            errors.push(format!("删除更新清理标记失败: {error}"));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn safe_file_name(name: &str) -> Result<String, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > 240 || trimmed.contains(['/', '\\', ':', '\0']) {
        return Err("更新安装包文件名无效".to_string());
    }
    let path = Path::new(trimmed);
    if path.file_name().and_then(|value| value.to_str()) != Some(trimmed) {
        return Err("更新安装包文件名包含非法路径".to_string());
    }
    Ok(trimmed.to_string())
}

fn validate_release_download_url(raw: &str) -> Result<reqwest::Url, String> {
    let url =
        reqwest::Url::parse(raw).map_err(|error| format!("更新安装包下载地址无效: {error}"))?;
    let allowed_path = "/AI-Good-Tool/Qoder-Switch/releases/download/";
    if url.scheme() != "https"
        || url.host_str() != Some("github.com")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !url.path().starts_with(allowed_path)
    {
        return Err("更新安装包下载地址不是 Qoder Switch 官方 Release".to_string());
    }
    Ok(url)
}

fn size_option(size: u64) -> Option<u64> {
    (size > 0).then_some(size)
}

fn begin_download(download: &State<'_, AppUpdateDownloadState>) -> Result<String, String> {
    let mut guard = download
        .0
        .lock()
        .map_err(|_| "应用更新下载状态锁已损坏".to_string())?;
    if guard.active_id.is_some() || guard.installing {
        return Err("已有应用更新正在进行".to_string());
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("读取系统时间失败: {error}"))?
        .as_millis();
    let id = format!("{}-{timestamp}", std::process::id());
    guard.active_id = Some(id.clone());
    guard.cancel_requested = false;
    Ok(id)
}

fn clear_download(download: &State<'_, AppUpdateDownloadState>, download_id: &str) {
    if let Ok(mut guard) = download.0.lock() {
        if guard.active_id.as_deref() == Some(download_id) {
            guard.active_id = None;
            guard.cancel_requested = false;
        }
    }
}

fn is_download_cancelled(
    download: &State<'_, AppUpdateDownloadState>,
    download_id: &str,
) -> Result<bool, String> {
    let guard = download
        .0
        .lock()
        .map_err(|_| "应用更新下载状态锁已损坏".to_string())?;
    Ok(guard.active_id.as_deref() == Some(download_id) && guard.cancel_requested)
}

fn emit_progress(
    app: &AppHandle,
    status: &str,
    version: &str,
    asset_name: &str,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    message: Option<&str>,
) {
    let _ = app.emit(
        DOWNLOAD_PROGRESS_EVENT,
        AppUpdateDownloadProgressEvent {
            status: status.to_string(),
            version: version.to_string(),
            asset_name: asset_name.to_string(),
            downloaded_bytes,
            total_bytes,
            message: message.map(ToString::to_string),
        },
    );
}

#[cfg(target_os = "windows")]
fn open_installer_with_system(path: &Path) -> Result<(), String> {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("exe") => Command::new(path)
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("打开更新安装包失败: {error}")),
        Some("msi") => Command::new("msiexec")
            .arg("/i")
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("打开更新安装包失败: {error}")),
        _ => Err("Windows 在线更新只允许打开 EXE 或 MSI 安装包".to_string()),
    }
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn open_installer_with_system(_path: &Path) -> Result<(), String> {
    Err("当前平台暂不支持在线安装".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn asset(name: &str) -> GitHubAsset {
        GitHubAsset {
            name: name.to_string(),
            browser_download_url: format!("https://example.invalid/{name}"),
            size: 10,
            digest: Some(format!("sha256:{}", "a".repeat(64))),
        }
    }

    fn release(version: &str, assets: Vec<GitHubAsset>) -> GitHubRelease {
        GitHubRelease {
            tag_name: version.to_string(),
            html_url: "https://github.com/AI-Good-Tool/Qoder-Switch/releases/latest".to_string(),
            assets,
            body: None,
        }
    }

    #[test]
    #[ignore = "reads public GitHub release metadata"]
    fn official_release_metadata_has_verified_platform_assets() {
        let release = fetch_latest_release().unwrap();
        parse_version(&release.tag_name).unwrap();
        for target in [
            UpdateTarget {
                os: "macos",
                arch: "aarch64",
            },
            UpdateTarget {
                os: "macos",
                arch: "x86_64",
            },
            UpdateTarget {
                os: "windows",
                arch: "x86_64",
            },
        ] {
            let asset = select_release_asset(&release.assets, target).unwrap();
            validate_release_download_url(&asset.browser_download_url).unwrap();
            asset_digest(asset).unwrap();
        }
    }

    #[test]
    fn digest_and_installer_revalidation_reject_missing_or_changed_bytes() {
        use sha2::{Digest, Sha256};
        let mut a = asset("Qoder.Switch_1.8.4_x64-setup.exe");
        a.digest = None;
        assert!(asset_digest(&a).is_err());
        a.digest = Some("sha256:invalid".into());
        assert!(asset_digest(&a).is_err());
        let dir = tempfile::tempdir().unwrap();
        let installer = dir.path().join("installer.exe");
        fs::write(&installer, b"synthetic installer").unwrap();
        let expected = (
            installer.canonicalize().unwrap(),
            format!("{:x}", Sha256::digest(b"synthetic installer")),
        );
        assert!(verify_installer_at(installer.to_str().unwrap(), dir.path(), None).is_err());
        assert!(
            verify_installer_at(installer.to_str().unwrap(), dir.path(), Some(&expected)).is_ok()
        );
        fs::write(&installer, b"tampered").unwrap();
        assert!(
            verify_installer_at(installer.to_str().unwrap(), dir.path(), Some(&expected)).is_err()
        );
    }

    #[test]
    fn asset_paths_and_unmarked_architectures_are_rejected() {
        for path in ["../bad.exe", r"C:\bad.exe", "folder/bad.exe", "bad:stream"] {
            assert!(safe_file_name(path).is_err());
        }
        assert!(select_release_asset(
            &[asset("Qoder.Switch.dmg")],
            UpdateTarget {
                os: "macos",
                arch: "aarch64"
            }
        )
        .is_none());
        assert!(validate_release_download_url(
            "https://github.com/other/repo/releases/download/v1/app.exe"
        )
        .is_err());
    }

    #[test]
    fn parses_prefixed_versions() {
        assert_eq!(parse_version("v0.1.12").unwrap(), Version::new(0, 1, 12));
        assert!(parse_version("latest").is_err());
        assert!(parse_version("v0.1.11").unwrap() <= parse_version("0.1.11").unwrap());
        assert!(parse_version("v0.1.12").unwrap() > parse_version("0.1.11").unwrap());
    }

    #[test]
    fn equal_release_version_is_not_an_update() {
        let info = build_update_info(
            &release("v0.1.11", vec![asset("Qoder.Switch_0.1.11_aarch64.dmg")]),
            Version::new(0, 1, 11),
            UpdateTarget {
                os: "macos",
                arch: "aarch64",
            },
        )
        .unwrap();
        assert!(!info.has_update);
        assert!(!info.can_download);
    }

    #[test]
    fn newer_release_exposes_a_matching_online_installer() {
        let info = build_update_info(
            &release("v0.1.12", vec![asset("Qoder.Switch_0.1.12_aarch64.dmg")]),
            Version::new(0, 1, 11),
            UpdateTarget {
                os: "macos",
                arch: "aarch64",
            },
        )
        .unwrap();
        assert!(info.has_update);
        assert!(info.can_download);
        assert_eq!(
            info.asset_name.as_deref(),
            Some("Qoder.Switch_0.1.12_aarch64.dmg")
        );
    }

    #[test]
    fn selects_matching_macos_architecture() {
        let assets = vec![
            asset("Qoder.Switch_0.1.12_x64.dmg"),
            asset("Qoder.Switch_0.1.12_aarch64.dmg"),
            asset("Qoder.Switch_0.1.12_aarch64.app.tar.gz"),
        ];
        let selected = select_release_asset(
            &assets,
            UpdateTarget {
                os: "macos",
                arch: "aarch64",
            },
        )
        .unwrap();
        assert_eq!(selected.name, "Qoder.Switch_0.1.12_aarch64.dmg");
    }

    #[test]
    fn prefers_windows_executable_installer() {
        let assets = vec![
            asset("Qoder.Switch_0.1.12_x64_en-US.msi"),
            asset("Qoder.Switch_0.1.12_x64-setup.exe"),
        ];
        let selected = select_release_asset(
            &assets,
            UpdateTarget {
                os: "windows",
                arch: "x86_64",
            },
        )
        .unwrap();
        assert!(selected.name.ends_with("setup.exe"));
    }

    #[test]
    fn rejects_packages_for_another_architecture() {
        let assets = vec![asset("Qoder.Switch_0.1.12_x64.dmg")];
        assert!(select_release_asset(
            &assets,
            UpdateTarget {
                os: "macos",
                arch: "aarch64",
            },
        )
        .is_none());
    }

    #[test]
    fn accepts_only_official_release_download_urls() {
        assert!(validate_release_download_url(
            "https://github.com/AI-Good-Tool/Qoder-Switch/releases/download/v0.1.12/app.dmg"
        )
        .is_ok());
        assert!(validate_release_download_url(
            "https://example.com/AI-Good-Tool/Qoder-Switch/releases/download/v0.1.12/app.dmg"
        )
        .is_err());
        assert!(validate_release_download_url(
            "http://github.com/AI-Good-Tool/Qoder-Switch/releases/download/v0.1.12/app.dmg"
        )
        .is_err());
    }

    #[test]
    fn cleanup_updates_dir_removes_downloaded_artifacts_and_marker() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join(CLEANUP_MARKER_FILE), b"pending").unwrap();
        fs::write(root.join("installer.dmg"), b"installer").unwrap();
        fs::create_dir(root.join("0.1.12")).unwrap();
        fs::write(root.join("0.1.12").join("installer.dmg"), b"old").unwrap();

        cleanup_updates_dir_contents(root).unwrap();

        assert!(fs::read_dir(root).unwrap().next().is_none());
    }

    #[test]
    fn startup_cleanup_skips_unmarked_downloads() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(root.join("installer.dmg"), b"installer").unwrap();

        cleanup_pending_update_artifacts_at(root).unwrap();

        assert!(root.join("installer.dmg").exists());
    }

    #[test]
    fn installer_paths_cannot_escape_updates_directory_through_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("updates");
        fs::create_dir(&root).unwrap();
        let outside = dir.path().join("other.dmg");
        fs::write(&outside, b"dmg").unwrap();
        let inside = root.join("app.dmg");
        fs::write(&inside, b"dmg").unwrap();
        assert_eq!(
            validated_installer_path(inside.to_str().unwrap(), &root).unwrap(),
            inside.canonicalize().unwrap()
        );
        assert!(validated_installer_path(outside.to_str().unwrap(), &root).is_err());
        #[cfg(unix)]
        {
            let link = root.join("escape.dmg");
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(validated_installer_path(link.to_str().unwrap(), &root).is_err());
        }
    }
}

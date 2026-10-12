//! Qoder Switch 桌面客户端（Tauri）
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod qcore;
mod qsession;
mod qstats;

use qcore as qc;
use serde_json::{json, Value};
use std::sync::Mutex;
use std::time::Duration;

struct SchedulerState {
    last_run: Option<String>,
    last_results: Vec<Value>,
}

struct AppState {
    scheduler: Mutex<SchedulerState>,
}

// ───────────────────────── 状态聚合 ─────────────────────────

fn account_public(a: &qc::Account, current_uids: &std::collections::HashMap<String, String>) -> Value {
    json!({
        "id": a.id(),
        "edition": a.edition,
        "editionLabel": qc::edition(&a.edition).map(|e| e.label).unwrap_or(""),
        "name": a.name,
        "email": a.email,
        "phone": a.phone,
        "note": a.note,
        "avatarUrl": a.avatarUrl,
        "uid": a.uid,
        "expiresAt": a.expiresAt,
        "refreshTokenExpiresAt": a.refreshTokenExpiresAt,
        "savedAt": a.savedAt,
        "hasToken": !a.token.is_empty(),
        "isCurrent": current_uids.get(&a.edition).map(|u| u == &a.uid).unwrap_or(false),
    })
}

fn build_state(state: &AppState) -> Value {
    let mut editions = serde_json::Map::new();
    let mut current_uids = std::collections::HashMap::new();
    for ed in qc::EDITIONS {
        let installed = std::path::Path::new(ed.app_path).is_dir();
        let mut uid = String::new();
        let mut uname = String::new();
        if installed {
            if let Some(sess) = qc::decrypt_auth_dat(ed) {
                uid = sess.pointer("/user/id").and_then(|x| x.as_str()).unwrap_or("").to_string();
                uname = sess.pointer("/user/name").and_then(|x| x.as_str())
                    .or(sess.pointer("/user/email").and_then(|x| x.as_str()))
                    .or(sess.pointer("/user/phone").and_then(|x| x.as_str()))
                    .unwrap_or("").to_string();
            }
        }
        current_uids.insert(ed.key.to_string(), uid.clone());
        editions.insert(ed.key.to_string(), json!({
            "label": ed.label,
            "installed": installed,
            "running": installed && qc::app_running(ed.key),
            "currentUid": uid,
            "currentName": uname,
        }));
    }
    let accounts: Vec<Value> = qc::load_accounts()
        .iter()
        .map(|a| account_public(a, &current_uids))
        .collect();
    let sched = state.scheduler.lock().unwrap();
    let log = qc::load_claim_log();
    let tail: Vec<Value> = log.iter().rev().take(30).cloned().collect::<Vec<_>>().into_iter().rev().collect();
    json!({
        "editions": editions,
        "accounts": accounts,
        "settings": qc::load_settings(),
        "scheduler": {
            "lastRun": sched.last_run,
            "lastResults": sched.last_results,
        },
        "claimLog": tail,
    })
}

#[tauri::command]
fn ping() -> String {
    "pong".to_string()
}

#[tauri::command]
async fn clear_claim_log() -> Result<Value, String> {
    qc::write_claim_log(&serde_json::json!([]));
    Ok(serde_json::json!({"ok": true, "message": "领取记录已清除"}))
}

// ───────────────────────── Tauri 命令 ─────────────────────────

#[tauri::command]
async fn get_state(state: tauri::State<'_, AppState>) -> Result<Value, String> {
    let (lr, lres) = {
        let g = state.scheduler.lock().unwrap();
        (g.last_run.clone(), g.last_results.clone())
    };
    let inner = AppState {
        scheduler: Mutex::new(SchedulerState {
            last_run: lr,
            last_results: lres,
        }),
    };
    let r = tauri::async_runtime::spawn_blocking(move || build_state(&inner))
        .await
        .map_err(|e| e.to_string());
    r
}

#[tauri::command]
async fn capture(edition: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        match qc::capture_current(&edition) {
            Ok((acc, is_new)) => Ok(json!({
                "ok": true,
                "message": format!("{} {}", if is_new { "已采集新账号" } else { "已更新账号" }, acc.name),
            })),
            Err(e) => Ok(json!({ "ok": false, "message": e })),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn switch_account(id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut steps = vec![];
        match qc::switch_account(&id, &mut steps) {
            Ok(msg) => Ok(json!({ "ok": true, "message": msg, "steps": steps })),
            Err(e) => Ok(json!({ "ok": false, "message": e, "steps": steps })),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn claim(id: Option<String>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(aid) = id {
            let mut accounts = qc::load_accounts();
            let pos = accounts.iter().position(|a| a.id() == aid);
            match pos {
                Some(i) => {
                    let r = qc::claim_account(&mut accounts[i], false);
                    let _ = qc::save_accounts(&accounts);
                    qc::append_claim_log(&[r.clone()]);
                    let ok = matches!(r.get("phase").and_then(|x| x.as_str()),
                        Some("claimed") | Some("nothing") | Some("dry_run"));
                    Ok(json!({
                        "ok": ok,
                        "message": format!("{}：{}", r.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                            r.get("msg").and_then(|x| x.as_str()).unwrap_or("")),
                    }))
                }
                None => Ok(json!({ "ok": false, "message": "账号不存在" })),
            }
        } else {
            let results = qc::claim_all(None, false);
            let total: f64 = results.iter()
                .filter_map(|r| r.get("credits").and_then(|x| x.as_f64()))
                .sum();
            let lines: Vec<String> = results.iter().map(|r| format!(
                "{} {}",
                r.get("name").and_then(|x| x.as_str()).unwrap_or(""),
                r.get("msg").and_then(|x| x.as_str()).unwrap_or(""),
            )).collect();
            Ok(json!({
                "ok": true,
                "message": format!("共领取 {} Credits。{}", total, lines.join("；")),
            }))
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn credits(id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut accounts = qc::load_accounts();
        let pos = accounts.iter().position(|a| a.id() == id);
        let i = match pos {
            Some(i) => i,
            None => return Ok(json!({ "ok": false, "message": "账号不存在" })),
        };
        let acc = &mut accounts[i];
        if let Err(e) = qc::ensure_token(acc) {
            return Ok(json!({ "ok": false, "message": e }));
        }
        let _ = qc::save_accounts(&accounts);
        let acc = &accounts[i];
        let (s1, summary) = qc::credits_summary(acc);
        let (s2, camps) = qc::get_campaigns(acc);
        let (s3, usage) = qc::quota_info(acc);
        Ok(json!({
            "ok": true,
            "data": {
                "summary": if s1 == 200 { summary } else { Value::Null },
                "campaigns": if s2 == 200 { camps.get("campaigns").cloned().unwrap_or(json!([])) } else { json!([]) },
                "usage": if s3 == 200 { usage.get("qoderUsage").cloned().unwrap_or(usage) } else { Value::Null },
            },
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn set_note(id: String, note: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut accounts = qc::load_accounts();
        for a in accounts.iter_mut() {
            if a.id() == id {
                a.note = note.clone();
            }
        }
        let _ = qc::save_accounts(&accounts);
        Ok(json!({ "ok": true, "message": "已保存备注" }))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn delete_account(id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let accounts: Vec<qc::Account> = qc::load_accounts()
            .into_iter()
            .filter(|a| a.id() != id)
            .collect();
        let _ = qc::save_accounts(&accounts);
        Ok(json!({ "ok": true, "message": "已删除" }))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn set_auto_claim(enabled: bool) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut s = qc::load_settings();
        s["autoClaim"] = json!(enabled);
        qc::save_settings(&s);
        Ok(json!({
            "ok": true,
            "message": format!("自动领取已{}", if enabled { "开启" } else { "关闭" }),
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn set_theme(theme: String) -> Result<Value, String> {
    if theme != "light" && theme != "dark" {
        return Err("无效的主题".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut s = qc::load_settings();
        s["theme"] = json!(theme);
        qc::save_settings(&s);
        Ok(json!({ "ok": true }))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// 通用偏好设置（语言 / 时区 / 数字格式等），白名单键防脏数据
#[tauri::command]
async fn set_setting(key: String, value: Value) -> Result<Value, String> {
    const ALLOWED: [&str; 4] = ["lang", "timezone", "tokenFmt", "theme"];
    if !ALLOWED.contains(&key.as_str()) {
        return Err(format!("不支持的设置项：{key}"));
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut s = qc::load_settings();
        s[key.as_str()] = value;
        qc::save_settings(&s);
        Ok(json!({ "ok": true }))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn open_app(edition: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        qc::launch_app(&edition);
        Ok(json!({ "ok": true, "message": "已启动" }))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn open_url(url: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        std::process::Command::new("open")
            .arg(&url)
            .output()
            .map_err(|e| e.to_string())?;
        Ok(json!({ "ok": true }))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn export_accounts() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let path = dirs::download_dir()
            .unwrap_or_else(|| dirs::home_dir().unwrap_or_default())
            .join("qoder-accounts-backup.json");
        match qc::export_accounts(path.to_string_lossy().as_ref()) {
            Ok(n) => Ok(json!({
                "ok": true,
                "count": n,
                "message": format!("已导出 {} 个账号到 {}", n, path.display()),
            })),
            Err(e) => Ok(json!({ "ok": false, "message": e })),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn import_accounts(path: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        match qc::import_accounts(&path) {
            Ok(n) => Ok(json!({ "ok": n > 0, "message": format!("成功导入/更新 {} 个账号", n) })),
            Err(e) => Ok(json!({ "ok": false, "message": e })),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

// ───────────────────────── 会话管理命令 ─────────────────────────

#[tauri::command]
async fn list_sessions(edition: String, query: Option<String>, limit: Option<i64>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        qsession::list_sessions(&edition, query.as_deref().unwrap_or(""), limit.unwrap_or(200))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn copy_session(edition: String, session_id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || qsession::copy_session(&edition, &session_id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn delete_session(edition: String, session_id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || qsession::delete_session(&edition, &session_id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn session_links() -> Result<Value, String> {
    Ok(qsession::session_links())
}

#[tauri::command]
async fn sync_session_group(group_id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || qsession::sync_group(&group_id))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn credit_stats(tz_offset: Option<i64>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || Ok(qsession::credit_stats(tz_offset)))
        .await
        .map_err(|e| e.to_string())?
}

// ───────────────────────── 添加账号（OAuth 设备登录） ─────────────────────────

#[tauri::command]
async fn login_begin(edition: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let (attempt, url) = qc::login_begin(&edition)?;
        Ok(json!({
            "ok": true,
            "url": url,
            "attempt": serde_json::to_value(&attempt).map_err(|e| e.to_string())?,
        }))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn login_poll(attempt: Value) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let att: qc::LoginAttempt = serde_json::from_value(attempt).map_err(|e| e.to_string())?;
        match qc::login_poll(&att)? {
            None => Ok(json!({ "done": false })),
            Some(sess) => {
                let edition = att.edition.clone();
                let (acc, is_new) = qc::save_login_account(&edition, &sess)?;
                Ok(json!({
                    "done": true,
                    "ok": true,
                    "isNew": is_new,
                    "name": acc.name,
                    "message": format!("{} {}", if is_new { "已添加账号" } else { "已更新账号" }, acc.name),
                }))
            }
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

// ───────────────────────── 账号刷新 ─────────────────────────

#[tauri::command]
async fn refresh_account(id: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut accounts = qc::load_accounts();
        let pos = accounts.iter().position(|a| a.id() == id);
        let Some(i) = pos else { return Ok(json!({ "ok": false, "message": "账号不存在" })) };
        let acc = &mut accounts[i];
        if acc.refreshToken.is_empty() {
            return Ok(json!({ "ok": false, "message": "该账号没有 refreshToken，无法刷新（请重新添加或采集）" }));
        }
        if qc::refresh_account_token(acc) {
            let name = acc.name.clone();
            let exp = acc.expiresAt.clone();
            qc::persist_and_writeback(&accounts[i].clone());
            let _ = qc::save_accounts(&accounts);
            Ok(json!({
                "ok": true,
                "expiresAt": exp,
                "message": format!("已刷新 {} 的登录凭证，有效期至 {}", name, exp.get(..10).unwrap_or("未知")),
            }))
        } else {
            Ok(json!({ "ok": false, "message": "刷新失败：refreshToken 已失效或网络错误，请在客户端重新登录" }))
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

// ───────────────────────── Token 统计 ─────────────────────────

#[tauri::command]
async fn token_stats(edition: String, tz_offset: Option<i64>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || qstats::token_stats(&edition, tz_offset))
        .await
        .map_err(|e| e.to_string())?
}

// ───────────────────────── 后台自动领取 ─────────────────────────

fn spawn_auto_claim(app: tauri::AppHandle) {
    let app1 = app.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(600));
        run_auto_claim(&app1);
    });
    // 启动 30 秒后先检查一次
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(30));
        run_auto_claim(&app);
    });
}

fn run_auto_claim(app: &tauri::AppHandle) {
    use tauri::Manager;
    let state = app.state::<AppState>();
    if !qc::load_settings().get("autoClaim").and_then(|x| x.as_bool()).unwrap_or(true) {
        return;
    }
    let accounts = qc::load_accounts();
    if accounts.is_empty() {
        return;
    }
    let mut need = false;
    for acc in &accounts {
        if acc.token.is_empty() {
            continue;
        }
        let (status, data) = qc::get_campaigns(acc);
        if status == 200 && !qc::claimable_campaigns(&data).is_empty() {
            need = true;
            break;
        }
        if status == 401 && !acc.refreshToken.is_empty() {
            need = true;
            break;
        }
    }
    if !need {
        return;
    }
    let results = qc::claim_all(None, false);
    let mut sched = state.scheduler.lock().unwrap();
    sched.last_run = Some(chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string());
    sched.last_results = results;
}

// ───────────────────────── 入口 ─────────────────────────

fn main() {
    // 调试：--dump-state 直接打印 get_state 的 JSON 并退出
    if std::env::args().any(|a| a == "--dump-state") {
        let state = AppState {
            scheduler: Mutex::new(SchedulerState {
                last_run: None,
                last_results: vec![],
            }),
        };
        println!("{}", serde_json::to_string_pretty(&build_state(&state)).unwrap());
        return;
    }
    // 调试：--claim 立即对所有账号执行一次领取并打印结果
    if std::env::args().any(|a| a == "--claim") {
        let results = qc::claim_all(None, false);
        for r in &results {
            println!("{}", serde_json::to_string(r).unwrap());
        }
        return;
    }
    // 调试：--sessions <edition> 打印会话列表；--cstats 打印积分统计
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--sessions") {
        let ed = args.get(pos + 1).map(|s| s.as_str()).unwrap_or("cn");
        let r = qsession::list_sessions(ed, "", 20).unwrap_or_else(|e| serde_json::json!({"error": e}));
        println!("{}", serde_json::to_string_pretty(&r).unwrap());
        return;
    }
    if args.iter().any(|a| a == "--cstats") {
        println!("{}", serde_json::to_string(&qsession::credit_stats(None)).unwrap());
        return;
    }
    if let Some(pos) = args.iter().position(|a| a == "--tstats") {
        let ed = args.get(pos + 1).map(|s| s.as_str()).unwrap_or("cn");
        match qstats::token_stats(ed, None) {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
            Err(e) => println!("{{\"error\":\"{}\"}}", e),
        }
        return;
    }
    // 调试：--copy <edition> <session_id> 复制会话（stderr 输出分阶段耗时）
    if let Some(pos) = args.iter().position(|a| a == "--copy") {
        let ed = args.get(pos + 1).cloned().unwrap_or_else(|| "cn".into());
        let sid = args.get(pos + 2).cloned().unwrap_or_default();
        match qsession::copy_session(&ed, &sid) {
            Ok(v) => println!("{}", serde_json::to_string_pretty(&v).unwrap()),
            Err(e) => eprintln!("复制失败：{}", e),
        }
        return;
    }
    // 调试：--switch <account_id> 切换账号（stderr 输出步骤）
    if let Some(pos) = args.iter().position(|a| a == "--switch") {
        let id = args.get(pos + 1).cloned().unwrap_or_default();
        let mut steps = vec![];
        match qc::switch_account(&id, &mut steps) {
            Ok(msg) => {
                for s in &steps { eprintln!("[step] {}", s); }
                println!("OK: {}", msg);
            }
            Err(e) => {
                for s in &steps { eprintln!("[step] {}", s); }
                eprintln!("切换失败：{}", e);
                std::process::exit(1);
            }
        }
        return;
    }
    tauri::Builder::default()
        .manage(AppState {
            scheduler: Mutex::new(SchedulerState {
                last_run: None,
                last_results: vec![],
            }),
        })
        .invoke_handler(tauri::generate_handler![
            ping,
            get_state,
            capture,
            switch_account,
            claim,
            credits,
            set_note,
            delete_account,
            set_auto_claim,
            set_theme,
            set_setting,
            open_app,
            open_url,
            export_accounts,
            import_accounts,
            list_sessions,
            copy_session,
            delete_session,
            session_links,
            sync_session_group,
            credit_stats,
            login_begin,
            login_poll,
            refresh_account,
            token_stats,
            clear_claim_log,
        ])
        .setup(|app| {
            spawn_auto_claim(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running qoder-switch");
}

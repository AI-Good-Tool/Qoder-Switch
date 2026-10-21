//! Qoder 会话管理：列表 / 副本 / 删除 / 关联组同步 / 积分统计
//!
//! Qoder 会话三件套：
//!   1) 索引：`~/Library/Application Support/com.qoder[cn].app.stable/main.sqlite` 的 chat_sessions 表
//!   2) 正文：`~/.qoder[-cn]/projects/<项目路径编码>/<sessionId>.jsonl`（每行含 sessionId 字段）
//!   3) 边车：同名目录（state.json + compression-v2/）
//!
//! 关联组存 `~/.qoder-switch/session-links.json`：
//!   { "version":1, "groups":[{ "id", "createdAt", "members":[{edition, sessionId, digest, updatedAt}] }] }
//! 状态判定：成员文件 sha1 摘要全部一致 → 内容一致；否则 → 待同步。

use crate::qcore::{self, Edition};
use serde_json::{json, Value};
use sha1::{Digest, Sha1};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

// ───────────────────────── 路径与基础工具 ─────────────────────────

pub fn db_path(ed: &Edition) -> PathBuf {
    qcore::home_dir().join(ed.data_dir).join("main.sqlite")
}

pub fn projects_dir(ed: &Edition) -> PathBuf {
    qcore::home_dir().join(ed.home_dir).join("projects")
}

fn links_file() -> PathBuf {
    qcore::switch_dir().join("session-links.json")
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn new_uuid() -> String {
    // 注意：/dev/urandom 永远不会 EOF，绝不能用 fs::read（会无限读取直到内存耗尽）！
    // 必须定长 read_exact 只取 16 字节。
    use std::io::Read;
    let mut b = [0u8; 16];
    let mut ok = false;
    if let Ok(mut f) = fs::File::open("/dev/urandom") {
        ok = f.read_exact(&mut b).is_ok();
    }
    if !ok || b.iter().all(|&x| x == 0) {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        for (i, chunk) in b.chunks_mut(2).enumerate() {
            let v = ((seed >> (i * 7)) & 0xffff) as u8;
            for (j, c) in chunk.iter_mut().enumerate() {
                *c = v.wrapping_add((i * 31 + j * 7) as u8);
            }
        }
    }
    b[6] = (b[6] & 0x0f) | 0x40; // v4
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

fn sql_escape(s: &str) -> String {
    s.replace('\'', "''")
}

/// sqlite3 CLI 查询（-json 输出），返回行数组。只读，可与应用并发。
pub fn sqlite_query(db: &Path, sql: &str) -> Result<Vec<Value>, String> {
    let out = Command::new("sqlite3")
        .arg("-json")
        .arg(db)
        .arg(sql)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "sqlite3 查询失败: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        return Ok(vec![]);
    }
    serde_json::from_str(&text).map_err(|e| format!("解析 sqlite 输出失败: {}", e))
}

/// sqlite3 CLI 写入。调用方须确保 Qoder 应用已退出（避免写锁冲突）。
fn sqlite_exec(db: &Path, sql: &str) -> Result<(), String> {
    let out = Command::new("sqlite3")
        .arg(db)
        .arg(sql)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "sqlite3 写入失败: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

fn file_digest(path: &Path) -> Option<String> {
    let data = fs::read(path).ok()?;
    let mut h = Sha1::new();
    h.update(&data);
    Some(format!("{:x}", h.finalize()))
}

// ───────────────────────── 关联组存储 ─────────────────────────

fn load_links() -> Value {
    fs::read_to_string(links_file())
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_else(|| json!({ "version": 1, "groups": [] }))
}

fn save_links(links: &Value) -> Result<(), String> {
    let dir = qcore::switch_dir();
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let tmp = links_file().with_extension("tmp");
    fs::write(&tmp, serde_json::to_string_pretty(links).unwrap()).map_err(|e| e.to_string())?;
    fs::rename(&tmp, links_file()).map_err(|e| e.to_string())
}

fn find_session_jsonl(ed: &Edition, session_id: &str) -> Option<PathBuf> {
    let root = projects_dir(ed);
    // 会话文件在任意项目目录下，按名称查找（一层项目目录足够）
    for proj in fs::read_dir(&root).ok()? {
        let proj = proj.ok()?.path();
        let f = proj.join(format!("{}.jsonl", session_id));
        if f.exists() {
            return Some(f);
        }
    }
    None
}

fn jsonl_for(ed: &Edition, session_id: &str) -> Option<PathBuf> {
    find_session_jsonl(ed, session_id)
}

// ───────────────────────── 会话列表 ─────────────────────────

pub fn list_sessions(edition_key: &str, query: &str, limit: i64) -> Result<Value, String> {
    let ed = qcore::edition(edition_key).ok_or("未知版本")?;
    let db = db_path(ed);
    if !db.exists() {
        return Ok(json!({ "sessions": [], "total": 0 }));
    }
    let lim = limit.clamp(1, 500);
    let q = query.trim();
    let where_clause = if q.is_empty() {
        "WHERE deleted_at IS NULL AND session_kind='standard'".to_string()
    } else {
        format!(
            "WHERE deleted_at IS NULL AND session_kind='standard' AND (title LIKE '%{}%' OR cwd LIKE '%{}%')",
            sql_escape(q),
            sql_escape(q)
        )
    };
    let rows = sqlite_query(
        &db,
        &format!(
            "SELECT session_id, origin_session_id, title, cwd, workspace_id, model, created_at, updated_at, archived \
             FROM chat_sessions {} ORDER BY updated_at DESC LIMIT {}",
            where_clause.clone(), lim
        ),
    )
    .map_err(|e| e)?;
    let total = sqlite_query(
        &db,
        &format!(
            "SELECT COUNT(*) AS n FROM chat_sessions {}",
            if q.is_empty() {
                "WHERE deleted_at IS NULL AND session_kind='standard'".to_string()
            } else {
                where_clause
            }
        ),
    )?
    .first()
    .and_then(|r| r["n"].as_i64())
    .unwrap_or(0);

    let links = load_links();
    let mut sessions = Vec::new();
    for r in rows {
        let sid = r["session_id"].as_str().unwrap_or("").to_string();
        let jsonl = jsonl_for(ed, &sid);
        let (size, has_file) = match &jsonl {
            Some(p) => (fs::metadata(p).ok().map(|m| m.len()).unwrap_or(0), true),
            None => (0, false),
        };
        // 关联信息
        let mut group_id = Value::Null;
        let mut content_state = "unlinked";
        if let Some(groups) = links["groups"].as_array() {
            for g in groups {
                let members = g["members"].as_array().cloned().unwrap_or_default();
                let hit = members.iter().any(|m| {
                    m["sessionId"].as_str() == Some(sid.as_str())
                        && m["edition"].as_str() == Some(edition_key)
                });
                if hit {
                    group_id = g["id"].clone();
                    // 重新计算各成员当前摘要做状态判定
                    let mut cur = Vec::new();
                    for m in &members {
                        let Some(med) = qcore::edition(m["edition"].as_str().unwrap_or("")) else {
                            continue;
                        };
                        let Some(mid) = m["sessionId"].as_str() else {
                            continue;
                        };
                        if let Some(p) = jsonl_for(med, mid) {
                            if let Some(d) = file_digest(&p) {
                                cur.push(d);
                            }
                        }
                    }
                    let all_same = cur.len() >= 2 && cur.iter().all(|d| *d == cur[0]);
                    content_state = if all_same { "consistent" } else { "modified" };
                    break;
                }
            }
        }
        sessions.push(json!({
            "id": sid,
            "edition": edition_key,
            "title": r["title"],
            "cwd": r["cwd"],
            "model": r["model"],
            "archived": r["archived"],
            "createdAt": r["created_at"],
            "updatedAt": r["updated_at"],
            "size": size,
            "hasFile": has_file,
            "groupId": group_id,
            "contentState": content_state,
        }));
    }
    Ok(json!({ "sessions": sessions, "total": total }))
}

// ───────────────────────── 复制会话（生成新 UUID 副本） ─────────────────────────

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<(), String> {
    fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_recursive(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn rewrite_session_ids(path: &Path, old_id: &str, new_id: &str) -> Result<(), String> {
    let content = fs::read_to_string(path).map_err(|e| e.to_string())?;
    fs::write(path, content.replace(old_id, new_id)).map_err(|e| e.to_string())
}

/// 在数据库中登记副本行（要求应用已退出）。
fn insert_copy_row(ed: &Edition, src: &Value, new_id: &str) -> Result<(), String> {
    let db = db_path(ed);
    let g = |k: &str| -> String {
        src.get(k)
            .map(|v| match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            })
            .unwrap_or_default()
    };
    let now = now_ms();
    let sql = format!(
        "INSERT INTO chat_sessions (session_id, origin_session_id, session_kind, conversation_mode, product_mode, \
         title, cwd, workspace_id, git_branch, model, permission_mode, additional_direcotries, extra_json, \
         created_at, updated_at, execution_target_json, archived, unread) \
         VALUES ('{sid}', '{origin}', 'standard', 'normal', 'coding', '{title}', '{cwd}', {ws}, {branch}, {model}, \
         '{perm}', '{{}}', '{{}}', {now}, {now}, '{{}}', 0, 0);",
        sid = new_id,
        origin = g("session_id"),
        title = sql_escape(&g("title")),
        cwd = sql_escape(&g("cwd")),
        ws = match src["workspace_id"].as_str() {
            Some(w) => format!("'{}'", sql_escape(w)),
            None => "NULL".to_string(),
        },
        branch = match src["git_branch"].as_str() {
            Some(w) => format!("'{}'", sql_escape(w)),
            None => "NULL".to_string(),
        },
        model = match src["model"].as_str() {
            Some(w) => format!("'{}'", sql_escape(w)),
            None => "NULL".to_string(),
        },
        perm = sql_escape(&g("permission_mode")),
        now = now
    );
    sqlite_exec(&db, &sql)
}

pub fn copy_session(edition_key: &str, session_id: &str) -> Result<Value, String> {
    let t0 = std::time::Instant::now();
    let log = |msg: String| eprintln!("[copy_session +{:?}] {}", t0.elapsed(), msg);
    log(format!(
        "开始：edition={} session={}",
        edition_key,
        &session_id[..8.min(session_id.len())]
    ));
    let ed = qcore::edition(edition_key).ok_or("未知版本")?;
    if qcore::app_running(edition_key) {
        log("应用运行中，先退出…".into());
        let ok = qcore::quit_app(edition_key);
        log(format!("quit_app 返回 {}（耗时 {:?}）", ok, t0.elapsed()));
    }
    let db = db_path(ed);
    let rows = sqlite_query(
        &db,
        &format!(
            "SELECT session_id, title, cwd, workspace_id, git_branch, model, permission_mode \
             FROM chat_sessions WHERE session_id='{}'",
            sql_escape(session_id)
        ),
    )?;
    log("源会话查询完成".into());
    let src = rows.first().cloned().ok_or("会话不存在")?;
    let src_jsonl = jsonl_for(ed, session_id).ok_or("找不到会话正文文件")?;
    let new_id = new_uuid();
    log("已定位正文文件".into());

    // 1) 正文 jsonl（重写 sessionId）
    let proj_dir = src_jsonl.parent().ok_or("解析项目目录失败")?.to_path_buf();
    let new_jsonl = proj_dir.join(format!("{}.jsonl", new_id));
    fs::copy(&src_jsonl, &new_jsonl).map_err(|e| e.to_string())?;
    rewrite_session_ids(&new_jsonl, session_id, &new_id)?;
    log("正文复制+重写完成".into());

    // 2) 边车目录
    let src_side = proj_dir.join(session_id);
    if src_side.is_dir() {
        copy_dir_recursive(&src_side, &proj_dir.join(&new_id))?;
        let state = proj_dir.join(&new_id).join("state.json");
        if state.exists() {
            rewrite_session_ids(&state, session_id, &new_id)?;
        }
        log("边车目录复制完成".into());
    }

    // 3) 数据库登记
    insert_copy_row(ed, &src, &new_id)?;
    log("数据库登记完成".into());

    // 4) 关联组：源会话所在组追加成员；无组则新建
    let mut links = load_links();
    let groups = links["groups"].as_array_mut().ok_or("关联数据损坏")?;
    let mut member = json!({
        "edition": edition_key,
        "sessionId": new_id,
        "digest": file_digest(&new_jsonl).unwrap_or_default(),
        "updatedAt": now_ms(),
    });
    let mut reused = false;
    for g in groups.iter_mut() {
        let hit = g["members"].as_array().map(|ms| {
            ms.iter().any(|m| {
                m["sessionId"].as_str() == Some(session_id)
                    && m["edition"].as_str() == Some(edition_key)
            })
        });
        if hit == Some(true) {
            if let Some(ms) = g["members"].as_array_mut() {
                ms.push(member.take());
            }
            reused = true;
            break;
        }
    }
    if !reused {
        // 源会话自身也要入组
        let src_member = json!({
            "edition": edition_key,
            "sessionId": session_id,
            "digest": file_digest(&src_jsonl).unwrap_or_default(),
            "updatedAt": now_ms(),
        });
        groups.push(json!({
            "id": new_uuid(),
            "createdAt": now_ms(),
            "members": [src_member, member],
        }));
    }
    save_links(&links)?;

    Ok(json!({
        "ok": true,
        "message": format!("已复制会话副本 {}", &new_id[..8]),
        "newSessionId": new_id,
    }))
}

// ───────────────────────── 关联组：列表与同步 ─────────────────────────

pub fn session_links() -> Value {
    let mut links = load_links();
    let groups = links["groups"]
        .as_array_mut()
        .map(|g| g.to_vec())
        .unwrap_or_default();
    let mut out = Vec::new();
    for mut g in groups {
        let members = g["members"].as_array().cloned().unwrap_or_default();
        let mut detail = Vec::new();
        let mut cur_digests = Vec::new();
        for m in &members {
            let Some(ed) = qcore::edition(m["edition"].as_str().unwrap_or("")) else {
                continue;
            };
            let Some(sid) = m["sessionId"].as_str() else {
                continue;
            };
            let jsonl = jsonl_for(ed, sid);
            let title = jsonl
                .as_ref()
                .and_then(|p| {
                    sqlite_query(
                        &db_path(ed),
                        &format!(
                            "SELECT title, cwd, updated_at FROM chat_sessions WHERE session_id='{}'",
                            sql_escape(sid)
                        ),
                    )
                    .ok()?
                    .into_iter()
                    .next()
                })
                .unwrap_or(json!({}));
            let digest = jsonl.as_deref().and_then(file_digest).unwrap_or_default();
            if !digest.is_empty() {
                cur_digests.push(digest.clone());
            }
            detail.push(json!({
                "edition": m["edition"],
                "sessionId": sid,
                "title": title.get("title").cloned().unwrap_or(Value::Null),
                "cwd": title.get("cwd").cloned().unwrap_or(Value::Null),
                "updatedAt": title.get("updated_at").cloned().unwrap_or(Value::Null),
                "exists": jsonl.is_some(),
                "digest": digest,
                "savedDigest": m["digest"].clone(),
            }));
        }
        let all_same = cur_digests.len() >= 2 && cur_digests.iter().all(|d| *d == cur_digests[0]);
        g["membersDetail"] = Value::Array(detail);
        g["contentState"] = json!(if all_same { "consistent" } else { "modified" });
        out.push(g);
    }
    json!({ "groups": out })
}

/// 同步关联组：以最新成员（文件修改时间）为准，把正文覆盖到其他成员（各自保留自己的 sessionId）。
pub fn sync_group(group_id: &str) -> Result<Value, String> {
    let mut links = load_links();
    let groups = links["groups"].as_array_mut().ok_or("关联数据损坏")?;
    let gi = groups
        .iter()
        .position(|g| g["id"].as_str() == Some(group_id))
        .ok_or("关联组不存在")?;
    let members = groups[gi]["members"]
        .as_array()
        .cloned()
        .ok_or("组数据损坏")?;
    if members.len() < 2 {
        return Err("关联组成员不足".to_string());
    }
    // 找最新成员
    let mut best: Option<(PathBuf, String, std::time::SystemTime)> = None;
    for m in &members {
        let Some(ed) = qcore::edition(m["edition"].as_str().unwrap_or("")) else {
            continue;
        };
        let Some(sid) = m["sessionId"].as_str() else {
            continue;
        };
        if let Some(p) = jsonl_for(ed, sid) {
            let mt = fs::metadata(&p)
                .and_then(|m| m.modified())
                .unwrap_or(UNIX_EPOCH);
            if best.as_ref().map(|(_, _, t)| mt > *t).unwrap_or(true) {
                best = Some((p, sid.to_string(), mt));
            }
        }
    }
    let (src_path, src_id, _) = best.ok_or("找不到可用源会话")?;
    let src_content = fs::read_to_string(&src_path).map_err(|e| e.to_string())?;
    let mut synced = 0usize;
    for m in &members {
        let Some(ed) = qcore::edition(m["edition"].as_str().unwrap_or("")) else {
            continue;
        };
        let Some(sid) = m["sessionId"].as_str() else {
            continue;
        };
        if sid == &src_id {
            continue;
        }
        let Some(p) = jsonl_for(ed, sid) else {
            continue;
        };
        let content = src_content.replace(&src_id, sid);
        fs::write(&p, content).map_err(|e| e.to_string())?;
        let side_old = src_path.parent().unwrap().join(&src_id);
        let side_new = p.parent().unwrap().join(sid);
        if side_old.is_dir() {
            copy_dir_recursive(&side_old, &side_new).ok();
            let state = side_new.join("state.json");
            if state.exists() {
                rewrite_session_ids(&state, &src_id, sid).ok();
            }
        }
        synced += 1;
    }
    // 更新组内摘要
    if let Some(g) = groups.get_mut(gi) {
        if let Some(ms) = g["members"].as_array_mut() {
            for m in ms.iter_mut() {
                if let (Some(ed), Some(sid)) = (
                    qcore::edition(m["edition"].as_str().unwrap_or("")),
                    m["sessionId"].as_str(),
                ) {
                    if let Some(p) = jsonl_for(ed, sid) {
                        m["digest"] = json!(file_digest(&p).unwrap_or_default());
                        m["updatedAt"] = json!(now_ms());
                    }
                }
            }
        }
    }
    save_links(&links)?;
    Ok(json!({ "ok": true, "message": format!("已同步 {} 个会话副本", synced) }))
}

// ───────────────────────── 删除会话（先备份） ─────────────────────────

pub fn delete_session(edition_key: &str, session_id: &str) -> Result<Value, String> {
    let ed = qcore::edition(edition_key).ok_or("未知版本")?;
    if qcore::app_running(edition_key) {
        qcore::quit_app(edition_key);
    }
    let db = db_path(ed);
    let rows = sqlite_query(
        &db,
        &format!(
            "SELECT * FROM chat_sessions WHERE session_id='{}'",
            sql_escape(session_id)
        ),
    )?;
    let row = rows.first().cloned().ok_or("会话不存在")?;
    // 备份
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let backup_dir = qcore::switch_dir().join("session-backups").join(format!(
        "{}-{}",
        ts,
        &session_id[..8.min(session_id.len())]
    ));
    fs::create_dir_all(&backup_dir).map_err(|e| e.to_string())?;
    fs::write(
        backup_dir.join("chat_sessions-row.json"),
        serde_json::to_string_pretty(&row).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    if let Some(jsonl) = jsonl_for(ed, session_id) {
        fs::copy(&jsonl, backup_dir.join("session.jsonl")).map_err(|e| e.to_string())?;
        let side = jsonl.parent().unwrap().join(session_id);
        if side.is_dir() {
            copy_dir_recursive(&side, &backup_dir.join("sidecar"))?;
        }
    }
    // 删除（软删除：deleted_at；正文与边车同时移除）
    sqlite_exec(
        &db,
        &format!(
            "UPDATE chat_sessions SET deleted_at={} WHERE session_id='{}';",
            now_ms(),
            sql_escape(session_id)
        ),
    )?;
    if let Some(jsonl) = jsonl_for(ed, session_id) {
        fs::remove_file(&jsonl).ok();
        let side = jsonl.parent().unwrap().join(session_id);
        if side.is_dir() {
            fs::remove_dir_all(&side).ok();
        }
    }
    // 渲染进程缓存清理：IDE 的 localStorage（leveldb）里 qoder.workbench.navigation.lastRoute.v1
    // 若仍指向已删会话，IDE 启动会恢复打开它（open 路径不过滤 deleted_at），侧边栏因此仍显示。
    let route_cleared = clear_renderer_route_cache(ed, session_id);
    let mut msg = format!("会话已删除（备份 {}）", backup_dir.display());
    if route_cleared {
        msg.push_str("；已同步清理 IDE 打开记录");
    }
    Ok(json!({
        "ok": true,
        "message": msg,
        "backup": backup_dir.display().to_string(),
    }))
}

/// 若渲染进程 leveldb 缓存中引用了该会话（如 lastRoute），删除整个 leveldb 目录。
/// 该目录只是 UI 偏好缓存（最后打开路由等），IDE 会自动重建；需在 IDE 退出后调用。
fn clear_renderer_route_cache(ed: &Edition, session_id: &str) -> bool {
    let leveldb = qcore::home_dir()
        .join(&ed.data_dir)
        .join("Local Storage")
        .join("leveldb");
    if !leveldb.is_dir() {
        return false;
    }
    let mut found = false;
    if let Ok(rd) = fs::read_dir(&leveldb) {
        'outer: for entry in rd.flatten() {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            if let Ok(data) = fs::read(&p) {
                // UUID 以 ASCII 存储；再兜底 UTF-16LE 编码
                let mut u16le = Vec::new();
                for unit in session_id.encode_utf16() {
                    u16le.extend_from_slice(&unit.to_le_bytes());
                }
                if data
                    .windows(session_id.len())
                    .any(|w| w == session_id.as_bytes())
                    || data.windows(u16le.len()).any(|w| w == u16le)
                {
                    found = true;
                    break 'outer;
                }
            }
        }
    }
    if found {
        fs::remove_dir_all(&leveldb).ok();
    }
    found
}

// ───────────────────────── 积分统计 ─────────────────────────

/// 拉取账号的积分摘要 + 每日热力图。返回 {accounts:[...], total: {...}, daily: {date: sum}}
/// tz_off：自定义时区偏移（分钟），None=跟随系统。
pub fn credit_stats(tz_off: Option<i64>) -> Value {
    let mut accounts = Vec::new();
    let mut daily: std::collections::BTreeMap<String, f64> = std::collections::BTreeMap::new();
    let mut total_credits = 0.0f64;
    let mut peak_credits = 0.0f64;
    let mut peak_date = String::new();
    for acc in qcore::load_accounts() {
        if acc.token.is_empty() {
            continue;
        }
        let Some(ed) = qcore::edition(&acc.edition) else {
            continue;
        };
        // summary
        let (s_status, s_body) = qcore::api_call(
            ed,
            "/sash/api/v1/ai-conversations/credits-summary",
            "GET",
            &acc.token,
            None,
        );
        let summary = if s_status == 200 { s_body } else { json!({}) };
        // heatmap
        let (h_status, h_body) = qcore::api_call(
            ed,
            "/sash/api/v1/ai-conversations/credits-heatmap",
            "GET",
            &acc.token,
            None,
        );
        let items = if h_status == 200 {
            h_body["items"].as_array().cloned().unwrap_or_default()
        } else {
            vec![]
        };
        for it in &items {
            let d = it["date"].as_str().unwrap_or("").to_string();
            let v = it["value"].as_f64().unwrap_or(0.0);
            *daily.entry(d).or_insert(0.0) += v;
        }
        let tc = summary["totalCredits"].as_f64().unwrap_or(0.0);
        let pc = summary["peakCredits"].as_f64().unwrap_or(0.0);
        // 配额（套餐 + 加量包合并）
        let (q_status, q_body) =
            qcore::api_call(ed, "/sash/api/v2/me/usage", "GET", &acc.token, None);
        let (mut q_total, mut q_used) = (0.0f64, 0.0f64);
        if q_status == 200 {
            for key in ["userQuota", "addOnQuota"] {
                let q = &q_body["qoderUsage"][key];
                q_total += q["total"].as_f64().unwrap_or(0.0);
                q_used += q["used"].as_f64().unwrap_or(0.0);
            }
        }
        total_credits += tc;
        if pc > peak_credits {
            peak_credits = pc;
            peak_date = summary["peakDate"].as_str().unwrap_or("").to_string();
        }
        accounts.push(json!({
            "id": acc.id(),
            "edition": acc.edition,
            "name": acc.name,
            "ok": s_status == 200 && h_status == 200,
            "totalCredits": tc,
            "peakCredits": pc,
            "peakDate": summary["peakDate"],
            "quotaTotal": q_total,
            "quotaUsed": q_used,
            "heat": items,
        }));
    }
    let today_key = match tz_off {
        Some(o) => (chrono::Utc::now() + chrono::Duration::minutes(o))
            .format("%Y-%m-%d")
            .to_string(),
        None => chrono::Local::now().format("%Y-%m-%d").to_string(),
    };
    json!({
        "accounts": accounts,
        "totalCredits": total_credits,
        "peakCredits": peak_credits,
        "peakDate": peak_date,
        "todayCredits": daily.get(&today_key).cloned().unwrap_or(0.0),
        "daily": daily.iter().map(|(d, v)| json!({"date": d, "value": v})).collect::<Vec<_>>(),
    })
}

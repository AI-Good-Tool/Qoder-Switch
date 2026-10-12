//! Token 统计（估算口径）
//!
//! 说明：Qoder 官方不公开逐请求真实 token 数（本地 jsonl 的 usage 恒为 0，
//! API 亦无明细端点），因此基于本地会话消息体量做估算：
//!   - 字符量：text + parts + attachments 的字符数（length）与字节数（CAST AS BLOB）
//!   - 宽字符数 ≈ (字节 - 字符) / 2（CJK 约 3 字节/字）
//!   - token ≈ ASCII/4 + 宽字符 × 0.8
//!   - 输出 = assistant 消息估算；输入 = 同一 turn 的 user 消息估算
//! 注意：sqlite3 CLI 的 -json 输出对大文本病态慢（353KB 需 4s），
//!       因此这里只取 chars/bytes 数字，绝不搬运 payload 原文。

use crate::qcore;
use crate::qsession::{db_path, sqlite_query};
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

fn days_before(n: i64) -> String {
    (chrono::Local::now() - chrono::Duration::days(n))
        .format("%Y-%m-%d")
        .to_string()
}

/// 自定义时区（分钟偏移，前端按 IANA 计算并处理夏令时）下的日期键
fn today_key(tz_off: Option<i64>) -> String {
    match tz_off {
        Some(o) => (chrono::Utc::now() + chrono::Duration::minutes(o))
            .format("%Y-%m-%d")
            .to_string(),
        None => today(),
    }
}

fn days_before_key(n: i64, tz_off: Option<i64>) -> String {
    match tz_off {
        Some(o) => (chrono::Utc::now() + chrono::Duration::minutes(o) - chrono::Duration::days(n))
            .format("%Y-%m-%d")
            .to_string(),
        None => days_before(n),
    }
}

/// 由字符数/字节数估算 token：宽字符 = (字节-字符)/2，token = ASCII/4 + 宽字符×0.8
fn est_from(chars: i64, bytes: i64) -> i64 {
    let chars = chars.max(0);
    let bytes = bytes.max(chars);
    let wide = ((bytes - chars) / 2).max(0);
    let ascii = chars - wide;
    (ascii as f64 / 4.0 + wide as f64 * 0.8) as i64
}

/// Token 统计主入口（估算口径）。tz_off：自定义时区偏移（分钟），None=跟随系统。
pub fn token_stats(edition_key: &str, tz_off: Option<i64>) -> Result<Value, String> {
    let Some(ed) = qcore::edition(edition_key) else {
        return Err("未知版本".into());
    };
    let db = db_path(ed);
    if !db.exists() {
        return Ok(json!({
            "available": false,
            "edition": edition_key,
            "label": ed.label,
            "daily": [], "projects": [], "models": [], "requests": [],
            "totals": {"today": 0, "d7": 0, "d30": 0, "total": 0},
        }));
    }

    // 自定义时区 → 用固定分钟偏移替代 'localtime'（SQLite 修饰符，负值同样合法）
    let tzmod = match tz_off {
        Some(o) => format!("'{} minutes'", o),
        None => "'localtime'".to_string(),
    };
    let sql = "SELECT datetime(m.created_at/1000,'unixepoch',__TZMOD__) AS ts, \
                date(m.created_at/1000,'unixepoch',__TZMOD__) AS d, \
                json_extract(m.payload_json,'$.role') AS role, \
                COALESCE(json_extract(m.payload_json,'$.turnId'),'') AS turn, \
                m.session_id AS sid, \
                COALESCE(NULLIF(s.model,''),'未知模型') AS model, \
                substr(COALESCE(s.title,''),1,60) AS title, \
                substr(s.cwd,-46) AS cwd, \
                COALESCE(length(json_extract(m.payload_json,'$.text')),0) \
                  + COALESCE(length(json_extract(m.payload_json,'$.parts')||''),0) \
                  + COALESCE(length(json_extract(m.payload_json,'$.attachments')||''),0) AS chars, \
                COALESCE(length(CAST(json_extract(m.payload_json,'$.text') AS BLOB)),0) \
                  + COALESCE(length(CAST(json_extract(m.payload_json,'$.parts') AS BLOB)),0) \
                  + COALESCE(length(CAST(json_extract(m.payload_json,'$.attachments') AS BLOB)),0) AS bytes, \
                COALESCE(json_extract(m.payload_json,'$.durationMs'), \
                         json_extract(m.payload_json,'$.turnMetrics.durationMs'), 0) AS dur \
         FROM chat_session_messages m JOIN chat_sessions s ON m.session_id = s.session_id \
         ORDER BY m.created_at ASC"
        .replace("__TZMOD__", &tzmod);
    let rows = sqlite_query(&db, &sql)?;

    struct Req {
        ts: String,
        d: String,
        model: String,
        title: String,
        cwd: String,
        sid: String,
        dur: i64,
        inp: i64,
        out: i64,
    }
    let mut requests: Vec<Req> = Vec::new();
    // 每个 session 内：turnId → 最近 user 消息估算（作为该 turn 的输入）
    let mut pending_input: BTreeMap<(String, String), i64> = BTreeMap::new();
    // 每日聚合：date → (requests, in, out)
    let mut daily: BTreeMap<String, (i64, i64, i64)> = BTreeMap::new();

    for r in &rows {
        let role = r["role"].as_str().unwrap_or("").to_string();
        let sid = r["sid"].as_str().unwrap_or("").to_string();
        let turn = r["turn"].as_str().unwrap_or("").to_string();
        let d = r["d"].as_str().unwrap_or("").to_string();
        if d.is_empty() {
            continue;
        }
        let est = est_from(
            r["chars"].as_i64().unwrap_or(0),
            r["bytes"].as_i64().unwrap_or(0),
        );
        if role == "user" {
            if !turn.is_empty() {
                pending_input.insert((sid.clone(), turn), est);
            }
            continue;
        }
        if role != "assistant" {
            continue;
        }
        let inp = pending_input.get(&(sid.clone(), turn)).copied().unwrap_or(0);
        requests.push(Req {
            ts: r["ts"].as_str().unwrap_or("").to_string(),
            d: d.clone(),
            model: r["model"].as_str().unwrap_or("未知模型").to_string(),
            title: r["title"].as_str().unwrap_or("").to_string(),
            cwd: r["cwd"].as_str().unwrap_or("").to_string(),
            sid,
            dur: r["dur"].as_i64().unwrap_or(0),
            inp,
            out: est,
        });
        let e = daily.entry(d).or_insert((0, 0, 0));
        e.0 += 1;
        e.1 += inp;
        e.2 += est;
    }
    requests.reverse(); // 最新在前

    // ── 每日序列 ──
    let daily: Vec<Value> = daily
        .into_iter()
        .map(|(date, (requests, inp, out))| {
            json!({"date": date, "requests": requests, "tokens": inp + out, "in": inp, "out": out})
        })
        .collect();

    // ── 汇总 ──
    let t = today_key(tz_off);
    let d7 = days_before_key(6, tz_off);
    let d30 = days_before_key(29, tz_off);
    let sum_tok = |from: &str| -> i64 {
        daily
            .iter()
            .filter(|p| p["date"].as_str().unwrap_or("") >= from)
            .map(|p| p["tokens"].as_i64().unwrap_or(0))
            .sum()
    };
    let sum_req = |from: &str| -> i64 {
        daily
            .iter()
            .filter(|p| p["date"].as_str().unwrap_or("") >= from)
            .map(|p| p["requests"].as_i64().unwrap_or(0))
            .sum()
    };
    let totals = json!({
        "today": {
            "tokens": daily.iter().find(|p| p["date"].as_str() == Some(t.as_str())).map(|p| p["tokens"].as_i64().unwrap_or(0)).unwrap_or(0),
            "requests": sum_req(&t),
        },
        "d7": {"tokens": sum_tok(&d7), "requests": sum_req(&d7)},
        "d30": {"tokens": sum_tok(&d30), "requests": sum_req(&d30)},
        "total": {
            "tokens": daily.iter().map(|p| p["tokens"].as_i64().unwrap_or(0)).sum::<i64>(),
            "requests": daily.iter().map(|p| p["requests"].as_i64().unwrap_or(0)).sum::<i64>(),
        },
    });

    // ── 按项目分布（按 token 量，含按日细分供前端按区间过滤）──
    let mut proj: BTreeMap<String, (i64, i64, BTreeMap<String, (i64, i64)>)> = BTreeMap::new();
    for q in &requests {
        let name = if q.cwd.is_empty() { "未知项目".to_string() } else { q.cwd.clone() };
        let e = proj.entry(name).or_insert((0, 0, BTreeMap::new()));
        e.0 += q.inp + q.out;
        e.1 += 1;
        let de = e.2.entry(q.d.clone()).or_insert((0, 0));
        de.0 += q.inp + q.out;
        de.1 += 1;
    }
    let daily_map = |m: &BTreeMap<String, (i64, i64)>| -> Value {
        Value::Object(m.iter().map(|(d, (t, r))| {
            (d.clone(), json!({"tokens": t, "requests": r}))
        }).collect())
    };
    let mut projects: Vec<Value> = proj
        .into_iter()
        .map(|(name, (tokens, requests, dm))| {
            json!({"name": name, "tokens": tokens, "requests": requests, "daily": daily_map(&dm)})
        })
        .collect();
    projects.sort_by(|a, b| b["tokens"].as_i64().unwrap_or(0).cmp(&a["tokens"].as_i64().unwrap_or(0)));
    projects.truncate(12);

    // ── 按模型分布（按 token 量，含按日细分）──
    let mut mdl: BTreeMap<String, (i64, i64, BTreeMap<String, (i64, i64)>)> = BTreeMap::new();
    for q in &requests {
        let e = mdl.entry(q.model.clone()).or_insert((0, 0, BTreeMap::new()));
        e.0 += q.inp + q.out;
        e.1 += 1;
        let de = e.2.entry(q.d.clone()).or_insert((0, 0));
        de.0 += q.inp + q.out;
        de.1 += 1;
    }
    let mut models: Vec<Value> = mdl
        .into_iter()
        .map(|(name, (tokens, requests, dm))| {
            json!({"name": name, "tokens": tokens, "requests": requests, "daily": daily_map(&dm)})
        })
        .collect();
    models.sort_by(|a, b| b["tokens"].as_i64().unwrap_or(0).cmp(&a["tokens"].as_i64().unwrap_or(0)));
    models.truncate(12);

    // ── 请求明细（最近 300 条）──
    let reqs: Vec<Value> = requests
        .iter()
        .take(300)
        .map(|q| {
            json!({
                "ts": q.ts, "model": q.model, "title": q.title, "cwd": q.cwd,
                "sessionId": q.sid, "durationMs": q.dur,
                "inTokens": q.inp, "outTokens": q.out,
            })
        })
        .collect();

    Ok(json!({
        "available": true,
        "edition": edition_key,
        "label": ed.label,
        "daily": daily,
        "totals": totals,
        "projects": projects,
        "models": models,
        "requests": reqs,
    }))
}

//! Persistent numeric gateway usage; never store prompts, credentials or errors.
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::Path,
    sync::Mutex,
};
static IO: Mutex<()> = Mutex::new(());
fn path() -> std::path::PathBuf {
    crate::qcore::switch_dir().join("gateway/usage.jsonl")
}
pub fn record(log: &crate::qproxy::RequestLog) -> Result<(), String> {
    append(
        &path(),
        &json!({"id":log.id,"time":log.time,"edition":log.edition,"model":log.model,"realModel":log.real_model,"modelName":log.model_name,"requestedModel":log.requested_model,"attempt":log.attempt,"clientRequestId":log.client_request_id,"upstreamCode":log.upstream_code,"account":log.account,"protocol":log.protocol,"status":log.status,"durationMs":log.duration_ms,
        "in":log.usage["prompt_tokens"].as_u64(),"out":log.usage["completion_tokens"].as_u64(),"cached":log.usage["prompt_tokens_details"]["cached_tokens"].as_u64()}),
    )
}
fn append(path: &Path, row: &Value) -> Result<(), String> {
    let _lock = IO.lock().map_err(|e| e.to_string())?;
    fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path).map_err(|e| e.to_string())?;
    let mut bytes = serde_json::to_vec(row).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    file.write_all(&bytes)
        .and_then(|_| file.sync_data())
        .map_err(|e| e.to_string())
}
pub fn stats(edition: &str, offset: Option<i64>) -> Result<Value, String> {
    read_stats(&path(), edition, offset)
}
fn read_stats(path: &Path, edition: &str, offset: Option<i64>) -> Result<Value, String> {
    let _lock = IO.lock().map_err(|e| e.to_string())?;
    let file = match fs::File::open(path) {
        Ok(f) => Some(f),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e.to_string()),
    };
    let mut daily: BTreeMap<String, Value> = BTreeMap::new();
    let mut groups: [BTreeMap<String, Value>; 2] = Default::default();
    let mut requests = Vec::new();
    let mut seen = HashSet::new();
    if let Some(file) = file {
        for line in BufReader::new(file).lines() {
            let line = line.map_err(|e| e.to_string())?;
            let row: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if row["edition"] != edition
                || !seen.insert(row["id"].as_str().unwrap_or("").to_string())
            {
                continue;
            }
            let time =
                match chrono::DateTime::parse_from_rfc3339(row["time"].as_str().unwrap_or("")) {
                    Ok(t) => t,
                    Err(_) => continue,
                };
            let ts = match offset {
                Some(o) => (time.with_timezone(&chrono::Utc)
                    + chrono::Duration::minutes(o.clamp(-1440, 1440)))
                .format("%Y-%m-%d %H:%M:%S")
                .to_string(),
                None => time
                    .with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string(),
            };
            let date = ts[..10].to_string();
            let inp = row["in"].as_u64().unwrap_or(0);
            let out = row["out"].as_u64().unwrap_or(0);
            let tokens = inp + out;
            let d = daily
                .entry(date.clone())
                .or_insert(json!({"date":date,"tokens":0,"in":0,"out":0,"requests":0}));
            for (k, n) in [
                ("tokens", tokens),
                ("in", inp),
                ("out", out),
                ("requests", 1),
            ] {
                d[k] = json!(d[k].as_u64().unwrap_or(0) + n);
            }
            for (i, key) in ["account", "model"].iter().enumerate() {
                let name = row[key].as_str().unwrap_or("未知");
                let g = groups[i]
                    .entry(name.into())
                    .or_insert(json!({"name":name,"tokens":0,"requests":0,"daily":{}}));
                g["tokens"] = json!(g["tokens"].as_u64().unwrap_or(0) + tokens);
                g["requests"] = json!(g["requests"].as_u64().unwrap_or(0) + 1);
                let prev = g["daily"][&date].clone();
                g["daily"][&date] = json!({"tokens":prev["tokens"].as_u64().unwrap_or(0)+tokens,"requests":prev["requests"].as_u64().unwrap_or(0)+1});
            }
            requests.push(json!({"ts":ts,"model":row["model"],"realModel":row["realModel"],"modelName":row["modelName"],"cwd":row["account"],"title":format!("{} · {}",row["protocol"].as_str().unwrap_or(""),row["status"].as_str().unwrap_or("")),"durationMs":row["durationMs"],"inTokens":row["in"],"outTokens":row["out"],"cachedTokens":row["cached"]}));
        }
    }
    requests.sort_by(|a, b| b["ts"].as_str().cmp(&a["ts"].as_str()));
    let distributions: Vec<Vec<Value>> = groups
        .into_iter()
        .map(|g| {
            let mut v: Vec<_> = g.into_values().collect();
            v.sort_by_key(|x| std::cmp::Reverse(x["tokens"].as_u64().unwrap_or(0)));
            v
        })
        .collect();
    Ok(
        json!({"available":true,"source":"proxy","edition":edition,"daily":daily.into_values().collect::<Vec<_>>(),"projects":distributions[0],"models":distributions[1],"requests":requests}),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persists_filters_deduplicates_and_handles_missing_usage() {
        let dir = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let p = dir.join("usage.jsonl");
        let row = json!({"id":"one","time":"2026-10-20T20:00:00Z","edition":"cn","model":"GLM","account":"A","protocol":"messages","status":"completed","in":100,"out":20,"cached":50});
        append(&p, &row).unwrap();
        append(&p, &row).unwrap();
        append(&p,&json!({"id":"two","time":"2026-10-20T20:01:00Z","edition":"cn","model":"GLM","status":"failed"})).unwrap();
        let d = read_stats(&p, "cn", Some(480)).unwrap();
        assert_eq!(d["daily"][0]["date"], "2026-10-21");
        assert_eq!(d["daily"][0]["tokens"], 120);
        assert_eq!(d["daily"][0]["requests"], 2);
        assert!(d["requests"][0]["inTokens"].is_null());
        assert_eq!(
            read_stats(&p, "intl", None).unwrap()["requests"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }
}

//! Loopback-only Qoder gateway. Official tokens stay in the backend.
use crate::{anthropic, qcore, response_tools::ToolMap, responses};
use axum::{
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Request, State},
    http::{HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fs,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{io::AsyncWriteExt, sync::oneshot};

const MAX_BODY: usize = 32 * 1024 * 1024;
const MAX_OUTPUT: usize = 16 * 1024 * 1024;
const MAX_EVENT: usize = 2 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub port: u16,
    pub edition: String,
    pub transport: String,
    pub cn_endpoint: String,
    pub intl_endpoint: String,
    pub disabled_accounts: Vec<String>,
    pub strategy: String,
    pub model_routes: BTreeMap<String, crate::qroutes::ModelRoute>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            port: 24818,
            edition: "both".into(),
            transport: "legacy".into(),
            cn_endpoint: "https://gateway.qoder.com.cn".into(),
            intl_endpoint: "https://api2.qoder.sh".into(),
            disabled_accounts: vec![],
            strategy: "roundRobin".into(),
            model_routes: BTreeMap::new(),
        }
    }
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestLog {
    pub id: String,
    pub time: String,
    pub protocol: String,
    pub model: String,
    pub requested_model: String,
    pub real_model: String,
    pub model_name: String,
    pub account: String,
    pub edition: String,
    pub status: String,
    pub http_status: u16,
    pub duration_ms: u128,
    pub first_byte_ms: Option<u128>,
    pub usage: Value,
    pub message: String,
    pub upstream_code: Option<String>,
    pub request_id: String,
}
#[derive(Default)]
struct Runtime {
    running: bool,
    shutdown: Option<oneshot::Sender<()>>,
    active: HashMap<String, usize>,
    cooldown: HashMap<String, Instant>,
    cursors: HashMap<String, usize>,
    logs: VecDeque<RequestLog>,
    models: HashMap<String, Vec<Value>>,
    model_updated: HashMap<String, String>,
    error: String,
}
pub struct Gateway {
    #[cfg(test)]
    fixture_accounts: Option<Vec<qcore::Account>>,
    config: Mutex<Config>,
    key: Mutex<String>,
    runtime: Mutex<Runtime>,
    client: reqwest::Client,
}
fn data_file(name: &str) -> PathBuf {
    qcore::switch_dir().join("gateway").join(name)
}
fn write_private(path: &PathBuf, data: &[u8]) -> Result<(), String> {
    use std::io::Write;
    fs::create_dir_all(path.parent().unwrap()).map_err(|_| "无法创建网关数据目录")?;
    let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&temp).map_err(|_| "无法保存网关配置")?;
    file.write_all(data).map_err(|_| "无法写入网关配置")?;
    file.sync_all().map_err(|_| "无法同步网关配置")?;
    fs::rename(temp, path).map_err(|_| "无法更新网关配置".into())
}
fn random_key() -> String {
    format!(
        "qs_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}
fn validate_key(key: &str) -> Result<(), String> {
    if !(32..=256).contains(&key.len()) || !key.bytes().all(|b| b.is_ascii_graphic()) {
        return Err("API Key 需为 32–256 位 ASCII 可见字符，不能包含空格或换行".into());
    }
    Ok(())
}
fn valid_endpoint(s: &str) -> bool {
    let Ok(url) = reqwest::Url::parse(s) else {
        return false;
    };
    let host = url.host_str().unwrap_or("");
    url.scheme() == "https"
        && (host.ends_with(".qoder.sh")
            || host.ends_with(".qoder.com.cn")
            || host.ends_with(".qoder.com"))
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() == "/"
}
impl Gateway {
    pub fn new() -> Result<Arc<Self>, String> {
        let config: Config = fs::read(data_file("settings.json"))
            .ok()
            .and_then(|v| serde_json::from_slice(&v).ok())
            .unwrap_or_default();
        validate_config(&config)?;
        let key_path = data_file("api-key");
        let key = match fs::read_to_string(&key_path) {
            Ok(s) if s.trim().len() >= 32 => s.trim().to_owned(),
            _ => {
                let s = random_key();
                write_private(&key_path, s.as_bytes())?;
                s
            }
        };
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(180))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| "无法初始化网络客户端")?;
        let mut runtime = Runtime::default();
        for ed in ["cn", "intl"] {
            if let Ok(bytes) = fs::read(data_file(&format!("models-{ed}.json"))) {
                if let Ok(items) = serde_json::from_slice::<Vec<Value>>(&bytes) {
                    runtime.models.insert(ed.into(), items);
                }
            }
        }
        Ok(Arc::new(Self {
            #[cfg(test)]
            fixture_accounts: None,
            config: Mutex::new(config),
            key: Mutex::new(key),
            runtime: Mutex::new(runtime),
            client,
        }))
    }
    fn api_key(&self) -> String {
        self.key.lock().unwrap().clone()
    }
    fn set_key_at(&self, key: &str, path: &PathBuf) -> Result<(), String> {
        validate_key(key)?;
        let mut current = self.key.lock().unwrap();
        write_private(path, key.as_bytes())?;
        *current = key.to_owned();
        Ok(())
    }
    fn accounts(&self) -> Vec<qcore::Account> {
        #[cfg(test)]
        if let Some(accounts) = &self.fixture_accounts {
            return accounts.clone();
        }
        qcore::load_accounts()
    }
    pub fn snapshot(&self) -> Value {
        let c = self.config.lock().unwrap().clone();
        let r = self.runtime.lock().unwrap();
        let models = crate::qroutes::catalog(&r.models, &c.model_routes, &c.edition);
        let accounts: Vec<Value> = self.accounts().iter().map(|a| json!({"id":a.id(),"enabled":!c.disabled_accounts.contains(&a.id()),"active":r.active.get(&a.id()).copied().unwrap_or(0),"cooldownSeconds":r.cooldown.get(&a.id()).map(|t|t.saturating_duration_since(Instant::now()).as_secs()).unwrap_or(0)})).collect();
        let logs: Vec<_> = r.logs.iter().rev().collect();
        json!({"config":c,"apiKey":self.api_key(),"running":r.running,"active":r.active.values().sum::<usize>(),"accounts":accounts,"models":models,"catalogs":r.models,"modelUpdated":r.model_updated.values().max(),"catalogUpdated":r.model_updated,"logs":logs,"error":r.error})
    }
    pub fn save(&self, c: Config) -> Result<(), String> {
        validate_config(&c)?;
        let mut current = self.config.lock().unwrap();
        if self.runtime.lock().unwrap().running
            && (current.transport != c.transport
                || current.port != c.port
                || current.cn_endpoint != c.cn_endpoint
                || current.intl_endpoint != c.intl_endpoint)
        {
            return Err("请先停止服务再修改端口、传输协议或上游地址".into());
        }
        crate::qroutes::validate_aliases(&crate::qroutes::catalog(
            &self.runtime.lock().unwrap().models,
            &c.model_routes,
            "both",
        ))?;
        write_private(
            &data_file("settings.json"),
            &serde_json::to_vec_pretty(&c).unwrap(),
        )?;
        *current = c;
        Ok(())
    }
    pub async fn start(self: &Arc<Self>) -> Result<(), String> {
        let port = {
            let _c = self.config.lock().unwrap();
            let mut r = self.runtime.lock().unwrap();
            if r.running {
                return Ok(());
            }
            r.running = true;
            r.error.clear();
            _c.port
        };
        let listener =
            match tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await {
                Ok(l) => l,
                Err(_) => {
                    let mut r = self.runtime.lock().unwrap();
                    r.running = false;
                    r.error = format!("端口 {port} 已占用或无法监听");
                    return Err(r.error.clone());
                }
            };
        let (tx, rx) = oneshot::channel();
        self.runtime.lock().unwrap().shutdown = Some(tx);
        let app = router(self.clone());
        let state = self.clone();
        tauri::async_runtime::spawn(async move {
            let result = axum::serve(listener, app)
                .with_graceful_shutdown(async {
                    let _ = rx.await;
                })
                .await;
            let mut r = state.runtime.lock().unwrap();
            r.running = false;
            r.shutdown = None;
            if result.is_err() {
                r.error = "本地 HTTP 服务意外退出".into();
            }
        });
        Ok(())
    }
    pub fn stop(&self) {
        if let Some(tx) = self.runtime.lock().unwrap().shutdown.take() {
            let _ = tx.send(());
        }
    }
    pub fn clear_logs(&self) {
        self.runtime.lock().unwrap().logs.clear();
    }
    fn log(&self, log: RequestLog) {
        #[cfg(not(test))]
        if let Err(e) = crate::qusage::record(&log) {
            self.runtime.lock().unwrap().error = format!("反代用量保存失败：{e}");
        }
        let mut r = self.runtime.lock().unwrap();
        r.logs.push_back(log);
        while r.logs.len() > 200 {
            r.logs.pop_front();
        }
    }
    pub async fn refresh_models(self: &Arc<Self>) -> Result<Value, String> {
        let mode = self.config.lock().unwrap().edition.clone();
        let editions = if mode == "both" {
            vec!["cn", "intl"]
        } else {
            vec![mode.as_str()]
        };
        let mut synced = Vec::new();
        let mut warnings = Vec::new();
        let mut count = 0;
        for edition in editions {
            match self.refresh_region_models(edition).await {
                Ok(v) => {
                    count += v["count"].as_u64().unwrap_or(0);
                    synced.push(edition.to_owned());
                }
                Err(e) => warnings.push(format!("{edition}: {e}")),
            }
        }
        if synced.is_empty() {
            return Err(warnings.join("；"));
        }
        Ok(json!({"count":count,"synced":synced,"warnings":warnings}))
    }
    async fn refresh_region_models(self: &Arc<Self>, edition: &str) -> Result<Value, String> {
        let mut c = self.config.lock().unwrap().clone();
        c.edition = edition.into();
        let ed = c.edition.clone();
        let disabled = c.disabled_accounts.clone();
        let account = tokio::task::spawn_blocking(move || {
            qcore::load_accounts()
                .into_iter()
                .find(|a| a.edition == ed && !disabled.contains(&a.id()) && !a.token.is_empty())
        })
        .await
        .map_err(|_| "读取账号失败")?
        .ok_or("请先添加或采集对应地区账号")?;
        let endpoint = if c.edition == "cn" {
            "https://gateway.qoder.com.cn"
        } else {
            "https://api2.qoder.sh"
        };
        let machine = machine_id(&c.edition);
        let prepared = runtime_helper(json!({"op":"prepare_models","edition":c.edition,"uid":account.uid,"token":account.token,"machineId":machine,"endpoint":endpoint})).await?;
        let url = prepared["url"].as_str().ok_or("模型目录地址缺失")?;
        // Only the installed runtime may choose an official HTTPS endpoint.
        let parsed = reqwest::Url::parse(url).map_err(|_| "模型目录地址无效")?;
        if !valid_endpoint(parsed.origin().ascii_serialization().as_str()) {
            return Err("运行时返回非官方目录地址".into());
        }
        let mut req = self.client.get(url);
        for (k, v) in prepared["headers"].as_object().ok_or("目录鉴权头缺失")? {
            if let Some(v) = v.as_str() {
                req = req.header(k, v);
            }
        }
        let response = req.send().await.map_err(|_| "模型目录连接失败")?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(format!("模型目录查询失败，HTTP {status}；请刷新账号后重试"));
        }
        let bytes = bounded_body(response, 8 * 1024 * 1024).await?;
        let text = String::from_utf8(bytes).map_err(|_| "模型目录编码无效")?;
        let raw = runtime_helper(json!({"op":"decrypt","edition":c.edition,"text":text})).await?;
        let models = normalize_models(&raw)?;
        write_private(
            &data_file(&format!("models-{}.json", c.edition)),
            &serde_json::to_vec_pretty(&models).unwrap(),
        )?;
        let mut r = self.runtime.lock().unwrap();
        r.models.insert(c.edition.clone(), models.clone());
        r.model_updated
            .insert(c.edition, chrono::Utc::now().to_rfc3339());
        Ok(json!({"count":models.len(),"models":models}))
    }
}
fn validate_config(c: &Config) -> Result<(), String> {
    if !["legacy", "modelServer"].contains(&c.transport.as_str())
        || c.port == 0
        || !["cn", "intl", "both"].contains(&c.edition.as_str())
        || !["roundRobin", "expiryFirst"].contains(&c.strategy.as_str())
        || !valid_endpoint(&c.cn_endpoint)
        || !valid_endpoint(&c.intl_endpoint)
    {
        return Err("配置无效：请选择有效端口、地区、调度策略和 Qoder 官方 HTTPS 地址".into());
    }
    Ok(())
}
fn machine_id(edition: &str) -> String {
    if let Some(ed) = qcore::edition(edition) {
        for path in [
            qcore::home_dir().join(ed.data_dir).join("auth.machine-id"),
            qcore::home_dir().join(ed.home_dir).join(".auth/machine_id"),
        ] {
            if let Ok(value) = fs::read_to_string(path) {
                if !value.trim().is_empty() {
                    return value.trim().to_owned();
                }
            }
        }
    }
    let folder = if edition == "cn" {
        ".qoder-cn"
    } else {
        ".qoder"
    };
    fs::read_to_string(qcore::home_dir().join(folder).join("installation_id"))
        .unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
        .trim()
        .to_owned()
}
async fn runtime_helper(input: Value) -> Result<Value, String> {
    let path = data_file("runtime-helper.mjs");
    if !path.exists() {
        write_private(&path, include_bytes!("../../scripts/qoder-runtime.mjs"))?;
    }
    // Keep the helper in sync when upgrading the app.
    if fs::read(&path).ok().as_deref() != Some(include_bytes!("../../scripts/qoder-runtime.mjs")) {
        write_private(&path, include_bytes!("../../scripts/qoder-runtime.mjs"))?;
    }
    let mut candidates = vec![
        PathBuf::from("/opt/homebrew/bin/node"),
        PathBuf::from("/usr/local/bin/node"),
    ];
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|p| p.join("node")));
    }
    let nvm = qcore::home_dir().join(".nvm/versions/node");
    if let Ok(entries) = fs::read_dir(nvm) {
        for e in entries.flatten() {
            candidates.push(e.path().join("bin/node"));
        }
    }
    let bundled = PathBuf::from(if input["edition"] == "cn" {
        "/Applications/Qoder CN.app/Contents/MacOS/Qoder CN"
    } else {
        "/Applications/Qoder.app/Contents/MacOS/Qoder"
    });
    let executable = if bundled.is_file() {
        bundled
    } else {
        candidates
            .into_iter()
            .find(|p| p.is_file())
            .ok_or("请安装 Qoder 或 Node.js")?
    };
    let mut child = tokio::process::Command::new(executable)
        .arg(path)
        .env("ELECTRON_RUN_AS_NODE", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| "无法启动 Qoder 协议运行时，请安装 Qoder 或 Node.js")?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&input).unwrap())
        .await
        .map_err(|_| "无法发送本地协议请求")?;
    let output = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .map_err(|_| "本地协议运行时超时")?
        .map_err(|_| "本地协议运行时失败")?;
    let v: Value = serde_json::from_slice(&output.stdout).map_err(|_| "本地协议运行时响应无效")?;
    if v["ok"] != true {
        return Err("Qoder 本地协议运行时不可用，请确认对应版本 Qoder 已安装且兼容".into());
    }
    Ok(v["data"].clone())
}
fn normalize_models(raw: &Value) -> Result<Vec<Value>, String> {
    let mut rows = Vec::new();
    if let Some(a) = raw.as_array() {
        rows.extend(a.iter());
    } else if let Some(a) = raw["assistant"]
        .as_array()
        .or_else(|| raw["data"]["assistant"].as_array())
        .or_else(|| raw["qodercli"].as_array())
        .or_else(|| raw["data"]["qodercli"].as_array())
        .or_else(|| raw["models"].as_array())
        .or_else(|| raw["data"].as_array())
    {
        rows.extend(a.iter());
    } else if let Some(o) = raw.as_object() {
        for v in o.values() {
            if let Some(a) = v.as_array() {
                rows.extend(a.iter());
            }
        }
    }
    let mut models = Vec::new();
    let mut ids = std::collections::HashSet::new();
    for m in rows {
        let Some(id) = m["key"]
            .as_str()
            .or_else(|| m["id"].as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        if !ids.insert(id.to_owned()) {
            continue;
        }
        models.push(json!({"id":id,"name":m["display_name"].as_str().or_else(||m["name"].as_str()).unwrap_or(id),"supportsTools":m["supports_tool_call"].as_bool().or(m["support_tool_call"].as_bool()),"supportsImages":m["is_vl"].as_bool(),"reasoning":model_reasoning(m),"contextWindow":m["max_input_tokens"].as_u64().or(m["context_length"].as_u64()),"isReasoning":m["is_reasoning"].as_bool().unwrap_or(false),"format":m["format"].as_str().unwrap_or("openai"),"verified":false}));
    }
    if models.is_empty() {
        return Err("模型目录未返回可识别的模型；请检查账号权限与客户端版本".into());
    }
    Ok(models)
}
fn model_reasoning(model: &Value) -> Value {
    let mut efforts = Vec::new();
    let mut default = None;
    if let Some(o) = model["thinking_config"]["enabled"]["efforts"].as_object() {
        for (effort, entry) in o {
            efforts.push(effort.clone());
            if entry["is_default"] == true {
                default = Some(effort.clone());
            }
        }
    }
    if model["thinking_config"].get("disabled").is_some() {
        efforts.push("none".into());
    }
    json!({"supportedEfforts":efforts,"defaultEffort":default})
}
async fn bounded_body(response: reqwest::Response, limit: usize) -> Result<Vec<u8>, String> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(part) = stream.next().await {
        let part = part.map_err(|_| "上游响应读取失败")?;
        if bytes.len() + part.len() > limit {
            return Err("上游响应超过本地大小限制".into());
        }
        bytes.extend_from_slice(&part);
    }
    Ok(bytes)
}

pub fn router(state: Arc<Gateway>) -> Router {
    let mut routes = Router::new().route("/v1/chat/completions", post(chat));
    for prefix in ["", "/v1", "/v1/v1"] {
        routes = routes
            .route(&format!("{prefix}/models"), get(models))
            .route(&format!("{prefix}/messages"), post(messages))
            .route(
                &format!("{prefix}/messages/count_tokens"),
                post(count_tokens),
            )
            .route(&format!("{prefix}/responses"), post(response_api));
    }
    routes
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .layer(middleware::from_fn_with_state(state.clone(), authenticate))
        .with_state(state)
}
async fn authenticate(State(g): State<Arc<Gateway>>, req: Request, next: Next) -> Response {
    let bearer = req
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "));
    let key = req.headers().get("x-api-key").and_then(|h| h.to_str().ok());
    let current = g.api_key();
    if bearer != Some(current.as_str()) && key != Some(current.as_str()) {
        return error(
            401,
            "本地 API Key 无效",
            req.uri().path().contains("messages"),
        );
    }
    if req.headers().contains_key("origin") {
        return error(403, "不接受网页跨域请求", false);
    }
    next.run(req).await
}
fn error(status: u16, message: &str, anthropic: bool) -> Response {
    let v = if anthropic {
        anthropic::error_value(status, message)
    } else {
        json!({"error":{"message":message,"type":"gateway_error","code":status}})
    };
    (StatusCode::from_u16(status).unwrap(), Json(v)).into_response()
}
// Preserve official display names at the public boundary; never persist aliases as upstream IDs.
pub(crate) fn public_catalog(models: &[Value]) -> Vec<Value> {
    models
        .iter()
        .map(|model| {
            let id = model["id"].as_str().unwrap_or("");
            let name = model["name"]
                .as_str()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or(id);
            let ambiguous = models.iter().any(|other| {
                other["id"] != model["id"]
                    && (other["name"].as_str() == Some(name) || other["id"].as_str() == Some(name))
            });
            let mut model = model.clone();
            model["publicId"] = json!(if ambiguous {
                format!("{name} [{id}]")
            } else {
                name.to_owned()
            });
            model
        })
        .collect()
}
fn resolve_model(models: &[Value], requested: &str) -> Option<Value> {
    // Internal IDs take precedence for backward compatibility, including renamed models.
    models
        .iter()
        .find(|m| m["id"] == requested)
        .cloned()
        .or_else(|| {
            public_catalog(models)
                .into_iter()
                .find(|m| m["publicId"] == requested)
        })
}
async fn models(State(g): State<Arc<Gateway>>) -> Response {
    let c = g.config.lock().unwrap().clone();
    let r = g.runtime.lock().unwrap();
    let rows:Vec<_>=crate::qroutes::catalog(&r.models,&c.model_routes,&c.edition).into_iter().filter(|m|m["available"]==true).map(|m|json!({"id":m["publicId"],"name":m["publicId"],"display_name":m["publicId"],"qoder_id":m["targets"][0]["id"],"upstreams":m["targets"].as_array().unwrap().iter().map(|t|json!({"edition":t["edition"],"id":t["id"],"name":t["name"]})).collect::<Vec<_>>(),"object":"model","type":"model","created":0,"created_at":"1970-01-01T00:00:00Z","owned_by":"qoder"})).collect();
    Json(json!({"object":"list","first_id":rows.first().map(|m|&m["id"]),"last_id":rows.last().map(|m|&m["id"]),"has_more":false,"data":rows})).into_response()
}
async fn count_tokens(State(_g): State<Arc<Gateway>>, Json(mut input): Json<Value>) -> Response {
    if input.get("max_tokens").is_none() {
        input["max_tokens"] = json!(1);
    }
    match anthropic::to_chat(&input) {
        Ok(v) => {
            let n = v["messages"].to_string().chars().count().div_ceil(3) + 16;
            (
                [("x-token-count-method", "estimate")],
                Json(json!({"input_tokens":n})),
            )
                .into_response()
        }
        Err(e) => error(400, &e, true),
    }
}
#[derive(Clone, Copy)]
enum Protocol {
    Chat,
    Messages,
    Responses,
}
impl Protocol {
    fn label(self) -> &'static str {
        match self {
            Self::Chat => "chat-completions",
            Self::Messages => "anthropic-messages",
            Self::Responses => "openai-responses",
        }
    }
}
async fn chat(State(g): State<Arc<Gateway>>, h: HeaderMap, Json(v): Json<Value>) -> Response {
    run(g, h, v, Protocol::Chat).await
}
async fn messages(State(g): State<Arc<Gateway>>, h: HeaderMap, Json(v): Json<Value>) -> Response {
    run(g, h, v, Protocol::Messages).await
}
async fn response_api(
    State(g): State<Arc<Gateway>>,
    h: HeaderMap,
    Json(v): Json<Value>,
) -> Response {
    run(g, h, v, Protocol::Responses).await
}
struct Lease {
    g: Arc<Gateway>,
    id: String,
    log: Option<RequestLog>,
    start: Instant,
}
impl Lease {
    fn finish(&mut self, status: &str, http: u16, message: &str, usage: Value) {
        if let Some(mut log) = self.log.take() {
            log.status = status.into();
            log.http_status = http;
            log.message = message.into();
            if !usage.is_null() {
                log.usage = usage;
            }
            log.duration_ms = self.start.elapsed().as_millis();
            self.g.log(log);
        }
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.finish("interrupted", 499, "客户端断开或请求取消", Value::Null);
        let mut r = self.g.runtime.lock().unwrap();
        if let Some(n) = r.active.get_mut(&self.id) {
            *n = n.saturating_sub(1);
        }
    }
}
fn expired(a: &qcore::Account) -> bool {
    chrono::DateTime::parse_from_rfc3339(&a.expiresAt)
        .map(|t| t.timestamp() <= chrono::Utc::now().timestamp())
        .unwrap_or(false)
}
async fn select(
    g: Arc<Gateway>,
    headers: &HeaderMap,
    p: Protocol,
    requested: &str,
    route: &Value,
) -> Result<(qcore::Account, Lease, Config, Value), String> {
    let reader = g.clone();
    let all = tokio::task::spawn_blocking(move || reader.accounts())
        .await
        .map_err(|_| "读取账号失败")?;
    let mut config = g.config.lock().unwrap().clone();
    let mut r = g.runtime.lock().unwrap();
    let pinned = headers.get("x-account-id").and_then(|v| v.to_str().ok());
    let targets = route["targets"].as_array().unwrap();
    let mut eligible: Vec<_> = all
        .into_iter()
        .enumerate()
        .filter(|(_, a)| {
            targets.iter().any(|t| t["edition"] == a.edition)
                && !a.token.is_empty()
                && !expired(a)
                && !config.disabled_accounts.contains(&a.id())
                && pinned.is_none_or(|id| id == a.id())
                && r.active.get(&a.id()).copied().unwrap_or(0) < 2
                && r.cooldown.get(&a.id()).is_none_or(|t| *t <= Instant::now())
        })
        .collect();
    if eligible.is_empty() {
        return Err("没有可用账号：请检查地区、有效期、停用状态、冷却或单账号并发上限（2）".into());
    }
    if config.strategy == "expiryFirst" {
        eligible.sort_by_key(|(_, a)| {
            chrono::DateTime::parse_from_rfc3339(&a.expiresAt)
                .map(|t| t.timestamp())
                .unwrap_or(i64::MAX)
        });
    }
    let route_id = route["routeId"].as_str().unwrap();
    let cursor = r.cursors.get(route_id).copied().unwrap_or(0);
    let index = if config.strategy == "roundRobin" {
        eligible.iter().position(|(i, _)| *i >= cursor).unwrap_or(0)
    } else {
        0
    };
    let (position, account) = eligible.remove(index);
    r.cursors
        .insert(route_id.into(), position.saturating_add(1));
    let model_info = targets
        .iter()
        .find(|t| t["edition"] == account.edition)
        .unwrap()
        .clone();
    config.edition = account.edition.clone();
    let id = account.id();
    *r.active.entry(id.clone()).or_default() += 1;
    drop(r);
    let request_id = uuid::Uuid::new_v4().to_string();
    let log = RequestLog {
        id: request_id.clone(),
        time: chrono::Utc::now().to_rfc3339(),
        protocol: p.label().into(),
        model: route["publicId"].as_str().unwrap().into(),
        requested_model: requested.into(),
        real_model: model_info["id"].as_str().unwrap().into(),
        model_name: model_info["name"]
            .as_str()
            .or(model_info["id"].as_str())
            .unwrap()
            .into(),
        account: if account.note.is_empty() {
            account.name.clone()
        } else {
            account.note.clone()
        },
        edition: account.edition.clone(),
        status: "running".into(),
        http_status: 0,
        duration_ms: 0,
        first_byte_ms: None,
        usage: Value::Null,
        message: String::new(),
        upstream_code: None,
        request_id,
    };
    Ok((
        account,
        Lease {
            g,
            id,
            log: Some(log),
            start: Instant::now(),
        },
        config,
        model_info,
    ))
}
async fn run(g: Arc<Gateway>, headers: HeaderMap, input: Value, protocol: Protocol) -> Response {
    let is_anthropic = matches!(protocol, Protocol::Messages);
    let mut tools = ToolMap::default();
    let converted = match protocol {
        Protocol::Messages => anthropic::to_chat(&input),
        Protocol::Responses => responses::to_chat_with_tools(&input).map(|(v, t)| {
            tools = t;
            v
        }),
        Protocol::Chat => responses::normalize_chat(input.clone()),
    };
    let mut body = match converted {
        Ok(v) => v,
        Err(e) => return error(400, &e, is_anthropic),
    };
    let Some(model) = body["model"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
    else {
        return error(400, "model 必须为非空字符串", is_anthropic);
    };
    if !body["messages"].as_array().is_some_and(|a| !a.is_empty()) {
        return error(400, "messages 必须为非空数组", is_anthropic);
    }
    let route = {
        let config = g.config.lock().unwrap();
        let runtime = g.runtime.lock().unwrap();
        crate::qroutes::resolve(
            &crate::qroutes::catalog(&runtime.models, &config.model_routes, &config.edition),
            &model,
        )
    };
    let Some(route) = route else {
        return error(
            400,
            "模型或别名不存在（内部 ID 有歧义时请使用别名），请同步模型目录",
            is_anthropic,
        );
    };
    let (account, mut lease, config, model_info) =
        match select(g.clone(), &headers, protocol, &model, &route).await {
            Ok(v) => v,
            Err(e) => return error(503, &e, is_anthropic),
        };
    let model = route["publicId"].as_str().unwrap().to_owned();
    let upstream_model = model_info["id"].as_str().unwrap().to_owned();
    body["model"] = json!(upstream_model);
    if is_anthropic {
        anthropic::apply_effort(&input, &mut body, &model_info);
    }
    if let Some(effort) = body["reasoning_effort"].as_str() {
        if model_info["reasoning"]["supportedEfforts"]
            .as_array()
            .is_some_and(|a| !a.is_empty() && !a.iter().any(|v| v == effort))
        {
            let e = "当前模型不支持所选思考档位，请在模型目录查看支持的档位";
            lease.finish("failed", 400, e, Value::Null);
            return error(400, e, is_anthropic);
        }
    }
    normalize_qoder_messages(&mut body);
    let downstream_stream = body["stream"].as_bool().unwrap_or(false);
    body["stream"] = json!(true);
    body["stream_options"] = json!({"include_usage":true});
    let request_id = lease.log.as_ref().unwrap().request_id.clone();
    let session = headers
        .get("x-session-id")
        .or_else(|| headers.get("x-conversation-id"))
        .and_then(|v| v.to_str().ok())
        .filter(|s| s.len() <= 256)
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    body["metadata"] = json!({"context":{"request_id":request_id,"request_set_id":request_id,"session_id":session,"client_type":"qodercli"},"business":{"id":request_id,"name":"chat","product":"Qoder","version":"1.1.66","type":"cli","begin_at":chrono::Utc::now().timestamp_millis()}});
    if body.to_string().len() > MAX_BODY {
        lease.finish("failed", 413, "转换后请求超过 32 MiB", Value::Null);
        return error(413, "转换后请求超过 32 MiB", is_anthropic);
    }
    let endpoint = if config.edition == "cn" {
        &config.cn_endpoint
    } else {
        &config.intl_endpoint
    };
    let request = if config.transport == "legacy" {
        let legacy = legacy_body(&body, &model_info, &request_id, &session, &config.edition);
        let prepared = match runtime_helper(json!({"op":"prepare_chat","edition":config.edition,"uid":account.uid,"token":account.token,"machineId":machine_id(&config.edition),"endpoint":endpoint,"body":legacy,"model":upstream_model})).await {
            Ok(v)=>v,Err(e)=>{lease.finish("failed",502,&e,Value::Null);return error(502,&e,is_anthropic)}
        };
        let Some(url) = prepared["url"].as_str() else {
            return error(502, "推理地址缺失", is_anthropic);
        };
        if prepared["body"].as_str().unwrap_or("").len() > MAX_BODY {
            lease.finish("failed", 413, "签名后请求超过 32 MiB", Value::Null);
            return error(413, "签名后请求超过 32 MiB", is_anthropic);
        }
        let mut request = g.client.post(url);
        for (name, value) in prepared["headers"].as_object().into_iter().flatten() {
            if let Some(value) = value.as_str() {
                request = request.header(name, value);
            }
        }
        // Match the installed runtime's device headers, without changing its auth signature.
        let ed_key = config.edition.clone();
        let identity = tokio::task::spawn_blocking(move || {
            qcore::edition(&ed_key)
                .map(qcore::device_headers)
                .unwrap_or_default()
        })
        .await
        .unwrap_or_default();
        for (name, value) in identity {
            if name != "Cosy-Version" && name != "Cosy-ClientType" {
                request = request.header(name, value);
            }
        }
        request
            .header("Accept", "text/event-stream")
            .body(prepared["body"].as_str().unwrap_or("").to_owned())
    } else {
        g.client
            .post(format!(
                "{}/model/v1/chat/completions",
                endpoint.trim_end_matches('/')
            ))
            .bearer_auth(&account.token)
            .header("Accept", "text/event-stream")
            .header("X-Request-ID", &request_id)
            .header("X-Session-ID", &session)
            .header("User-Agent", "qoder/1.1.66")
            .json(&body)
    };
    let sent = request.send().await;
    let response = match sent {
        Ok(r) => r,
        Err(_) => {
            lease.finish("failed", 502, "Qoder 上游连接失败或超时", Value::Null);
            return error(502, "Qoder 上游连接失败或超时", is_anthropic);
        }
    };
    if !response.status().is_success() {
        let status = response.status().as_u16();
        let retry = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(60)
            .min(3600);
        if [401, 403, 429].contains(&status) {
            g.runtime.lock().unwrap().cooldown.insert(
                account.id(),
                Instant::now() + Duration::from_secs(if status == 429 { retry } else { 60 }),
            );
        }
        let message =
            format!("Qoder 上游拒绝请求（HTTP {status}），请检查账号、模型权限或接入地址");
        if let Ok(bytes) = bounded_body(response, 64 * 1024).await {
            if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                lease.log.as_mut().unwrap().upstream_code =
                    v.get("code").or_else(|| v["error"].get("code")).map(|v| {
                        v.to_string()
                            .chars()
                            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                            .take(48)
                            .collect()
                    });
            }
        }
        lease.finish("failed", status, &message, Value::Null);
        return error(status, &message, is_anthropic);
    }
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    if !content_type.contains("text/event-stream") {
        lease.finish("failed", 502, "Qoder 上游没有返回 SSE 流", Value::Null);
        return error(502, "Qoder 上游没有返回 SSE 流", is_anthropic);
    }
    if !downstream_stream {
        let mut stream = response.bytes_stream();
        let mut parser = Parser::default();
        let mut aggregate = Aggregate::new(&model);
        let mut failure = None;
        while let Some(part) = stream.next().await {
            let part = match part {
                Ok(b) => b,
                Err(_) => {
                    failure = Some("上游连接中断".to_owned());
                    break;
                }
            };
            if lease.log.as_ref().unwrap().first_byte_ms.is_none() {
                lease.log.as_mut().unwrap().first_byte_ms = Some(lease.start.elapsed().as_millis());
            }
            match parser.feed(&part) {
                Ok(events) => {
                    for event in events {
                        if let Some(chunk) = event {
                            if let Err(e) = aggregate.push(&chunk) {
                                failure = Some(e);
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    failure = Some(e);
                    break;
                }
            }
            if failure.is_some() || parser.done {
                break;
            }
        }
        if failure.is_none() && (!parser.done || !aggregate.finished) {
            failure = Some("上游流未完整结束".into());
        }
        if let Some(e) = failure {
            lease.finish("failed", 502, &e, aggregate.usage());
            return error(502, &e, is_anthropic);
        }
        let completion = aggregate.value;
        let result = match protocol {
            Protocol::Chat => Ok(completion.clone()),
            Protocol::Messages => anthropic::from_completion(&completion, &model),
            Protocol::Responses => {
                responses::from_completion_with_tools(&completion, &model, tools)
            }
        };
        return match result {
            Ok(v) => {
                lease.finish("completed", 200, "", completion["usage"].clone());
                Json(v).into_response()
            }
            Err(e) => {
                lease.finish("failed", 502, &e, completion["usage"].clone());
                error(502, &e, is_anthropic)
            }
        };
    }
    let mut upstream = response.bytes_stream();
    let mut converter = match protocol {
        Protocol::Chat => Converter::Chat,
        Protocol::Messages => Converter::Messages(anthropic::Stream::new(&model)),
        Protocol::Responses => Converter::Responses(responses::Stream::with_tools(&model, tools)),
    };
    let output = async_stream::stream! {
        let mut parser=Parser::default();let mut usage=Value::Null;let mut failure=None;let mut ended=false;
        while let Some(part)=upstream.next().await{
            let part=match part{Ok(b)=>b,Err(_)=>{failure=Some("上游连接中断或超时".to_owned());break}};
            if lease.log.as_ref().unwrap().first_byte_ms.is_none(){lease.log.as_mut().unwrap().first_byte_ms=Some(lease.start.elapsed().as_millis());}
            let events=match parser.feed(&part){Ok(v)=>v,Err(e)=>{failure=Some(e);break}};
            for event in events{
                let bytes=if let Some(mut chunk)=event{chunk["model"]=json!(model);if chunk["usage"].is_object(){usage=chunk["usage"].clone();lease.log.as_mut().unwrap().usage=usage.clone();}format!("data: {chunk}\n\n").into_bytes()}else{ended=true;b"data: [DONE]\n\n".to_vec()};
                let out=converter.feed(&bytes);if !out.is_empty(){yield Ok::<Bytes,std::convert::Infallible>(Bytes::from(out));}
            }
            if ended{break}
        }
        if !ended&&failure.is_none(){failure=Some("上游流缺少完整结束标记".into());}
        if ended && !converter.succeeded() && failure.is_none(){failure=Some("协议响应适配失败".into());}
        if let Some(e)=failure{let bytes=converter.fail(&e);yield Ok(Bytes::from(bytes));lease.finish("failed",502,&e,usage);}
        else{lease.finish("completed",200,"",usage);}
    };
    (
        [
            ("content-type", "text/event-stream"),
            ("cache-control", "no-cache"),
            ("x-accel-buffering", "no"),
        ],
        Body::from_stream(output),
    )
        .into_response()
}

fn normalize_qoder_messages(chat: &mut Value) {
    for row in chat["messages"].as_array_mut().into_iter().flatten() {
        if let Some(parts) = row["content"].as_array() {
            if parts
                .iter()
                .all(|p| p["type"] == "text" && p["text"].is_string())
            {
                row["content"] = json!(parts
                    .iter()
                    .map(|p| p["text"].as_str().unwrap())
                    .collect::<Vec<_>>()
                    .join("\n"));
            }
        }
    }
}

fn legacy_body(
    chat: &Value,
    model_info: &Value,
    request_id: &str,
    session: &str,
    edition: &str,
) -> Value {
    let model = chat["model"].as_str().unwrap_or("");
    let reasoning =
        model_info["isReasoning"].as_bool().unwrap_or(false) && chat["reasoning_effort"] != "none";
    let rows = chat["messages"].as_array().cloned().unwrap_or_default();
    let prompt = rows
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_str())
        .unwrap_or("");
    let system: Vec<Value> = rows
        .iter()
        .filter(|m| m["role"] == "system")
        .filter_map(|m| m["content"].as_str())
        .map(|s| json!({"type":"text","text":s}))
        .collect();
    let mut parameters = json!({});
    for name in [
        "max_tokens",
        "temperature",
        "top_p",
        "stop",
        "reasoning_effort",
        "parallel_tool_calls",
        "tool_choice",
        "response_format",
    ] {
        if let Some(v) = chat.get(name) {
            parameters[name] = v.clone();
        }
    }
    if chat["reasoning_effort"] == "none" {
        parameters["enable_thinking"] = json!(false);
    }
    json!({"request_id":request_id,"request_set_id":request_id,"chat_record_id":request_id,"session_id":session,"stream":true,"chat_task":"FREE_INPUT","chat_context":{"text":prompt,"features":[],"extra":{"context":[],"modelConfig":{"key":model,"is_reasoning":reasoning},"originalContent":prompt},"chatPrompt":"","imageUrls":null},"is_reply":true,"is_retry":false,"source":1,"version":"3","agent_id":"agent_common","task_id":"common","session_type":if edition=="cn"{"qoderclicn"}else{"qodercli"},"aliyun_user_type":"","model_config":{"key":model,"display_name":model_info["name"].as_str().unwrap_or(model),"model":"","format":model_info["format"].as_str().unwrap_or("openai"),"is_vl":model_info["supportsImages"].as_bool().unwrap_or(false),"is_reasoning":reasoning,"api_key":"","url":"","source":"system","max_input_tokens":model_info["contextWindow"].as_u64().unwrap_or(200000)},"system":system,"messages":rows,"tools":chat.get("tools").cloned().unwrap_or(json!([])),"parameters":parameters,"business":{"id":request_id,"name":"chat","product":"cli","version":"1.1.66","type":"agent","begin_at":chrono::Utc::now().timestamp_millis()}})
}

enum Converter {
    Chat,
    Messages(anthropic::Stream),
    Responses(responses::Stream),
}
impl Converter {
    fn succeeded(&self) -> bool {
        match self {
            Self::Chat => true,
            Self::Messages(s) => s.succeeded(),
            Self::Responses(s) => s.succeeded(),
        }
    }
    fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        match self {
            Self::Chat => bytes.to_vec(),
            Self::Messages(s) => s.feed(bytes),
            Self::Responses(s) => s.feed(bytes),
        }
    }
    fn fail(&mut self, e: &str) -> Vec<u8> {
        match self {
            Self::Chat => format!(
                "data: {}\n\n",
                json!({"error":{"message":e,"type":"upstream_error"}})
            )
            .into_bytes(),
            Self::Messages(s) => s.fail(e),
            Self::Responses(s) => s.fail(e),
        }
    }
}
#[derive(Default)]
struct Parser {
    pending: Vec<u8>,
    frame: Vec<u8>,
    retained: usize,
    done: bool,
    finished: bool,
}
impl Parser {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<Option<Value>>, String> {
        self.retained += bytes.len();
        if self.retained > MAX_OUTPUT || self.pending.len() + bytes.len() > MAX_EVENT {
            return Err("上游流超过本地大小限制".into());
        }
        self.pending.extend_from_slice(bytes);
        let mut out = vec![];
        while let Some(n) = self.pending.iter().position(|b| *b == b'\n') {
            let mut line = self.pending.drain(..=n).collect::<Vec<_>>();
            while line.last().is_some_and(|b| *b == b'\r' || *b == b'\n') {
                line.pop();
            }
            if let Some(data) = line.strip_prefix(b"data:") {
                if !self.frame.is_empty() {
                    self.frame.push(b'\n');
                }
                self.frame
                    .extend_from_slice(data.strip_prefix(b" ").unwrap_or(data));
                if self.frame.len() > MAX_EVENT {
                    return Err("上游 SSE 事件过大".into());
                }
            }
            if !line.is_empty() || self.frame.is_empty() {
                continue;
            }
            let data = std::mem::take(&mut self.frame);
            if data == b"[DONE]" {
                if !self.finished {
                    return Err("上游没有返回有效 finish_reason".into());
                }
                self.done = true;
                out.push(None);
                break;
            }
            if data == b"[NOT_EXCEED_QUOTA]" {
                continue;
            }
            if data.starts_with(b"[EXCEED_QUOTA]") {
                return Err("Qoder 模型配额不足".into());
            }
            if data.starts_with(b"[NOTIFICATIONS]") {
                continue;
            }
            let mut v: Value = serde_json::from_slice(&data).map_err(|_| "上游 SSE JSON 无效")?;
            if let Some(status) = v["statusCodeValue"].as_u64() {
                if status != 200 {
                    return Err(format!("Qoder 流式业务错误（{status}）"));
                }
                let inner = v["body"].as_str().unwrap_or("").trim();
                if inner.is_empty() {
                    continue;
                }
                if inner == "[DONE]" {
                    if !self.finished {
                        return Err("上游没有返回有效 finish_reason".into());
                    }
                    self.done = true;
                    out.push(None);
                    break;
                }
                v = serde_json::from_str(inner).map_err(|_| "Qoder 流式信封内容无效")?;
            }
            if v.get("error").is_some_and(|v| !v.is_null())
                || v.get("code").is_some_and(|v| v.as_i64() != Some(0))
            {
                return Err("Qoder 返回业务错误事件，请检查模型权限与配额".into());
            }
            for c in v["choices"].as_array().into_iter().flatten() {
                if c["finish_reason"].is_string() {
                    self.finished = true;
                }
            }
            out.push(Some(v));
        }
        Ok(out)
    }
}
struct Aggregate {
    value: Value,
    size: usize,
    finished: bool,
}
impl Aggregate {
    fn new(model: &str) -> Self {
        Self {
            value: json!({"id":format!("chatcmpl_{}",uuid::Uuid::new_v4().simple()),"object":"chat.completion","created":chrono::Utc::now().timestamp(),"model":model,"choices":[{"index":0,"message":{"role":"assistant","content":""},"finish_reason":null}],"usage":null}),
            size: 0,
            finished: false,
        }
    }
    fn usage(&self) -> Value {
        self.value["usage"].clone()
    }
    fn push(&mut self, v: &Value) -> Result<(), String> {
        self.size += v.to_string().len();
        if self.size > MAX_OUTPUT {
            return Err("聚合响应过大".into());
        }
        if v["usage"].is_object() {
            self.value["usage"] = v["usage"].clone();
        }
        for choice in v["choices"].as_array().into_iter().flatten() {
            if choice["index"].as_u64().unwrap_or(0) != 0 {
                return Err("当前只支持一个生成结果".into());
            }
            if choice["finish_reason"].is_string() {
                self.value["choices"][0]["finish_reason"] = choice["finish_reason"].clone();
                self.finished = true;
            }
            let m = &mut self.value["choices"][0]["message"];
            for field in ["content", "reasoning_content"] {
                if let Some(text) = choice["delta"][field].as_str() {
                    let current = m[field].as_str().unwrap_or("");
                    m[field] = json!(format!("{current}{text}"));
                }
            }
            for tool in choice["delta"]["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let i = tool["index"].as_u64().unwrap_or(0) as usize;
                if i > 256 {
                    return Err("上游工具索引无效".into());
                }
                if !m["tool_calls"].is_array() {
                    m["tool_calls"] = json!([]);
                }
                let calls = m["tool_calls"].as_array_mut().unwrap();
                while calls.len() <= i {
                    calls.push(
                        json!({"type":"function","id":"","function":{"name":"","arguments":""}}),
                    );
                }
                if tool["id"].is_string() {
                    calls[i]["id"] = tool["id"].clone();
                }
                for field in ["name", "arguments"] {
                    if let Some(delta) = tool["function"][field].as_str() {
                        let current = calls[i]["function"][field].as_str().unwrap_or("");
                        calls[i]["function"][field] = json!(format!("{current}{delta}"));
                    }
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn official_endpoints_only() {
        assert!(valid_endpoint("https://api2-v2.qoder.sh"));
        for s in [
            "http://api2.qoder.sh",
            "https://qoder.sh.evil.com",
            "https://user@api2.qoder.sh",
            "https://api2.qoder.sh/path",
        ] {
            assert!(!valid_endpoint(s));
        }
    }
    #[test]
    fn fragmented_sse_and_business_errors() {
        let mut p = Parser::default();
        assert!(p
            .feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},")
            .unwrap()
            .is_empty());
        assert_eq!(
            p.feed(b"\"finish_reason\":\"stop\"}]}\r\n\r\ndata: [DONE]\n\n")
                .unwrap()
                .len(),
            2
        );
        assert!(p.done);
        assert!(Parser::default()
            .feed(b"data: {\"code\":11128}\n\n")
            .is_err());
        assert!(Parser::default().feed(b"data: [DONE]\n\n").is_err());
    }
    #[test]
    fn account_catalog_deduplicates() {
        let rows =
            normalize_models(&json!({"qodercli":[{"key":"a","display_name":"A"},{"key":"a"}]}))
                .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["name"], "A");
    }
    #[test]
    fn aggregate_parallel_tool_fragments() {
        let mut a = Aggregate::new("m");
        a.push(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","function":{"name":"read","arguments":"{\"x\":"}},{"index":1,"id":"b","function":{"name":"write","arguments":"{}"}}]}}]})).unwrap();
        a.push(&json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":12}})).unwrap();
        assert_eq!(
            a.value["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"],
            "{\"x\":1}"
        );
        assert_eq!(a.usage()["prompt_tokens"], 12);
    }
}

#[tauri::command]
pub async fn gateway_state(g: tauri::State<'_, Arc<Gateway>>) -> Result<Value, String> {
    let g = g.inner().clone();
    tokio::task::spawn_blocking(move || g.snapshot())
        .await
        .map_err(|_| "读取网关状态失败".into())
}
#[tauri::command]
pub async fn gateway_start(g: tauri::State<'_, Arc<Gateway>>) -> Result<Value, String> {
    g.start().await?;
    Ok(g.snapshot())
}
#[tauri::command]
pub async fn gateway_stop(g: tauri::State<'_, Arc<Gateway>>) -> Result<Value, String> {
    g.stop();
    Ok(json!({"ok":true}))
}
#[tauri::command]
pub async fn gateway_save(
    g: tauri::State<'_, Arc<Gateway>>,
    config: Config,
) -> Result<Value, String> {
    g.save(config)?;
    Ok(g.snapshot())
}
#[tauri::command]
pub fn gateway_save_route(
    g: tauri::State<'_, Arc<Gateway>>,
    route_id: String,
    alias: String,
    cn: Option<String>,
    intl: Option<String>,
) -> Result<Value, String> {
    let mut config = g.config.lock().unwrap().clone();
    let runtime = g.runtime.lock().unwrap();
    if !crate::qroutes::catalog(&runtime.models, &config.model_routes, "both")
        .iter()
        .any(|r| r["routeId"] == route_id)
    {
        return Err("模型映射不存在，请刷新目录".into());
    }
    if cn.is_none() && intl.is_none() {
        return Err("请至少选择一个地区的真实模型".into());
    }
    for (edition, id) in [("cn", &cn), ("intl", &intl)] {
        if let Some(id) = id {
            if !runtime
                .models
                .get(edition)
                .is_some_and(|rows| rows.iter().any(|m| m["id"] == *id))
            {
                return Err(format!("{edition} 模型不在缓存目录中，请先同步目录"));
            }
        }
    }
    drop(runtime);
    config
        .model_routes
        .insert(route_id, crate::qroutes::ModelRoute { alias, cn, intl });
    g.save(config)?;
    Ok(json!({"ok":true}))
}
#[tauri::command]
pub fn gateway_generate_key() -> String {
    random_key()
}
#[tauri::command]
pub async fn gateway_set_key(
    g: tauri::State<'_, Arc<Gateway>>,
    api_key: String,
) -> Result<Value, String> {
    let gateway = g.inner().clone();
    tokio::task::spawn_blocking(move || gateway.set_key_at(&api_key, &data_file("api-key")))
        .await
        .map_err(|_| "保存 API Key 失败")??;
    Ok(json!({"ok":true}))
}
#[tauri::command]
pub async fn gateway_models(g: tauri::State<'_, Arc<Gateway>>) -> Result<Value, String> {
    g.inner().clone().refresh_models().await
}
#[tauri::command]
pub async fn gateway_clear_logs(g: tauri::State<'_, Arc<Gateway>>) -> Result<Value, String> {
    g.clear_logs();
    Ok(json!({"ok":true}))
}

pub fn run_cli(args: &[String]) -> bool {
    if !args
        .iter()
        .any(|a| a == "--gateway-server" || a == "--gateway-models")
    {
        return false;
    }
    let runtime = tokio::runtime::Runtime::new().expect("async runtime");
    let result: Result<(), String> = runtime.block_on(async {
        let g = Gateway::new()?;
        if let Some(i) = args.iter().position(|a| a == "--gateway-models") {
            if let Some(ed) = args
                .get(i + 1)
                .filter(|s| ["cn", "intl"].contains(&s.as_str()))
            {
                g.config.lock().unwrap().edition = ed.clone();
            }
            let result = g.refresh_models().await?;
            // Model metadata only; no keys, tokens or account identifiers.
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
            return Ok(());
        }
        g.start().await?;
        println!("Qoder gateway listening on loopback; use the key in desktop settings");
        tokio::signal::ctrl_c()
            .await
            .map_err(|_| "signal failure")?;
        g.stop();
        Ok(())
    });
    if let Err(e) = result {
        eprintln!("{e}");
        std::process::exit(1);
    }
    true
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    async fn fixture() -> (
        Arc<Gateway>,
        String,
        Arc<AtomicUsize>,
        tokio::task::JoinHandle<()>,
        tokio::task::JoinHandle<()>,
    ) {
        fixture_with_regions(false).await
    }
    async fn fixture_with_regions(
        dual: bool,
    ) -> (
        Arc<Gateway>,
        String,
        Arc<AtomicUsize>,
        tokio::task::JoinHandle<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let count = Arc::new(AtomicUsize::new(0));
        let counter = count.clone();
        let upstream = Router::new().route("/model/v1/chat/completions", post(move |Json(body):Json<Value>| {
            let counter = counter.clone();
            async move {
                counter.fetch_add(1, Ordering::SeqCst);
                let mut chunks = Vec::new();
                if !["fixture", "intl-fixture", "cn-only", "intl-only", "failure", "truncated"].contains(&body["model"].as_str().unwrap_or("")) {chunks.push(json!({"code":400}));}
                else if body["model"]=="failure" { chunks.push(json!({"code":11128})); }
                else if body["messages"].as_array().unwrap().iter().any(|m|m["role"]=="tool") || !body["tools"].as_array().is_some_and(|v|!v.is_empty()) {
                    chunks.push(json!({"choices":[{"index":0,"delta":{"content":"fixture "}}]}));
                    chunks.push(json!({"choices":[{"index":0,"delta":{"content":"OK"},"finish_reason":"stop"}]}));
                } else {
                    let tool=&body["tools"][0]["function"];
                    let args=if tool["parameters"]["properties"].get("input").is_some(){json!({"input":"*** Begin Patch\n*** End Patch"}).to_string()}else{"{}".into()};
                    chunks.push(json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"fixture-call","type":"function","function":{"name":tool["name"],"arguments":args}}]},"finish_reason":"tool_calls"}]}));
                }
                chunks.push(json!({"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":3,"total_tokens":15}}));
                for chunk in &mut chunks {chunk["model"] = body["model"].clone();}
                let mut wire=chunks.iter().map(|v|format!("data: {v}\n\n")).collect::<String>();
                if body["model"]!="truncated" {wire.push_str("data: [DONE]\n\n");}
                ([("content-type","text/event-stream")],wire)
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let upstream_url = format!("http://{}", listener.local_addr().unwrap());
        let upstream_task = tokio::spawn(async move {
            axum::serve(listener, upstream).await.unwrap();
        });
        let mut runtime = Runtime::default();
        runtime.models.insert(
            "cn".into(),
            ["fixture", "failure", "truncated"]
                .iter()
                .map(|m| json!({"id":m}))
                .collect(),
        );
        let g = Arc::new(Gateway {
            fixture_accounts: Some(if dual {
                ["cn", "intl", "cn", "intl"]
                    .iter()
                    .enumerate()
                    .map(|(i, ed)| qcore::Account {
                        edition: ed.to_string(),
                        uid: format!("fixture{i}"),
                        name: format!("Account {i}"),
                        token: "never-real".into(),
                        ..Default::default()
                    })
                    .collect()
            } else {
                vec![qcore::Account {
                    edition: "cn".into(),
                    uid: "fixture".into(),
                    name: "Offline fixture".into(),
                    token: "never-real".into(),
                    ..Default::default()
                }]
            }),
            config: Mutex::new(Config {
                transport: "modelServer".into(),
                cn_endpoint: upstream_url.clone(),
                intl_endpoint: upstream_url,
                ..Default::default()
            }),
            key: Mutex::new("fixture-local-key".into()),
            runtime: Mutex::new(runtime),
            client: reqwest::Client::new(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let app = router(g.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (g, url, count, task, upstream_task)
    }
    #[tokio::test]
    async fn both_regions_route_per_model_and_record_real_identity() {
        let (g, url, count, task, upstream) = fixture_with_regions(true).await;
        {
            let mut runtime = g.runtime.lock().unwrap();
            runtime.models = HashMap::from([
                (
                    "cn".into(),
                    vec![
                        json!({"id":"fixture","name":"Shared"}),
                        json!({"id":"cn-only","name":"China Only"}),
                    ],
                ),
                (
                    "intl".into(),
                    vec![
                        json!({"id":"intl-fixture","name":"Shared"}),
                        json!({"id":"intl-only","name":"International Only"}),
                    ],
                ),
            ]);
        }
        g.config.lock().unwrap().model_routes.insert(
            "Shared".into(),
            crate::qroutes::ModelRoute {
                alias: "my-shared".into(),
                cn: Some("fixture".into()),
                intl: Some("intl-fixture".into()),
            },
        );
        let client = reqwest::Client::new();
        let list: Value = client
            .get(format!("{url}/v1/models"))
            .bearer_auth(g.api_key())
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(list["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == "my-shared"));
        assert!(!list["data"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == "Shared"));
        for expected in ["cn", "intl", "cn", "intl", "cn"] {
            let response = client
                .post(format!("{url}/v1/chat/completions"))
                .bearer_auth(g.api_key())
                .json(&json!({"model":"my-shared","messages":[{"role":"user","content":"test"}]}))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            assert_eq!(
                response.json::<Value>().await.unwrap()["model"],
                "my-shared"
            );
            let runtime = g.runtime.lock().unwrap();
            let log = runtime.logs.back().unwrap();
            assert_eq!(log.edition, expected);
            assert_eq!(log.model, "my-shared");
            assert_eq!(
                log.real_model,
                if expected == "cn" {
                    "fixture"
                } else {
                    "intl-fixture"
                }
            );
        }
        for (model, region) in [("China Only", "cn"), ("International Only", "intl")] {
            for _ in 0..3 {
                let res = client
                    .post(format!("{url}/v1/responses"))
                    .bearer_auth(g.api_key())
                    .json(&json!({"model":model,"input":"test"}))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(res.status(), 200);
                let _ = res.bytes().await;
                assert_eq!(
                    g.runtime.lock().unwrap().logs.back().unwrap().edition,
                    region
                );
            }
        }
        for streaming in [false, true] {
            for (path, body) in [
                (
                    "/v1/messages",
                    json!({"model":"my-shared","messages":[{"role":"user","content":"test"}],"max_tokens":32,"stream":streaming}),
                ),
                (
                    "/v1/responses",
                    json!({"model":"my-shared","input":"test","stream":streaming}),
                ),
            ] {
                let res = client
                    .post(format!("{url}{path}"))
                    .bearer_auth(g.api_key())
                    .json(&body)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(res.status(), 200);
                let wire = res.text().await.unwrap();
                assert!(wire.contains("my-shared"));
                assert!(!wire.contains("intl-fixture"));
                let runtime = g.runtime.lock().unwrap();
                let log = runtime.logs.back().unwrap();
                assert_eq!(log.model, "my-shared");
                assert!(matches!(
                    log.real_model.as_str(),
                    "fixture" | "intl-fixture"
                ));
            }
        }
        // A pin to the other region must not widen the model's allowed account pool.
        let before = count.load(Ordering::SeqCst);
        let response = client
            .post(format!("{url}/v1/chat/completions"))
            .bearer_auth(g.api_key())
            .header("x-account-id", "intl:fixture1")
            .json(&json!({"model":"China Only","messages":[{"role":"user","content":"test"}]}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 503);
        assert_eq!(count.load(Ordering::SeqCst), before);
        // Disable both CN accounts: shared routes still work internationally; CN-only must fail closed.
        g.config.lock().unwrap().disabled_accounts =
            vec!["cn:fixture0".into(), "cn:fixture2".into()];
        for (model, status) in [("my-shared", 200), ("China Only", 503)] {
            let res = client
                .post(format!("{url}/v1/chat/completions"))
                .bearer_auth(g.api_key())
                .json(&json!({"model":model,"messages":[{"role":"user","content":"test"}]}))
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), status);
            let _ = res.bytes().await;
        }
        task.abort();
        upstream.abort();
    }
    #[tokio::test]
    async fn route_selection_respects_capacity_cooldown_and_strategy() {
        let (g, _url, count, task, upstream) = fixture_with_regions(true).await;
        let route = json!({"routeId":"shared","publicId":"alias","targets":[{"edition":"cn","id":"fixture"},{"edition":"intl","id":"intl-fixture"}]});
        let mut held = Vec::new();
        for _ in 0..8 {
            let (_, lease, _, _) = select(
                g.clone(),
                &HeaderMap::new(),
                Protocol::Chat,
                "alias",
                &route,
            )
            .await
            .unwrap();
            held.push(lease);
        }
        assert!(select(
            g.clone(),
            &HeaderMap::new(),
            Protocol::Chat,
            "alias",
            &route
        )
        .await
        .is_err());
        drop(held);
        {
            let mut runtime = g.runtime.lock().unwrap();
            runtime.cooldown.insert(
                "cn:fixture0".into(),
                Instant::now() + Duration::from_secs(30),
            );
            runtime.cooldown.insert(
                "cn:fixture2".into(),
                Instant::now() + Duration::from_secs(30),
            );
        }
        let (account, lease, config, info) = select(
            g.clone(),
            &HeaderMap::new(),
            Protocol::Chat,
            "alias",
            &route,
        )
        .await
        .unwrap();
        assert_eq!(account.edition, "intl");
        assert_eq!(config.edition, "intl");
        assert_eq!(info["id"], "intl-fixture");
        drop(lease);
        g.config.lock().unwrap().strategy = "expiryFirst".into();
        let (_, lease, _, _) = select(
            g.clone(),
            &HeaderMap::new(),
            Protocol::Chat,
            "alias",
            &route,
        )
        .await
        .unwrap();
        drop(lease);
        assert_eq!(count.load(Ordering::SeqCst), 0);
        task.abort();
        upstream.abort();
    }
    #[tokio::test]
    async fn key_rotation_persists_and_rejects_old_key() {
        let (g, url, _count, upstream, server) = fixture().await;
        let old = g.api_key();
        let key = random_key();
        assert_ne!(key, random_key());
        assert!(validate_key(&key).is_ok());
        for invalid in [
            "",
            "short",
            &"x".repeat(257),
            &format!("{} ", "x".repeat(32)),
            &"中".repeat(32),
        ] {
            assert!(validate_key(invalid).is_err());
        }
        let path = std::env::temp_dir().join(format!("qoder-key-test-{}", uuid::Uuid::new_v4()));
        g.set_key_at(&key, &path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), key);
        assert!(g.set_key_at("invalid", &path).is_err());
        assert_eq!(g.api_key(), key);
        let client = reqwest::Client::new();
        assert_eq!(
            client
                .get(format!("{url}/v1/models"))
                .bearer_auth(old)
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(
            client
                .get(format!("{url}/v1/models"))
                .bearer_auth(&key)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        assert_eq!(
            client
                .get(format!("{url}/v1/models"))
                .header("x-api-key", &key)
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
        server.abort();
        upstream.abort();
    }
    #[tokio::test]
    async fn official_names_are_advertised_and_legacy_ids_still_route() {
        let (g, url, count, task, upstream) = fixture().await;
        g.runtime.lock().unwrap().models.get_mut("cn").unwrap()[0]["name"] =
            json!("Official Fixture");
        let client = reqwest::Client::new();
        for prefix in ["", "/v1", "/v1/v1"] {
            let list: Value = client
                .get(format!("{url}{prefix}/models"))
                .bearer_auth(g.api_key())
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            assert_eq!(list["data"][0]["id"], "Official Fixture");
            assert_eq!(list["data"][0]["display_name"], "Official Fixture");
            assert_eq!(list["data"][0]["qoder_id"], "fixture");
        }
        assert_eq!(g.snapshot()["models"][0]["publicId"], "Official Fixture");
        for model in ["Official Fixture", "fixture"] {
            for streaming in [false, true] {
                for (path, mut body) in [
                    (
                        "/v1/chat/completions",
                        json!({"messages":[{"role":"user","content":"test"}]}),
                    ),
                    (
                        "/v1/messages",
                        json!({"messages":[{"role":"user","content":"test"}],"max_tokens":32}),
                    ),
                    ("/v1/responses", json!({"input":"test"})),
                ] {
                    body["model"] = json!(model);
                    body["stream"] = json!(streaming);
                    let response = client
                        .post(format!("{url}{path}"))
                        .bearer_auth(g.api_key())
                        .json(&body)
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(response.status(), 200);
                    let wire = response.text().await.unwrap();
                    if streaming {
                        assert!(
                            !wire.contains("response.failed") && !wire.contains("event: error")
                        );
                        let events: Vec<Value> = wire
                            .lines()
                            .filter_map(|line| line.strip_prefix("data: "))
                            .filter_map(|line| serde_json::from_str(line).ok())
                            .collect();
                        let returned = if path.ends_with("responses") {
                            &events.last().unwrap()["response"]["model"]
                        } else if path.ends_with("messages") {
                            &events[0]["message"]["model"]
                        } else {
                            &events[0]["model"]
                        };
                        assert_eq!(returned, "Official Fixture");
                    } else {
                        assert_eq!(
                            serde_json::from_str::<Value>(&wire).unwrap()["model"],
                            "Official Fixture"
                        );
                    }
                }
            }
        }
        assert_eq!(count.load(Ordering::SeqCst), 12);
        task.abort();
        upstream.abort();
    }
    #[test]
    fn ambiguous_public_names_are_disambiguated_without_shadowing_internal_ids() {
        let models = vec![
            json!({"id":"a","name":"Shared"}),
            json!({"id":"b","name":"Shared"}),
        ];
        assert_eq!(public_catalog(&models)[0]["publicId"], "Shared [a]");
        assert_eq!(resolve_model(&models, "Shared [b]").unwrap()["id"], "b");
        assert!(resolve_model(&models, "Shared").is_none());
        let models = vec![
            json!({"id":"a","name":"b"}),
            json!({"id":"b","name":"Other"}),
        ];
        assert_eq!(resolve_model(&models, "b").unwrap()["id"], "b");
        assert_eq!(resolve_model(&models, "b [a]").unwrap()["id"], "a");
    }
    #[tokio::test]
    async fn http_auth_counting_and_three_protocols() {
        let (g, url, count, task, upstream) = fixture().await;
        let client = reqwest::Client::new();
        assert_eq!(
            client
                .get(format!("{url}/v1/models"))
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let count_response = client
            .post(format!("{url}/v1/messages/count_tokens"))
            .header("x-api-key", g.api_key())
            .json(
                &json!({"model":"fixture","messages":[{"role":"user","content":"secret prompt"}]}),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(count_response.status(), 200);
        assert_eq!(count_response.headers()["x-token-count-method"], "estimate");
        assert_eq!(count.load(Ordering::SeqCst), 0);
        for streaming in [false, true] {
            for (path, body) in [
                (
                    "/v1/chat/completions",
                    json!({"model":"fixture","messages":[{"role":"user","content":"secret prompt"}],"stream":streaming}),
                ),
                (
                    "/v1/messages",
                    json!({"model":"fixture","max_tokens":64,"messages":[{"role":"user","content":"secret prompt"}],"stream":streaming}),
                ),
                (
                    "/v1/responses",
                    json!({"model":"fixture","input":"secret prompt","stream":streaming}),
                ),
            ] {
                let result = client
                    .post(format!("{url}{path}"))
                    .bearer_auth(g.api_key())
                    .json(&body)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(result.status(), 200);
                let text = result.text().await.unwrap();
                assert!(text.contains("fixture") && text.contains("OK"));
                if streaming {
                    assert!(text.contains(if path.ends_with("messages") {
                        "message_stop"
                    } else if path.ends_with("responses") {
                        "response.completed"
                    } else {
                        "[DONE]"
                    }));
                }
            }
        }
        assert_eq!(count.load(Ordering::SeqCst), 6);
        let r = g.runtime.lock().unwrap();
        assert!(r
            .logs
            .iter()
            .all(|l| l.status == "completed" && l.usage["total_tokens"] == 15));
        assert!(!serde_json::to_string(&r.logs)
            .unwrap()
            .contains("secret prompt"));
        assert_eq!(r.active.values().sum::<usize>(), 0);
        task.abort();
        upstream.abort();
    }
    #[tokio::test]
    async fn custom_namespace_tool_roundtrip_and_stream_failure() {
        let (g, url, _, task, upstream) = fixture().await;
        let client = reqwest::Client::new();
        let input = json!({"model":"fixture","input":"apply once","tools":[{"type":"namespace","name":"functions","tools":[{"type":"custom","name":"apply_patch","format":{"type":"text"}}]}]});
        let result: Value = client
            .post(format!("{url}/v1/responses"))
            .bearer_auth(g.api_key())
            .json(&input)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let call = result["output"][0].clone();
        assert_eq!(call["type"], "custom_tool_call");
        assert_eq!(call["namespace"], "functions");
        assert_eq!(call["input"], "*** Begin Patch\n*** End Patch");
        let mut second = input.clone();
        second["input"] = json!([{"role":"user","content":"apply once"},call,{"type":"custom_tool_call_output","call_id":"fixture-call","output":"applied"}]);
        let result: Value = client
            .post(format!("{url}/v1/responses"))
            .bearer_auth(g.api_key())
            .json(&second)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(result["status"], "completed");
        assert_eq!(result["output"][0]["content"][0]["text"], "fixture OK");
        let mut streaming = input.clone();
        streaming["stream"] = json!(true);
        let wire = client
            .post(format!("{url}/v1/responses"))
            .bearer_auth(g.api_key())
            .json(&streaming)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(wire.contains("response.custom_tool_call_input.delta"));
        assert!(!wire.contains("response.function_call_arguments.delta"));
        assert!(wire.contains("response.completed"));
        for model in ["failure", "truncated"] {
            let wire = client
                .post(format!("{url}/v1/responses"))
                .bearer_auth(g.api_key())
                .json(&json!({"model":model,"input":"x","stream":true}))
                .send()
                .await
                .unwrap()
                .text()
                .await
                .unwrap();
            assert!(wire.contains("response.failed"));
            assert!(!wire.contains("response.completed"));
        }
        task.abort();
        upstream.abort();
    }
}

#[cfg(test)]
mod qoder_contract_tests {
    use super::*;
    #[test]
    fn text_history_is_scalar_while_user_images_are_preserved() {
        let mut chat = json!({"messages":[{"role":"assistant","content":[{"type":"text","text":"before tool"}],"tool_calls":[{"id":"c","type":"function","function":{"name":"echo","arguments":"{}"}}]},{"role":"tool","tool_call_id":"c","content":[{"type":"text","text":"first"},{"type":"text","text":"second"}]},{"role":"user","content":[{"type":"text","text":"look"},{"type":"image_url","image_url":{"url":"https://example.test/image.png"}}]}]});
        normalize_qoder_messages(&mut chat);
        assert_eq!(chat["messages"][0]["content"], "before tool");
        assert_eq!(chat["messages"][1]["content"], "first\nsecond");
        assert!(chat["messages"][2]["content"].is_array());
        assert_eq!(chat["messages"][0]["tool_calls"][0]["id"], "c");
    }
    #[test]
    fn legacy_transport_preserves_model_tools_and_token_limits() {
        let chat = json!({"model":"exact-model","messages":[{"role":"user","content":"a"}],"tools":[{"type":"function","function":{"name":"echo"}}],"max_tokens":321,"reasoning_effort":"none"});
        let body = legacy_body(
            &chat,
            &json!({"isReasoning":true,"supportsImages":true}),
            "r",
            "s",
            "cn",
        );
        assert_eq!(body["model_config"]["key"], "exact-model");
        assert_eq!(body["parameters"]["max_tokens"], 321);
        assert_eq!(body["parameters"]["enable_thinking"], false);
        assert_eq!(body["model_config"]["is_reasoning"], false);
        assert_eq!(body["tools"], chat["tools"]);
        assert_eq!(body["session_type"], "qoderclicn");
    }
    #[test]
    fn legacy_envelope_and_quota_events_are_not_misreported_as_success() {
        let chunk = json!({"choices":[{"delta":{"content":"ok"},"finish_reason":"stop"}]});
        let envelope = json!({"statusCodeValue":200,"body":chunk.to_string()});
        let mut p = Parser::default();
        let events = p
            .feed(
                format!("data: [NOT_EXCEED_QUOTA]\n\ndata: {envelope}\n\ndata: [DONE]\n\n")
                    .as_bytes(),
            )
            .unwrap();
        assert_eq!(events.len(), 2);
        assert!(p.done);
        assert!(Parser::default()
            .feed(b"data: {\"statusCodeValue\":400,\"body\":\"provider rejected\"}\n\n")
            .is_err());
        assert!(Parser::default()
            .feed(b"data: [EXCEED_QUOTA] unavailable\n\n")
            .is_err());
    }
}

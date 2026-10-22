//! Anthropic Messages compatibility over the shared Qoder chat pipeline.
//! No Anthropic credentials, signatures, or server-side tools are fabricated.
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn text_block(block: &Value) -> Result<&str, String> {
    if block["type"] != "text" {
        return Err("此位置只支持 text 内容块".into());
    }
    block["text"].as_str().ok_or("text 必须是字符串".into())
}

// Claude Code places its own billing transport metadata in a dedicated leading
// system block. It has no Chat Completions equivalent. Do not forward that
// metadata as model instructions; preserve identity text and every other rule.
fn without_billing_metadata(value: &str) -> &str {
    let (first, rest) = value.split_once('\n').unwrap_or((value, ""));
    if first.starts_with("x-anthropic-billing-header:") {
        rest
    } else {
        value
    }
}

fn system_text(value: &Value) -> Result<String, String> {
    if let Some(s) = value.as_str() {
        return Ok(without_billing_metadata(s).into());
    }
    let blocks = value.as_array().ok_or("system 必须是字符串或内容块数组")?;
    blocks
        .iter()
        .map(|block| text_block(block).map(without_billing_metadata))
        .filter(|s| !matches!(s, Ok(s) if s.is_empty()))
        .collect::<Result<Vec<_>, _>>()
        .map(|s| s.join("\n"))
}

fn part(block: &Value) -> Result<Value, String> {
    match block["type"].as_str() {
        Some("text") => {
            Ok(json!({"type":"text","text":block["text"].as_str().ok_or("text 必须是字符串")?}))
        }
        Some("image") => {
            let source = &block["source"];
            let url = match source["type"].as_str() {
                Some("base64") => {
                    let mime = source["media_type"].as_str().ok_or("图片缺少 media_type")?;
                    if !["image/png", "image/jpeg", "image/gif", "image/webp"].contains(&mime) {
                        return Err("不支持此图片格式".into());
                    }
                    format!(
                        "data:{mime};base64,{}",
                        source["data"].as_str().ok_or("图片缺少 base64 数据")?
                    )
                }
                Some("url") => {
                    let url = source["url"].as_str().ok_or("图片缺少 URL")?;
                    if !url.starts_with("https://") {
                        return Err("图片 URL 必须使用 HTTPS".into());
                    }
                    url.into()
                }
                _ => return Err("图片只支持 base64 或 URL 来源".into()),
            };
            Ok(json!({"type":"image_url","image_url":{"url":url}}))
        }
        _ => Err(format!(
            "暂不支持 {} 内容块；未向上游发送请求",
            block["type"].as_str().unwrap_or("未知")
        )),
    }
}

fn flush_user(parts: &mut Vec<Value>, out: &mut Vec<Value>) {
    if !parts.is_empty() {
        out.push(json!({"role":"user","content":std::mem::take(parts)}));
    }
}

pub fn to_chat(input: &Value) -> Result<Value, String> {
    let model = input["model"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("model 必须是非空字符串")?;
    let max = input["max_tokens"]
        .as_u64()
        .filter(|n| *n > 0)
        .ok_or("max_tokens 必须是正整数")?;
    if input.get("stream").is_some_and(|s| !s.is_boolean()) {
        return Err("stream 必须是布尔值".into());
    }
    let mut messages = vec![];
    if let Some(system) = input.get("system") {
        messages.push(json!({"role":"system","content":system_text(system)?}));
    }
    let rows = input["messages"]
        .as_array()
        .filter(|a| !a.is_empty())
        .ok_or("messages 必须是非空数组")?;
    for row in rows {
        let role = row["role"].as_str().ok_or("消息缺少 role")?;
        // Claude Code sends mid-conversation system updates. Collect these
        // below into the leading system message required by chat providers.
        if role == "system" {
            messages.push(json!({"role":"system","content":system_text(&row["content"])?}));
            continue;
        }
        if !["user", "assistant"].contains(&role) {
            return Err(format!("不支持消息角色 {role}"));
        }
        if let Some(s) = row["content"].as_str() {
            messages.push(json!({"role":role,"content":s}));
            continue;
        }
        let blocks = row["content"]
            .as_array()
            .ok_or("content 必须是字符串或数组")?;
        let mut parts = vec![];
        let mut calls = vec![];
        let mut thinking = String::new();
        for block in blocks {
            match block["type"].as_str() {
                Some("tool_use") if role == "assistant" => {
                    let id = block["id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or("tool_use 缺少 id")?;
                    let name = block["name"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or("tool_use 缺少 name")?;
                    if !block["input"].is_object() {
                        return Err("tool_use.input 必须是对象".into());
                    }
                    calls.push(json!({"id":id,"type":"function","function":{"name":name,"arguments":block["input"].to_string()}}));
                }
                Some("tool_result") if role == "user" => {
                    // Native Qoder emits every result before user text in the same
                    // turn, so parallel tool calls remain contiguous and matched.
                    let id = block["tool_use_id"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or("tool_result 缺少 tool_use_id")?;
                    let content = match block.get("content") {
                        None => json!(""),
                        Some(v) if v.is_string() => v.clone(),
                        Some(v) if v.is_array() => json!(v
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(part)
                            .collect::<Result<Vec<_>, _>>()?),
                        _ => return Err("tool_result.content 必须是字符串或内容块数组".into()),
                    };
                    // OpenAI has no is_error flag: preserve the failure in the tool text.
                    let content = if block["is_error"] == true {
                        json!(format!(
                            "Tool execution error: {}",
                            if let Some(s) = content.as_str() {
                                s.to_owned()
                            } else {
                                content.to_string()
                            }
                        ))
                    } else {
                        content
                    };
                    messages.push(json!({"role":"tool","tool_call_id":id,"content":content}));
                }
                Some("thinking") if role == "assistant" => {
                    thinking.push_str(block["thinking"].as_str().ok_or("thinking 必须是字符串")?);
                }
                _ => parts.push(part(block)?),
            }
        }
        if role == "user" {
            flush_user(&mut parts, &mut messages);
        } else {
            let mut message = json!({"role":"assistant","content":if parts.is_empty(){Value::Null}else{json!(parts)}});
            if !calls.is_empty() {
                message["tool_calls"] = json!(calls);
            }
            if !thinking.is_empty() {
                message["reasoning_content"] = json!(thinking);
            }
            messages.push(message);
        }
    }
    let systems = messages
        .iter()
        .filter(|m| m["role"] == "system")
        .filter_map(|m| m["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n\n");
    messages.retain(|m| m["role"] != "system");
    if !systems.is_empty() {
        messages.insert(0, json!({"role":"system","content":systems}));
    }
    let mut result = json!({"model":model,"messages":messages,"max_tokens":max,"stream":input["stream"].as_bool().unwrap_or(false),"stream_options":{"include_usage":true}});
    for key in ["temperature", "top_p", "top_k"] {
        if let Some(v) = input.get(key) {
            result[key] = v.clone();
        }
    }
    if let Some(stop) = input.get("stop_sequences") {
        result["stop"] = stop.clone();
    }
    if let Some(tools) = input.get("tools") {
        let tools = tools
            .as_array()
            .ok_or("tools 必须是数组")?
            .iter()
            .map(|tool| {
                if tool.get("type").is_some_and(|t| t != "custom")
                    || !tool["input_schema"].is_object()
                {
                    return Err(
                        "仅支持含 input_schema 的客户端工具；不支持 Anthropic 服务端工具"
                            .to_owned(),
                    );
                }
                let name = tool["name"]
                    .as_str()
                    .filter(|s| !s.is_empty())
                    .ok_or("工具缺少 name")?;
                let mut function = json!({"name":name,"parameters":tool["input_schema"]});
                if let Some(description) = tool.get("description") {
                    function["description"] = description.clone();
                }
                Ok(json!({"type":"function","function":function}))
            })
            .collect::<Result<Vec<_>, String>>()?;
        if !tools.is_empty() {
            result["tools"] = json!(tools);
        }
    }
    if let Some(choice) = input.get("tool_choice") {
        result["tool_choice"] = match choice["type"].as_str() {
            Some("auto") => json!("auto"),
            Some("any") => json!("required"),
            Some("none") => json!("none"),
            Some("tool") => {
                json!({"type":"function","function":{"name":choice["name"].as_str().ok_or("tool_choice 缺少 name")?}})
            }
            _ => return Err("tool_choice 类型无效".into()),
        };
        if choice["disable_parallel_tool_use"] == true {
            result["parallel_tool_calls"] = json!(false);
        }
    }
    // Output format has an equivalent; cache metadata and thinking signatures do not.
    if input["output_config"]["format"]["type"] == "json_schema" {
        result["response_format"] = json!({"type":"json_schema","json_schema":{"name":"response","strict":true,"schema":input["output_config"]["format"]["schema"]}});
    }
    for key in ["mcp_servers", "container"] {
        if input.get(key).is_some_and(|v| !v.is_null()) {
            return Err(format!("不支持 Anthropic {key}，请使用客户端工具"));
        }
    }
    Ok(result)
}

/// Claude effort names aren't universally supported by WorkBuddy models. Use a
/// requested effort only when advertised, otherwise retain the model default.
pub fn apply_effort(input: &Value, chat: &mut Value, model: &Value) {
    let requested = input["output_config"]["effort"]
        .as_str()
        .or_else(|| input["reasoning_effort"].as_str());
    let choices = model["reasoning"]["supportedEfforts"].as_array();
    if let Some(effort) =
        requested.filter(|effort| choices.is_some_and(|a| a.iter().any(|v| v == *effort)))
    {
        chat["reasoning_effort"] = json!(effort);
    }
    if input["thinking"]["type"] == "disabled"
        && choices.is_some_and(|a| a.iter().any(|v| v == "none"))
    {
        chat["reasoning_effort"] = json!("none");
    }
}

pub fn usage(value: &Value) -> Value {
    let cached = value["prompt_tokens_details"]["cached_tokens"]
        .as_u64()
        .unwrap_or(0);
    json!({"input_tokens":value["prompt_tokens"].as_u64().unwrap_or(0).saturating_sub(cached),"output_tokens":value["completion_tokens"].as_u64().unwrap_or(0),"cache_read_input_tokens":cached,"cache_creation_input_tokens":0})
}
fn stop_reason(reason: &str) -> Result<&'static str, String> {
    match reason {
        "stop" => Ok("end_turn"),
        "length" => Ok("max_tokens"),
        "tool_calls" | "function_call" => Ok("tool_use"),
        "content_filter" => Ok("refusal"),
        _ => Err("上游缺少有效 finish_reason".into()),
    }
}
pub fn from_completion(value: &Value, model: &str) -> Result<Value, String> {
    let choice = value["choices"].get(0).ok_or("上游没有 choices")?;
    let message = &choice["message"];
    let mut content = vec![];
    if let Some(s) = message["reasoning_content"]
        .as_str()
        .filter(|s| !s.is_empty())
    {
        content.push(json!({"type":"thinking","thinking":s,"signature":""}));
    }
    if let Some(s) = message["content"].as_str().filter(|s| !s.is_empty()) {
        content.push(json!({"type":"text","text":s}));
    }
    if let Some(s) = message["refusal"].as_str().filter(|s| !s.is_empty()) {
        content.push(json!({"type":"text","text":s}));
    }
    for tool in message["tool_calls"].as_array().into_iter().flatten() {
        let args =
            serde_json::from_str::<Value>(tool["function"]["arguments"].as_str().unwrap_or("{}"))
                .map_err(|_| "上游工具参数不是有效 JSON")?;
        if !args.is_object() {
            return Err("上游工具参数必须是 JSON 对象".into());
        }
        content.push(json!({"type":"tool_use","id":tool["id"].as_str().ok_or("上游工具缺少 id")?,"name":tool["function"]["name"].as_str().ok_or("上游工具缺少 name")?,"input":args}));
    }
    Ok(
        json!({"id":format!("msg_{}",uuid::Uuid::new_v4().simple()),"type":"message","role":"assistant","model":model,"content":content,"stop_reason":stop_reason(choice["finish_reason"].as_str().unwrap_or(""))?,"stop_sequence":null,"usage":usage(&value["usage"])}),
    )
}

pub fn error_value(status: u16, message: &str) -> Value {
    let kind = match status {
        400 | 422 => "invalid_request_error",
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        413 => "request_too_large",
        429 => "rate_limit_error",
        503 | 529 => "overloaded_error",
        _ => "api_error",
    };
    json!({"type":"error","error":{"type":kind,"message":message}})
}
fn event(value: Value) -> Vec<u8> {
    format!(
        "event: {}\ndata: {value}\n\n",
        value["type"].as_str().unwrap_or("error")
    )
    .into_bytes()
}

#[derive(Default)]
struct Tool {
    id: String,
    name: String,
    args: String,
    block: Option<usize>,
}
pub struct Stream {
    pending: Vec<u8>,
    model: String,
    started: bool,
    ended: bool,
    failed: bool,
    saw_choice: bool,
    blocks: Vec<String>,
    text: Option<usize>,
    thinking: Option<usize>,
    tools: BTreeMap<u64, Tool>,
    finish: Option<String>,
    tokens: Value,
}
impl Stream {
    pub fn new(model: &str) -> Self {
        Self {
            pending: vec![],
            model: model.into(),
            started: false,
            ended: false,
            failed: false,
            saw_choice: false,
            blocks: vec![],
            text: None,
            thinking: None,
            tools: BTreeMap::new(),
            finish: None,
            tokens: usage(&Value::Null),
        }
    }
    pub fn succeeded(&self) -> bool {
        self.ended && !self.failed
    }
    pub fn ended(&self) -> bool {
        self.ended
    }
    fn start(&mut self, out: &mut Vec<u8>) {
        if self.started {
            return;
        }
        self.started = true;
        out.extend(event(json!({"type":"message_start","message":{"id":format!("msg_{}",uuid::Uuid::new_v4().simple()),"type":"message","role":"assistant","model":self.model,"content":[],"stop_reason":null,"stop_sequence":null,"usage":self.tokens}})));
    }
    fn block(&mut self, kind: &str, content: Value, out: &mut Vec<u8>) -> usize {
        let index = self.blocks.len();
        self.blocks.push(kind.into());
        out.extend(event(
            json!({"type":"content_block_start","index":index,"content_block":content}),
        ));
        index
    }
    fn delta(&mut self, kind: &str, value: &str, out: &mut Vec<u8>) {
        if value.is_empty() {
            return;
        }
        let index = if kind == "text" {
            if let Some(i) = self.text {
                i
            } else {
                let i = self.block(kind, json!({"type":"text","text":""}), out);
                self.text = Some(i);
                i
            }
        } else if let Some(i) = self.thinking {
            i
        } else {
            let i = self.block(
                kind,
                json!({"type":"thinking","thinking":"","signature":""}),
                out,
            );
            self.thinking = Some(i);
            i
        };
        let delta = if kind == "text" {
            json!({"type":"text_delta","text":value})
        } else {
            json!({"type":"thinking_delta","thinking":value})
        };
        out.extend(event(
            json!({"type":"content_block_delta","index":index,"delta":delta}),
        ));
    }
    fn tool(&mut self, index: u64, fragment: &Value, out: &mut Vec<u8>) -> Result<(), String> {
        let mut tool = self.tools.remove(&index).unwrap_or_default();
        if let Some(id) = fragment["id"].as_str() {
            tool.id.push_str(id);
        }
        if let Some(name) = fragment["function"]["name"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            if tool.block.is_some() {
                return Err("上游在工具参数之后修改了工具名称".into());
            }
            tool.name.push_str(name);
        }
        let args = fragment["function"]["arguments"].as_str().unwrap_or("");
        tool.args.push_str(args);
        if tool.args.len() > 1024 * 1024 {
            return Err("上游工具参数过大".into());
        }
        if !args.is_empty() && tool.block.is_none() && !tool.id.is_empty() && !tool.name.is_empty()
        {
            tool.block = Some(self.block(
                "tool_use",
                json!({"type":"tool_use","id":tool.id,"name":tool.name,"input":{}}),
                out,
            ));
            out.extend(event(json!({"type":"content_block_delta","index":tool.block.unwrap(),"delta":{"type":"input_json_delta","partial_json":tool.args}})));
        } else if let Some(i) = tool.block {
            if !args.is_empty() {
                out.extend(event(json!({"type":"content_block_delta","index":i,"delta":{"type":"input_json_delta","partial_json":args}})));
            }
        }
        self.tools.insert(index, tool);
        Ok(())
    }
    fn complete(&mut self, out: &mut Vec<u8>) -> Result<(), String> {
        if !self.saw_choice {
            return Err("上游没有聊天内容事件".into());
        }
        let reason = stop_reason(self.finish.as_deref().unwrap_or(""))?;
        let mut tools = std::mem::take(&mut self.tools);
        for tool in tools.values_mut() {
            if tool.id.is_empty() || tool.name.is_empty() {
                return Err("上游工具缺少名称或 id".into());
            }
            let args = if tool.args.is_empty() {
                "{}"
            } else {
                &tool.args
            };
            if !serde_json::from_str::<Value>(args).is_ok_and(|v| v.is_object()) {
                return Err("上游工具参数不是完整 JSON 对象".into());
            }
            if tool.block.is_none() {
                let i = self.block(
                    "tool_use",
                    json!({"type":"tool_use","id":tool.id,"name":tool.name,"input":{}}),
                    out,
                );
                out.extend(event(json!({"type":"content_block_delta","index":i,"delta":{"type":"input_json_delta","partial_json":args}})));
            }
        }
        for index in 0..self.blocks.len() {
            out.extend(event(json!({"type":"content_block_stop","index":index})));
        }
        out.extend(event(json!({"type":"message_delta","delta":{"stop_reason":reason,"stop_sequence":null},"usage":self.tokens})));
        out.extend(event(json!({"type":"message_stop"})));
        self.ended = true;
        Ok(())
    }
    fn frame(&mut self, frame: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
        let frame = std::str::from_utf8(frame).map_err(|_| "上游 SSE 不是有效 UTF-8")?;
        let data = frame
            .lines()
            .filter_map(|l| l.strip_prefix("data:").map(str::trim_start))
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            return Ok(());
        }
        if data == "[DONE]" {
            return self.complete(out);
        }
        let v: Value = serde_json::from_str(&data).map_err(|_| "上游 SSE 不是有效 JSON")?;
        if v.get("error").is_some_and(|v| !v.is_null())
            || (v.get("choices").is_none()
                && v.get("code")
                    .is_some_and(|c| !c.is_null() && c != 0 && c != 200))
        {
            return Err(v["error"]["message"]
                .as_str()
                .or_else(|| v["msg"].as_str())
                .unwrap_or("WorkBuddy SSE 返回错误")
                .into());
        }
        if v.get("usage").is_some_and(|u| !u.is_null()) {
            self.tokens = usage(&v["usage"]);
        }
        self.start(out);
        for c in v["choices"].as_array().into_iter().flatten() {
            if c["index"].as_u64().unwrap_or(0) != 0 {
                continue;
            }
            self.saw_choice = true;
            for key in ["reasoning_content", "content", "refusal"] {
                if let Some(s) = c["delta"][key].as_str() {
                    self.delta(
                        if key == "reasoning_content" {
                            "thinking"
                        } else {
                            "text"
                        },
                        s,
                        out,
                    );
                }
            }
            for t in c["delta"]["tool_calls"].as_array().into_iter().flatten() {
                self.tool(t["index"].as_u64().unwrap_or(0), t, out)?;
            }
            if let Some(reason) = c["finish_reason"].as_str() {
                self.finish = Some(reason.into());
            }
        }
        Ok(())
    }
    pub fn fail(&mut self, message: &str) -> Vec<u8> {
        if self.ended {
            return vec![];
        }
        self.ended = true;
        self.failed = true;
        event(error_value(502, message))
    }
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        if self.ended {
            return vec![];
        }
        self.pending.extend_from_slice(bytes);
        let mut out = vec![];
        loop {
            let lf = self
                .pending
                .windows(2)
                .position(|w| w == b"\n\n")
                .map(|i| (i, 2));
            let crlf = self
                .pending
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|i| (i, 4));
            let Some((index, size)) = lf.into_iter().chain(crlf).min_by_key(|(i, _)| *i) else {
                break;
            };
            if index > 1024 * 1024 {
                out.extend(self.fail("上游 SSE 事件过大"));
                return out;
            }
            let frame = self.pending.drain(..index + size).collect::<Vec<_>>();
            if let Err(e) = self.frame(&frame[..index], &mut out) {
                out.extend(self.fail(&e));
                return out;
            }
            if self.ended {
                self.pending.clear();
                break;
            }
        }
        if self.pending.len() > 1024 * 1024 {
            out.extend(self.fail("上游 SSE 事件过大"));
        }
        out
    }
    pub fn eof(&mut self) -> Vec<u8> {
        self.fail("上游流未完整结束，缺少 [DONE]")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request() -> Value {
        json!({"model":"kimi-k3-1","max_tokens":128,"messages":[{"role":"user","content":"你好"}]})
    }
    fn events(bytes: &[u8]) -> Vec<Value> {
        std::str::from_utf8(bytes)
            .unwrap()
            .split("\n\n")
            .filter_map(|f| {
                f.lines()
                    .find_map(|l| l.strip_prefix("data: "))
                    .map(|d| serde_json::from_str(d).unwrap())
            })
            .collect()
    }
    #[test]
    fn converts_system_images_tools_results_and_thinking_history() {
        let mut input = request();
        input["system"] =
            json!([{"type":"text","text":"系统","cache_control":{"type":"ephemeral"}}]);
        input["tools"] = json!([{"name":"read_file","description":"读取文件","input_schema":{"type":"object","properties":{"path":{"type":"string"}}},"cache_control":{"type":"ephemeral"}}]);
        input["tool_choice"] =
            json!({"type":"tool","name":"read_file","disable_parallel_tool_use":true});
        input["messages"] = json!([
            {"role":"user","content":[{"type":"text","text":"读取"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"dGVzdA=="}}]},
            {"role":"assistant","content":[{"type":"thinking","thinking":"分析","signature":""},{"type":"tool_use","id":"call_1","name":"read_file","input":{"path":"a.txt"}}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"call_1","content":"文件内容"},{"type":"text","text":"请总结"}]}
        ]);
        let out = to_chat(&input).unwrap();
        assert_eq!(out["messages"][0]["role"], "system");
        assert_eq!(
            out["messages"][1]["content"][1]["image_url"]["url"],
            "data:image/png;base64,dGVzdA=="
        );
        assert_eq!(out["messages"][2]["reasoning_content"], "分析");
        assert_eq!(
            serde_json::from_str::<Value>(
                out["messages"][2]["tool_calls"][0]["function"]["arguments"]
                    .as_str()
                    .unwrap()
            )
            .unwrap()["path"],
            "a.txt"
        );
        assert_eq!(out["messages"][3]["role"], "tool");
        assert_eq!(out["messages"][3]["tool_call_id"], "call_1");
        assert_eq!(out["messages"][4]["content"][0]["text"], "请总结");
        assert_eq!(out["tool_choice"]["function"]["name"], "read_file");
        assert_eq!(out["parallel_tool_calls"], false);
        assert!(out["tools"][0]["function"].get("cache_control").is_none());
    }
    #[test]
    fn unsupported_documents_and_server_tools_fail_before_upstream() {
        let mut input = request();
        input["messages"][0]["content"] = json!([{"type":"document","source":{}}]);
        assert!(to_chat(&input).unwrap_err().contains("document"));
        let mut input = request();
        input["tools"] = json!([{"type":"web_search_20250305","name":"web_search"}]);
        assert!(to_chat(&input).is_err());
        for invalid in [
            json!(null),
            json!([]),
            json!({"model":"a","max_tokens":0,"messages":[]}),
        ] {
            assert!(to_chat(&invalid).is_err());
        }
    }
    #[test]
    fn k3_medium_uses_declared_default_instead_of_unsupported_effort() {
        let mut input = request();
        input["output_config"] = json!({"effort":"medium"});
        let model =
            json!({"reasoning":{"defaultEffort":"high","supportedEfforts":["low","high","xhigh"]}});
        let mut chat = to_chat(&input).unwrap();
        apply_effort(&input, &mut chat, &model);
        assert!(chat.get("reasoning_effort").is_none());
        input["output_config"]["effort"] = json!("low");
        apply_effort(&input, &mut chat, &model);
        assert_eq!(chat["reasoning_effort"], "low");
    }
    #[test]
    fn claude_code_system_updates_are_merged_before_conversation_in_order() {
        let mut input = request();
        input["messages"].as_array_mut().unwrap().push(json!({"role":"system","content":[{"type":"text","text":"当前会话补充规则","cache_control":{"type":"ephemeral"}}]}));
        let out = to_chat(&input).unwrap();
        assert_eq!(out["messages"][0]["role"], "system");
        assert_eq!(out["messages"][1]["role"], "user");
        assert_eq!(out["messages"][0]["content"], "当前会话补充规则");
        input["system"] = json!("原始规则");
        assert_eq!(
            to_chat(&input).unwrap()["messages"][0]["content"],
            "原始规则\n\n当前会话补充规则"
        );
    }
    #[test]
    fn billing_metadata_is_excluded_without_rewriting_identity_or_user_content() {
        let mut input = request();
        let identity = "You are a Claude agent, built on Anthropic's Claude Agent SDK.";
        input["system"] = json!([
            {"type":"text","text":"x-anthropic-billing-header: cc_version=2.1.274; cc_entrypoint=sdk-cli;"},
            {"type":"text","text":identity},
            {"type":"text","text":"Follow the user's instructions."}
        ]);
        input["messages"][0]["content"] =
            json!("x-anthropic-billing-header: user supplied example");
        let chat = to_chat(&input).unwrap();
        assert_eq!(
            chat["messages"][0]["content"],
            format!("{identity}\nFollow the user's instructions.")
        );
        assert_eq!(
            chat["messages"][1]["content"],
            input["messages"][0]["content"]
        );
        input["system"] = json!(format!(
            "x-anthropic-billing-header: cc_version=2;\r\n{identity}"
        ));
        assert_eq!(to_chat(&input).unwrap()["messages"][0]["content"], identity);
        input["system"] = json!("Keep this literal example:\nx-anthropic-billing-header: example");
        assert_eq!(
            to_chat(&input).unwrap()["messages"][0]["content"],
            input["system"]
        );
        input["system"] = json!([{"type":"image","source":{}}]);
        assert!(to_chat(&input).is_err());
    }
    #[test]
    fn nonstream_completion_preserves_tools_usage_and_stop_reason() {
        let value = json!({"choices":[{"message":{"content":"调用工具","reasoning_content":"分析","tool_calls":[{"id":"call_a","function":{"name":"sum","arguments":"{\"a\":1}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":100,"completion_tokens":8,"prompt_tokens_details":{"cached_tokens":20}}});
        let result = from_completion(&value, "kimi-k3-1").unwrap();
        assert_eq!(result["type"], "message");
        assert_eq!(result["stop_reason"], "tool_use");
        assert_eq!(result["content"][2]["input"]["a"], 1);
        assert_eq!(result["usage"]["input_tokens"], 80);
        assert_eq!(result["usage"]["cache_read_input_tokens"], 20);
        let mut invalid = value;
        invalid["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] = json!("{");
        assert!(from_completion(&invalid, "k3").is_err());
    }
    #[test]
    fn incremental_stream_preserves_utf8_parallel_tools_and_late_usage() {
        let chunks = [
            json!({"choices":[{"index":0,"delta":{"reasoning_content":"想一想"}}]}),
            json!({"choices":[{"index":0,"delta":{"content":"你好"}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read_","arguments":""}},{"index":1,"id":"call_b","function":{"name":"sum","arguments":"{\"b\":"}}]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"name":"file","arguments":"{\"path\":"}},{"index":1,"function":{"arguments":"2}"}}]}}]}),
            json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.txt\"}"}}]},"finish_reason":"tool_calls"}]}),
            json!({"choices":[],"usage":{"prompt_tokens":27,"completion_tokens":12}}),
        ];
        let source = chunks
            .iter()
            .map(|c| format!("data: {c}\r\n\r\n"))
            .collect::<String>()
            + "data: [DONE]\r\n\r\n";
        let mut adapter = Stream::new("kimi-k3-1");
        let mut out = vec![];
        for b in source.as_bytes().chunks(1) {
            out.extend(adapter.feed(b));
        }
        let e = events(&out);
        assert_eq!(e.first().unwrap()["type"], "message_start");
        assert_eq!(e.last().unwrap()["type"], "message_stop");
        let starts = e
            .iter()
            .filter(|e| e["type"] == "content_block_start")
            .collect::<Vec<_>>();
        assert_eq!(starts.len(), 4);
        assert_eq!(
            e.iter()
                .filter(|e| e["type"] == "content_block_stop")
                .count(),
            4
        );
        let mut tool_inputs = BTreeMap::<u64, String>::new();
        for e in &e {
            if e["delta"]["type"] == "input_json_delta" {
                tool_inputs
                    .entry(e["index"].as_u64().unwrap())
                    .or_default()
                    .push_str(e["delta"]["partial_json"].as_str().unwrap());
            }
        }
        assert_eq!(tool_inputs.len(), 2);
        for input in tool_inputs.values() {
            assert!(serde_json::from_str::<Value>(input).unwrap().is_object());
        }
        let last = e.iter().find(|e| e["type"] == "message_delta").unwrap();
        assert_eq!(last["delta"]["stop_reason"], "tool_use");
        assert_eq!(last["usage"]["input_tokens"], 27);
        assert_eq!(last["usage"]["output_tokens"], 12);
        assert!(adapter.eof().is_empty());
    }
    #[test]
    fn errors_and_truncated_stream_never_emit_success_stop() {
        let mut adapter = Stream::new("hy3");
        let mut output =
            adapter.feed(b"data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\n");
        output.extend(adapter.eof());
        let e = events(&output);
        assert_eq!(e.last().unwrap()["type"], "error");
        assert!(!e.iter().any(|e| e["type"] == "message_stop"));
        let mut adapter = Stream::new("hy3");
        let e = events(&adapter.feed(b"data: {\"error\":{\"message\":\"quota\"}}\n\n"));
        assert_eq!(e[0]["error"]["message"], "quota");
        let mut adapter = Stream::new("hy3");
        assert_eq!(
            events(&adapter.feed(&vec![b'x'; 1024 * 1024 + 1]))[0]["type"],
            "error"
        );
    }
}

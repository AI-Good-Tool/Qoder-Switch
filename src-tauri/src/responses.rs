//! Stateless Responses compatibility. No server tools, opaque reasoning or stored chains.
use serde_json::{json, Value};
use std::collections::BTreeMap;

fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{key} 必须为非空字符串"))
}
fn content(v: &Value, images: bool) -> Result<Value, String> {
    if v.is_string() {
        return Ok(v.clone());
    }
    let rows = v.as_array().ok_or("content 必须是字符串或数组")?;
    let mut out = vec![];
    for p in rows {
        out.push(match p["type"].as_str() {
            Some("input_text" | "output_text" | "text") => json!({"type":"text","text":p["text"].as_str().ok_or("文本块缺少 text")?}),
            Some("input_image") if images => {
                let url = text(p, "image_url")?;
                if !url.starts_with("https://") && !["data:image/png;base64,", "data:image/jpeg;base64,", "data:image/webp;base64,", "data:image/gif;base64,"].iter().any(|s| url.starts_with(s)) {
                    return Err("图片仅支持 HTTPS URL 或受支持的 base64 图片".into());
                }
                let mut image = json!({"url":url});
                if let Some(detail) = p.get("detail") { image["detail"] = detail.clone(); }
                json!({"type":"image_url","image_url":image})
            }
            _ => return Err("Responses 暂仅支持文本、用户图片和客户端 function 工具；不支持文档、音视频或服务端工具".into()),
        });
    }
    Ok(json!(out))
}

pub fn normalize_chat(mut body: Value) -> Result<Value, String> {
    if !body.is_object() {
        return Err("请求体必须是 JSON 对象".into());
    }
    if body.get("stream").is_some_and(|v| !v.is_boolean()) {
        return Err("stream 必须是布尔值".into());
    }
    if let Some(max) = body.get("max_completion_tokens").cloned() {
        if max.as_u64().is_none_or(|n| n == 0) {
            return Err("max_completion_tokens 必须为正整数".into());
        }
        if body.get("max_tokens").is_some_and(|v| *v != max) {
            return Err("max_tokens 与 max_completion_tokens 冲突".into());
        }
        body["max_tokens"] = max;
        body.as_object_mut()
            .unwrap()
            .remove("max_completion_tokens");
    }
    if body
        .get("max_tokens")
        .is_some_and(|v| v.as_u64().is_none_or(|n| n == 0))
    {
        return Err("max_tokens 必须为正整数".into());
    }
    if let Some(rows) = body["messages"].as_array_mut() {
        let mut instructions = vec![];
        let mut ordinary = vec![];
        for row in rows.drain(..) {
            if row["role"] == "system" || row["role"] == "developer" {
                let value = content(&row["content"], false)?;
                let s = if let Some(s) = value.as_str() {
                    s.to_owned()
                } else {
                    value
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|p| p["text"].as_str().unwrap_or(""))
                        .collect::<Vec<_>>()
                        .join("\n")
                };
                instructions.push(s);
            } else {
                ordinary.push(row);
            }
        }
        if !instructions.is_empty() {
            rows.push(json!({"role":"system","content":instructions.join("\n\n")}));
        }
        rows.extend(ordinary);
    }
    Ok(body)
}

pub fn to_chat(input: &Value) -> Result<Value, String> {
    to_chat_with_tools(input).map(|(body, _)| body)
}

pub fn to_chat_with_tools(
    input: &Value,
) -> Result<(Value, crate::response_tools::ToolMap), String> {
    let (flat, tools) = crate::response_tools::flatten(input)?;
    to_chat_flat(&flat).map(|body| (body, tools))
}

fn to_chat_flat(input: &Value) -> Result<Value, String> {
    let model = text(input, "model")?;
    for key in ["previous_response_id", "conversation", "prompt"] {
        if input.get(key).is_some_and(|v| !v.is_null()) {
            return Err(format!(
                "当前为无状态 Responses；不支持 {key}，请回传完整 input 历史"
            ));
        }
    }
    for key in ["background", "store"] {
        if input.get(key).is_some_and(|v| !v.is_boolean()) {
            return Err(format!("{key} 必须是布尔值"));
        }
        if input[key] == true {
            return Err(format!("当前不支持 {key}=true"));
        }
    }
    if input.get("stream").is_some_and(|v| !v.is_boolean()) {
        return Err("stream 必须是布尔值".into());
    }
    if let Some(include) = input.get("include").filter(|v| !v.is_null()) {
        let fields = include.as_array().ok_or("include 必须为数组或 null")?;
        // Codex requests this optional output field even for third-party models.
        // Accept the selector without inventing encrypted state. Actual opaque
        // reasoning input remains rejected below because it cannot be decoded.
        if fields
            .iter()
            .any(|v| v.as_str() != Some("reasoning.encrypted_content"))
        {
            return Err("当前不支持此 Responses include 扩展".into());
        }
    }
    let mut rows = vec![];
    let mut calls = std::collections::BTreeSet::new();
    let mut pending_calls = std::collections::BTreeSet::new();
    if let Some(v) = input.get("instructions").filter(|v| !v.is_null()) {
        rows.push(
            json!({"role":"system","content":v.as_str().ok_or("instructions 必须是字符串")?}),
        );
    }
    let items = if let Some(s) = input["input"].as_str() {
        vec![json!({"role":"user","content":s})]
    } else {
        input["input"]
            .as_array()
            .filter(|a| !a.is_empty())
            .ok_or("input 必须是字符串或非空数组")?
            .clone()
    };
    for item in items {
        match item["type"].as_str().unwrap_or("message") {
            "message" => {
                let role = text(&item, "role")?;
                if !["system", "developer", "user", "assistant"].contains(&role) {
                    return Err("不支持此 Responses 消息角色".into());
                }
                let value = content(&item["content"], role == "user")?;
                // A Responses reasoning item and message are separate output
                // items, but belong to the same assistant message upstream.
                if role == "assistant" && rows.last().is_some_and(reasoning_only) {
                    rows.last_mut().unwrap()["content"] = value;
                } else {
                    rows.push(json!({"role":role,"content":value}));
                }
            }
            "function_call" => {
                let id = text(&item, "call_id")?;
                if !calls.insert(id.to_owned()) {
                    return Err("function_call 的 call_id 重复".into());
                }
                pending_calls.insert(id.to_owned());
                let name = text(&item, "name")?;
                let args = item["arguments"]
                    .as_str()
                    .ok_or("function_call.arguments 必须是 JSON 字符串")?;
                if !serde_json::from_str::<Value>(args).is_ok_and(|v| v.is_object()) {
                    return Err("function_call.arguments 必须编码 JSON 对象".into());
                }
                if !rows.last().is_some_and(|r| r["role"] == "assistant") {
                    rows.push(json!({"role":"assistant","content":null}));
                }
                let row = rows.last_mut().unwrap();
                if !row["tool_calls"].is_array() {
                    row["tool_calls"] = json!([]);
                }
                row["tool_calls"].as_array_mut().unwrap().push(
                    json!({"id":id,"type":"function","function":{"name":name,"arguments":args}}),
                );
            }
            "function_call_output" => {
                let id = text(&item, "call_id")?;
                if !pending_calls.remove(id) {
                    return Err(
                        "function_call_output 缺少匹配的调用，或结果重复；请提供完整历史".into(),
                    );
                }
                rows.push(json!({"role":"tool","tool_call_id":id,"content":content(&item["output"],false)?}));
            }
            "reasoning" => {
                if item.get("encrypted_content").is_some_and(|v| !v.is_null()) {
                    return Err("不支持跨提供商复用加密 reasoning，请使用完整明文上下文".into());
                }
                let parts = item["summary"]
                    .as_array()
                    .ok_or("reasoning 缺少 summary 数组")?;
                let mut summary = vec![];
                for part in parts {
                    if part["type"] != "summary_text" {
                        return Err("reasoning 仅支持 summary_text".into());
                    }
                    summary.push(part["text"].as_str().ok_or("summary_text 缺少 text")?);
                }
                let summary = summary.join("\n");
                if !summary.trim().is_empty() {
                    if let Some(last) = rows.last_mut().filter(|r| r["role"] == "assistant") {
                        let previous = last["reasoning_content"].as_str().unwrap_or("");
                        last["reasoning_content"] = json!(if previous.is_empty() {
                            summary
                        } else {
                            format!("{previous}\n{summary}")
                        });
                    } else {
                        rows.push(
                            json!({"role":"assistant","content":null,"reasoning_content":summary}),
                        );
                    }
                }
            }
            _ => {
                return Err(
                    "不支持 Responses 引用项或服务端工具项；请提供完整消息与客户端 function 调用"
                        .into(),
                )
            }
        }
    }
    if !pending_calls.is_empty() {
        return Err("历史 function_call 缺少结果，请先回传工具输出".into());
    }
    if rows.iter().any(reasoning_only) {
        return Err("历史思考摘要缺少对应的 assistant 正文或工具调用；请提供完整历史".into());
    }
    let mut chat =
        json!({"model":model,"messages":rows,"stream":input["stream"].as_bool().unwrap_or(false)});
    for key in ["temperature", "top_p", "parallel_tool_calls"] {
        if let Some(v) = input.get(key) {
            chat[key] = v.clone();
        }
    }
    if let Some(v) = input.get("max_output_tokens") {
        if v.as_u64().is_none_or(|n| n == 0) {
            return Err("max_output_tokens 必须是正整数".into());
        }
        chat["max_tokens"] = v.clone();
    }
    if let Some(effort) = input["reasoning"].get("effort").filter(|v| !v.is_null()) {
        chat["reasoning_effort"] = json!(effort.as_str().ok_or("reasoning.effort 必须是字符串")?);
    }
    if let Some(v) = input["text"].get("verbosity") {
        chat["verbosity"] = v.clone();
    }
    if let Some(format) = input["text"].get("format") {
        chat["response_format"] = match format["type"].as_str() {
            Some("text" | "json_object") => json!({"type":format["type"]}),
            Some("json_schema") if format["schema"].is_object() => {
                json!({"type":"json_schema","json_schema":{"name":text(format,"name")?,"schema":format["schema"],"strict":format["strict"].as_bool().unwrap_or(false)}})
            }
            _ => return Err("不支持此 text.format".into()),
        };
    }
    if let Some(tools) = input.get("tools") {
        let mut mapped = vec![];
        for (index, tool) in tools
            .as_array()
            .ok_or("tools 必须是数组")?
            .iter()
            .enumerate()
        {
            if tool["type"] != "function" {
                let kind = tool["type"].as_str().unwrap_or("missing");
                let kind: String = kind
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .take(48)
                    .collect();
                return Err(format!("tools[{index}] 类型 {kind} 暂不支持：当前支持 JSON function 工具；custom 自由文本工具、工具命名空间及服务端工具需要独立适配，不能直接丢弃"));
            }
            let parameters = match tool.get("parameters") {
                None => {
                    json!({"type":"object","properties":{},"required":[],"additionalProperties":false})
                }
                Some(value) if value.is_object() => value.clone(),
                _ => return Err(format!("tools[{index}].parameters 必须为 JSON 对象")),
            };
            let mut f = json!({"name":text(tool,"name")?,"parameters":parameters});
            for key in ["description", "strict"] {
                if let Some(v) = tool.get(key) {
                    f[key] = v.clone();
                }
            }
            mapped.push(json!({"type":"function","function":f}));
        }
        if !mapped.is_empty() {
            chat["tools"] = json!(mapped);
        }
    }
    if let Some(v) = input.get("tool_choice") {
        chat["tool_choice"] = if let Some(s) = v
            .as_str()
            .filter(|s| ["auto", "none", "required"].contains(s))
        {
            json!(s)
        } else if v["type"] == "function" {
            json!({"type":"function","function":{"name":text(v,"name")?}})
        } else {
            return Err("不支持此 tool_choice".into());
        };
    }
    normalize_chat(chat)
}

fn reasoning_only(row: &Value) -> bool {
    row["role"] == "assistant"
        && row["content"].is_null()
        && !row["tool_calls"].as_array().is_some_and(|v| !v.is_empty())
        && row["reasoning_content"].is_string()
}

fn usage(v: &Value) -> Value {
    let i = v["prompt_tokens"].as_u64().unwrap_or(0);
    let o = v["completion_tokens"].as_u64().unwrap_or(0);
    json!({"input_tokens":i,"output_tokens":o,"total_tokens":v["total_tokens"].as_u64().unwrap_or(i+o),"input_tokens_details":{"cached_tokens":v["prompt_tokens_details"]["cached_tokens"].as_u64().unwrap_or(0)},"output_tokens_details":{"reasoning_tokens":v["completion_tokens_details"]["reasoning_tokens"].as_u64().unwrap_or(0)}})
}

pub struct Stream {
    pending: Vec<u8>,
    frame: Vec<u8>,
    response: Value,
    sequence: u64,
    started: bool,
    ended: bool,
    finish: Option<String>,
    text_item: Option<usize>,
    reasoning_item: Option<usize>,
    tools: BTreeMap<u64, Tool>,
    retained: usize,
    tool_map: crate::response_tools::ToolMap,
}
#[derive(Default)]
struct Tool {
    item: Option<usize>,
    call: String,
    name: String,
    args: String,
}
impl Stream {
    pub fn new(model: &str) -> Self {
        Self {
            pending: vec![],
            frame: vec![],
            response: json!({"id":format!("resp_{}",uuid::Uuid::new_v4().simple()),"object":"response","created_at":chrono::Utc::now().timestamp(),"model":model,"status":"in_progress","output":[],"usage":null,"error":null,"incomplete_details":null,"store":false,"background":false,"parallel_tool_calls":true,"tools":[],"tool_choice":"auto","text":{"format":{"type":"text"}}}),
            sequence: 0,
            started: false,
            ended: false,
            finish: None,
            text_item: None,
            reasoning_item: None,
            tools: BTreeMap::new(),
            retained: 0,
            tool_map: Default::default(),
        }
    }
    pub fn succeeded(&self) -> bool {
        self.ended && self.response["status"] != "failed"
    }
    pub fn ended(&self) -> bool {
        self.ended
    }
    pub fn with_tools(model: &str, tools: crate::response_tools::ToolMap) -> Self {
        Self {
            tool_map: tools,
            ..Self::new(model)
        }
    }
    fn event(&mut self, v: Value, out: &mut Vec<u8>) {
        let events = match self.tool_map.1.events(v) {
            Ok(v) => v,
            Err(_) => {
                self.ended = true;
                self.response["status"] = json!("failed");
                self.response["error"] =
                    json!({"code":"custom_tool_error","message":"custom 工具参数包装无效"});
                vec![json!({"type":"response.failed","response":self.response})]
            }
        };
        for mut v in events {
            v["sequence_number"] = json!(self.sequence);
            self.sequence += 1;
            out.extend_from_slice(
                format!(
                    "event: {}\ndata: {}\n\n",
                    v["type"].as_str().unwrap_or("error"),
                    v
                )
                .as_bytes(),
            );
        }
    }
    fn start(&mut self, out: &mut Vec<u8>) {
        if !self.started {
            self.started = true;
            self.event(
                json!({"type":"response.created","response":self.response}),
                out,
            );
            self.event(
                json!({"type":"response.in_progress","response":self.response}),
                out,
            );
        }
    }
    fn add(&mut self, value: Value, out: &mut Vec<u8>) -> usize {
        let mut value = value;
        self.tool_map.restore(&mut value);
        let index = self.response["output"].as_array().unwrap().len();
        self.response["output"]
            .as_array_mut()
            .unwrap()
            .push(value.clone());
        self.event(
            json!({"type":"response.output_item.added","output_index":index,"item":value}),
            out,
        );
        index
    }
    fn chunk(&mut self, v: Value, out: &mut Vec<u8>) -> Result<(), String> {
        if v.get("error").is_some_and(|v| !v.is_null()) {
            return Err("上游返回错误事件；详情见请求记录".into());
        }
        if v["usage"].is_object() {
            self.response["usage"] = usage(&v["usage"]);
        }
        for c in v["choices"].as_array().into_iter().flatten() {
            if c["index"].as_u64().unwrap_or(0) != 0 {
                return Err("Responses 仅支持一个生成结果".into());
            }
            if let Some(finish) = c["finish_reason"].as_str() {
                self.finish = Some(finish.into());
            }
            let d = &c["delta"];
            for (key, reasoning) in [
                ("reasoning_content", true),
                ("content", false),
                ("refusal", false),
            ] {
                let Some(delta) = d[key].as_str().filter(|s| !s.is_empty()) else {
                    continue;
                };
                let index = if reasoning {
                    match self.reasoning_item {
                        Some(i) => i,
                        None => {
                            let i=self.add(json!({"id":format!("rs_{}",uuid::Uuid::new_v4().simple()),"type":"reasoning","summary":[]}),out);
                            self.event(json!({"type":"response.reasoning_summary_part.added","item_id":self.response["output"][i]["id"],"output_index":i,"summary_index":0,"part":{"type":"summary_text","text":""}}),out);
                            self.response["output"][i]["summary"] =
                                json!([{"type":"summary_text","text":""}]);
                            self.reasoning_item = Some(i);
                            i
                        }
                    }
                } else {
                    match self.text_item {
                        Some(i) => i,
                        None => {
                            let i=self.add(json!({"id":format!("msg_{}",uuid::Uuid::new_v4().simple()),"type":"message","role":"assistant","status":"in_progress","content":[]}),out);
                            let part = json!({"type":"output_text","text":"","annotations":[],"logprobs":[]});
                            self.event(json!({"type":"response.content_part.added","item_id":self.response["output"][i]["id"],"output_index":i,"content_index":0,"part":part}),out);
                            self.response["output"][i]["content"] = json!([part]);
                            self.text_item = Some(i);
                            i
                        }
                    }
                };
                let array = if reasoning { "summary" } else { "content" };
                let old = self.response["output"][index][array][0]["text"]
                    .as_str()
                    .unwrap_or("");
                self.response["output"][index][array][0]["text"] = json!(format!("{old}{delta}"));
                let mut event = json!({"type":if reasoning {"response.reasoning_summary_text.delta"} else {"response.output_text.delta"},"item_id":self.response["output"][index]["id"],"output_index":index,"delta":delta});
                event[if reasoning {
                    "summary_index"
                } else {
                    "content_index"
                }] = json!(0);
                self.event(event, out);
            }
            for t in d["tool_calls"].as_array().into_iter().flatten() {
                let key = t["index"].as_u64().unwrap_or(0);
                let mut tool = self.tools.remove(&key).unwrap_or_default();
                if t.get("type").is_some_and(|v| v != "function") {
                    return Err("上游返回不支持的工具类型".into());
                }
                if let Some(id) = t["id"].as_str() {
                    if tool.item.is_some() && tool.call != id {
                        return Err("流中工具调用 ID 改变".into());
                    }
                    tool.call = id.into();
                }
                if let Some(name) = t["function"]["name"].as_str().filter(|s| !s.is_empty()) {
                    if tool.item.is_some() {
                        return Err("工具参数开始后工具名仍变化".into());
                    }
                    tool.name.push_str(name);
                }
                let delta = t["function"]["arguments"].as_str().unwrap_or("");
                tool.args.push_str(delta);
                if !delta.is_empty() && tool.item.is_none() {
                    if tool.call.is_empty() || tool.name.is_empty() {
                        return Err("上游工具调用缺少 ID 或名称".into());
                    }
                    tool.item=Some(self.add(json!({"type":"function_call","id":format!("fc_{}",uuid::Uuid::new_v4().simple()),"call_id":tool.call,"name":tool.name,"arguments":"","status":"in_progress"}),out));
                }
                if let Some(i) = tool.item {
                    self.response["output"][i]["arguments"] = json!(tool.args);
                    if !delta.is_empty() {
                        self.event(json!({"type":"response.function_call_arguments.delta","item_id":self.response["output"][i]["id"],"output_index":i,"delta":delta}),out);
                    }
                }
                self.tools.insert(key, tool);
            }
        }
        Ok(())
    }
    fn complete(&mut self, out: &mut Vec<u8>) -> Result<(), String> {
        let incomplete = match self.finish.as_deref() {
            Some("stop" | "tool_calls" | "function_call") => None,
            Some("length") => Some("max_output_tokens"),
            Some("content_filter") => Some("content_filter"),
            _ => return Err("上游缺少有效 finish_reason".into()),
        };
        for tool in self.tools.values() {
            if incomplete.is_none()
                && (tool.item.is_none()
                    || !serde_json::from_str::<Value>(&tool.args).is_ok_and(|v| v.is_object()))
            {
                return Err("上游工具参数不是完整 JSON 对象".into());
            }
        }
        for i in 0..self.response["output"].as_array().unwrap().len() {
            let mut item = self.response["output"][i].clone();
            match item["type"].as_str() {
                Some("message") => {
                    self.event(json!({"type":"response.output_text.done","item_id":item["id"],"output_index":i,"content_index":0,"text":item["content"][0]["text"],"logprobs":[]}),out);
                    self.event(json!({"type":"response.content_part.done","item_id":item["id"],"output_index":i,"content_index":0,"part":item["content"][0]}),out);
                    item["status"] = json!(if incomplete.is_some() {
                        "incomplete"
                    } else {
                        "completed"
                    });
                }
                Some("reasoning") => {
                    self.event(json!({"type":"response.reasoning_summary_text.done","item_id":item["id"],"output_index":i,"summary_index":0,"text":item["summary"][0]["text"]}),out);
                    self.event(json!({"type":"response.reasoning_summary_part.done","item_id":item["id"],"output_index":i,"summary_index":0,"part":item["summary"][0]}),out);
                }
                Some("function_call") => {
                    self.event(json!({"type":"response.function_call_arguments.done","item_id":item["id"],"output_index":i,"arguments":item["arguments"]}),out);
                    item["status"] = json!(if incomplete.is_some() {
                        "incomplete"
                    } else {
                        "completed"
                    });
                }
                _ => {}
            }
            self.response["output"][i] = item.clone();
            self.event(
                json!({"type":"response.output_item.done","output_index":i,"item":item}),
                out,
            );
        }
        if self.ended {
            return Ok(());
        }
        self.response["status"] = json!(if incomplete.is_some() {
            "incomplete"
        } else {
            "completed"
        });
        if let Some(reason) = incomplete {
            self.response["incomplete_details"] = json!({"reason":reason});
        }
        self.event(json!({"type":if incomplete.is_some(){"response.incomplete"}else{"response.completed"},"response":self.response}),out);
        self.ended = true;
        Ok(())
    }
    pub fn fail(&mut self, message: &str) -> Vec<u8> {
        if self.ended {
            return vec![];
        }
        let mut out = vec![];
        self.start(&mut out);
        self.response["status"] = json!("failed");
        self.response["error"] = json!({"code":"upstream_stream_error","message":message});
        self.event(
            json!({"type":"response.failed","response":self.response}),
            &mut out,
        );
        self.ended = true;
        out
    }
    pub fn eof(&mut self) -> Vec<u8> {
        self.fail("上游流未完整结束，缺少 [DONE]")
    }
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<u8> {
        if self.ended {
            return vec![];
        }
        self.retained += bytes.len();
        if self.retained > 16 * 1024 * 1024 || self.pending.len() + bytes.len() > 2 * 1024 * 1024 {
            return self.fail("Responses 响应超过本地缓冲上限");
        }
        self.pending.extend_from_slice(bytes);
        let mut out = vec![];
        self.start(&mut out);
        while let Some(n) = self.pending.iter().position(|b| *b == b'\n') {
            let mut line = self.pending.drain(..=n).collect::<Vec<_>>();
            while line.last().is_some_and(|b| *b == b'\n' || *b == b'\r') {
                line.pop();
            }
            if let Some(data) = line.strip_prefix(b"data:") {
                if !self.frame.is_empty() {
                    self.frame.push(b'\n');
                }
                self.frame
                    .extend_from_slice(data.strip_prefix(b" ").unwrap_or(data));
                if self.frame.len() > 2 * 1024 * 1024 {
                    out.extend(self.fail("上游 SSE 事件过大"));
                    break;
                }
            }
            if !line.is_empty() || self.frame.is_empty() {
                continue;
            }
            let data = std::mem::take(&mut self.frame);
            let result = if data == b"[DONE]" {
                self.complete(&mut out)
            } else {
                serde_json::from_slice::<Value>(&data)
                    .map_err(|_| "上游 SSE 包含无效 JSON".to_owned())
                    .and_then(|v| self.chunk(v, &mut out))
            };
            if let Err(e) = result {
                out.extend(self.fail(&e));
            }
            if self.ended {
                break;
            }
        }
        out
    }
}

pub fn from_completion(value: &Value, model: &str) -> Result<Value, String> {
    from_completion_with_tools(value, model, Default::default())
}
pub fn from_completion_with_tools(
    value: &Value,
    model: &str,
    tools: crate::response_tools::ToolMap,
) -> Result<Value, String> {
    let choice = value["choices"].get(0).ok_or("上游缺少 choices")?;
    let mut delta = choice["message"].clone();
    if let Some(calls) = delta["tool_calls"].as_array_mut() {
        for (i, t) in calls.iter_mut().enumerate() {
            t["index"] = json!(i);
        }
    }
    let chunk = json!({"choices":[{"index":0,"delta":delta,"finish_reason":choice["finish_reason"]}],"usage":value["usage"]});
    let mut s = Stream::with_tools(model, tools);
    s.feed(format!("data: {chunk}\n\ndata: [DONE]\n\n").as_bytes());
    if s.response["status"] == "failed" {
        return Err(s.response["error"]["message"]
            .as_str()
            .unwrap_or("响应适配失败")
            .into());
    }
    s.tool_map.1.restore_response(&mut s.response)?;
    Ok(s.response)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn events(bytes: &[u8]) -> Vec<Value> {
        String::from_utf8(bytes.to_vec())
            .unwrap()
            .lines()
            .filter_map(|s| s.strip_prefix("data: "))
            .map(|s| serde_json::from_str(s).unwrap())
            .collect()
    }
    fn wire(chunks: Vec<Value>, done: bool) -> Vec<u8> {
        let mut s = chunks
            .iter()
            .map(|v| format!("data: {v}\r\n\r\n"))
            .collect::<String>();
        if done {
            s.push_str("data: [DONE]\r\n\r\n");
        }
        s.into_bytes()
    }
    #[test]
    fn input_maps_instructions_images_parallel_tool_history_and_schema() {
        let body = json!({"model":"chosen-model","store":false,"instructions":"identity stays","input":[
            {"role":"developer","content":[{"type":"input_text","text":"project rules"}]},
            {"role":"user","content":[{"type":"input_text","text":"你好"},{"type":"input_image","image_url":"https://example.com/image.png"}]},
            {"type":"reasoning","summary":[{"type":"summary_text","text":"provided reasoning"}]},
            {"type":"function_call","call_id":"a","name":"read","arguments":"{\"x\":1}"},
            {"type":"function_call","call_id":"b","name":"read","arguments":"{}"},
            {"type":"function_call_output","call_id":"a","output":"result A"},
            {"type":"function_call_output","call_id":"b","output":[{"type":"input_text","text":"result B"}]}
        ],"tools":[{"type":"function","name":"read","parameters":{"type":"object"},"strict":false}],"tool_choice":{"type":"function","name":"read"},"max_output_tokens":512,"reasoning":{"effort":"high"},"text":{"format":{"type":"json_schema","name":"answer","schema":{"type":"object"},"strict":true}},"stream":true});
        let out = to_chat(&body).unwrap();
        assert_eq!(out["model"], "chosen-model");
        assert_eq!(out["max_tokens"], 512);
        assert_eq!(
            out["messages"][0]["content"],
            "identity stays\n\nproject rules"
        );
        assert_eq!(out["messages"][1]["content"][1]["type"], "image_url");
        assert_eq!(
            out["messages"][2]["tool_calls"].as_array().unwrap().len(),
            2
        );
        assert_eq!(
            out["messages"][2]["reasoning_content"],
            "provided reasoning"
        );
        assert_eq!(out["messages"][3]["tool_call_id"], "a");
        assert_eq!(out["tools"][0]["function"]["name"], "read");
        assert_eq!(out["response_format"]["json_schema"]["name"], "answer");
        assert_eq!(out["tool_choice"]["function"]["name"], "read");
        assert!(out.get("store").is_none());
    }
    #[test]
    fn unsupported_state_and_native_server_features_fail_explicitly() {
        let base = json!({"model":"m","input":"hello"});
        for patch in [
            json!({"previous_response_id":"resp_old"}),
            json!({"store":true}),
            json!({"background":true}),
            json!({"conversation":"c"}),
            json!({"include":["file_search_call.results"]}),
            json!({"tools":[{"type":"web_search"}]}),
            json!({"input":[{"type":"reasoning","summary":[],"encrypted_content":"opaque"}]}),
            json!({"input":[{"role":"user","content":[{"type":"input_file","file_id":"f"}]}]}),
            json!({"stream":"yes"}),
            json!({"max_output_tokens":0}),
        ] {
            let mut value = base.clone();
            for (k, v) in patch.as_object().unwrap() {
                value[k] = v.clone();
            }
            assert!(to_chat(&value).is_err(), "{patch}");
        }
        assert!(to_chat(&json!({"model":"m","input":[{"type":"function_call_output","call_id":"orphan","output":"x"}]})).is_err());
    }
    #[test]
    fn parameterless_functions_and_unsupported_tool_types_are_distinguished() {
        let chat = to_chat(&json!({"model":"m","input":"hello","tools":[{"type":"function","name":"ping","strict":true}]})).unwrap();
        assert_eq!(chat["tools"][0]["function"]["parameters"]["type"], "object");
        assert_eq!(
            chat["tools"][0]["function"]["parameters"]["required"],
            json!([])
        );
        assert_eq!(
            chat["tools"][0]["function"]["parameters"]["additionalProperties"],
            false
        );
        assert_eq!(chat["tools"][0]["function"]["strict"], true);
        // A no-argument call must survive both JSON/SSE output and the next
        // request's call/result replay without changing its call ID or payload.
        let source = wire(
            vec![
                json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"ping-call","type":"function","function":{"name":"ping","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}),
            ],
            true,
        );
        let mut stream = Stream::new("m");
        let events = events(&stream.feed(&source));
        let streamed = &events.last().unwrap()["response"];
        let completion = json!({"choices":[{"message":{"tool_calls":[{"id":"ping-call","type":"function","function":{"name":"ping","arguments":"{}"}}]},"finish_reason":"tool_calls"}]});
        for response in [streamed.clone(), from_completion(&completion, "m").unwrap()] {
            let call = response["output"][0].clone();
            assert_eq!(call["arguments"], "{}");
            let replay = to_chat(&json!({"model":"m","input":[{"role":"user","content":"ping"},call,{"type":"function_call_output","call_id":"ping-call","output":"pong"}]})).unwrap();
            assert_eq!(replay["messages"][1]["tool_calls"][0]["id"], "ping-call");
            assert_eq!(replay["messages"][2]["tool_call_id"], "ping-call");
        }
        for kind in ["web_search", "file_search"] {
            let error =
                to_chat(&json!({"model":"m","input":"hello","tools":[{"type":kind,"name":"x"}]}))
                    .unwrap_err();
            assert!(error.contains("tools[0]") && error.contains(kind));
        }
        assert!(to_chat(&json!({"model":"m","input":"hello","tools":[{"type":"function","name":"ping","parameters":null}]})).unwrap_err().contains("parameters"));
    }
    #[test]
    fn reasoning_text_and_tool_history_form_one_nonempty_assistant_message() {
        for message in [true, false] {
            let mut items = vec![
                json!({"role":"user","content":"run once"}),
                json!({"type":"reasoning","summary":[{"type":"summary_text","text":"plan"}]}),
            ];
            if message {
                items.push(
                    json!({"role":"assistant","content":[{"type":"output_text","text":"running"}]}),
                );
            }
            items.extend([
                json!({"type":"function_call","call_id":"id","name":"run","arguments":"{}"}),
                json!({"type":"function_call_output","call_id":"id","output":"OK"}),
            ]);
            let chat = to_chat(&json!({"model":"kimi-k3-1","input":items})).unwrap();
            assert_eq!(chat["messages"].as_array().unwrap().len(), 3);
            assert_eq!(chat["messages"][1]["reasoning_content"], "plan");
            assert_eq!(chat["messages"][1]["tool_calls"][0]["id"], "id");
            assert!(!chat["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(reasoning_only));
        }
        let chat = to_chat(&json!({"model":"m","input":[{"type":"reasoning","summary":[{"type":"summary_text","text":"plan"}]},{"role":"assistant","content":"answer"}]})).unwrap();
        assert_eq!(chat["messages"].as_array().unwrap().len(), 1);
        assert_eq!(chat["messages"][0]["content"], "answer");
        assert!(to_chat(&json!({"model":"m","input":[{"type":"reasoning","summary":[{"type":"summary_text","text":"orphan"}]}]})).is_err());
        let chat = to_chat(&json!({"model":"m","input":[{"type":"reasoning","summary":[{"type":"summary_text","text":""}]},{"role":"user","content":"hello"}]})).unwrap();
        assert_eq!(chat["messages"].as_array().unwrap().len(), 1);
    }
    #[test]
    fn namespace_functions_roundtrip_json_sse_and_reordered_history() {
        let tools = json!([
            {"type":"function","name":"wbns_0","parameters":{"type":"object"}},
            {"type":"namespace","name":"left","description":"left rules","tools":[{"type":"function","name":"read","parameters":{"type":"object"}}]},
            {"type":"namespace","name":"right","tools":[{"type":"function","name":"read"}]}
        ]);
        let input = json!({"model":"keep","input":"hello","tools":tools,"tool_choice":{"type":"function","namespace":"right","name":"read"}});
        let (chat, mapping) = to_chat_with_tools(&input).unwrap();
        assert_eq!(chat["tools"].as_array().unwrap().len(), 3);
        let alias = chat["tools"][2]["function"]["name"].as_str().unwrap();
        assert_ne!(alias, "wbns_0");
        assert_ne!(chat["tools"][1]["function"]["name"], alias);
        assert_eq!(chat["tool_choice"]["function"]["name"], alias);
        assert!(chat["tools"][1]["function"]["description"]
            .as_str()
            .unwrap()
            .contains("left rules"));
        let delta = json!({"tool_calls":[{"index":0,"id":"call_n","type":"function","function":{"name":alias,"arguments":"{}"}}]});
        let bytes = wire(
            vec![json!({"choices":[{"index":0,"delta":delta,"finish_reason":"tool_calls"}]})],
            true,
        );
        let mut stream = Stream::with_tools("keep", mapping.clone());
        let mut out = vec![];
        for byte in bytes.chunks(1) {
            out.extend(stream.feed(byte));
        }
        let events = events(&out);
        for event in &events {
            if event["item"]["type"] == "function_call" {
                assert_eq!(event["item"]["namespace"], "right");
                assert_eq!(event["item"]["name"], "read");
            }
        }
        assert_eq!(events.last().unwrap()["type"], "response.completed");
        let completion = json!({"choices":[{"message":delta,"finish_reason":"tool_calls"}]});
        let json_response = from_completion_with_tools(&completion, "keep", mapping).unwrap();
        for response in [json_response, events.last().unwrap()["response"].clone()] {
            let call = response["output"][0].clone();
            assert_eq!(call["namespace"], "right");
            assert_eq!(call["name"], "read");
            // A newly introduced namespace changes alias numbering. Histories
            // are remapped by identity, never by yesterday's alias.
            let replay = to_chat(&json!({"model":"keep","tools":[{"type":"namespace","name":"aaa","tools":[{"type":"function","name":"new"}]},tools[2]],"input":[{"role":"user","content":"x"},call,{"type":"function_call_output","call_id":"call_n","output":"OK"}]})).unwrap();
            assert_eq!(
                replay["messages"][1]["tool_calls"][0]["function"]["name"],
                replay["tools"][1]["function"]["name"]
            );
            assert_eq!(replay["messages"][2]["tool_call_id"], "call_n");
        }
    }
    #[test]
    fn malformed_namespaces_fail_and_removed_tools_remain_in_history() {
        for tools in [
            json!([{"type":"namespace","name":"n","tools":[]}]),
            json!([{"type":"namespace","name":"n","tools":[{"type":"namespace","name":"x"}]}]),
            json!([{"type":"namespace","name":"n","tools":[{"type":"function","name":"x"},{"type":"function","name":"x"}]}]),
        ] {
            assert!(to_chat(&json!({"model":"m","input":"x","tools":tools})).is_err());
        }
        assert!(to_chat(&json!({"model":"m","input":[{"type":"function_call","namespace":7,"name":"x","call_id":"id","arguments":"{}"}]})).is_err());
        let body = json!({"model":"m","input":[{"type":"function_call","namespace":"removed","name":"x","call_id":"id","arguments":"{}"},{"type":"function_call_output","call_id":"id","output":"done"}]});
        assert!(
            to_chat(&body).unwrap()["messages"][0]["tool_calls"][0]["function"]["name"]
                .as_str()
                .unwrap()
                .starts_with("wbns_")
        );
    }
    #[test]
    fn optional_encrypted_output_selector_does_not_require_encrypted_state() {
        for include in [
            json!(null),
            json!([]),
            json!(["reasoning.encrypted_content"]),
        ] {
            let chat = to_chat(&json!({"model":"keep","input":"hello","include":include})).unwrap();
            assert_eq!(chat["model"], "keep");
            assert_eq!(chat["messages"][0]["content"], "hello");
            assert!(chat.get("include").is_none());
            assert!(!chat.to_string().contains("encrypted_content"));
        }
        for include in [
            json!("reasoning.encrypted_content"),
            json!([7]),
            json!(["reasoning.encrypted_content", "unknown"]),
        ] {
            assert!(to_chat(&json!({"model":"m","input":"hello","include":include})).is_err());
        }
        assert!(to_chat(&json!({"model":"m","include":["reasoning.encrypted_content"],"input":[{"type":"reasoning","summary":[],"encrypted_content":"opaque"}]})).is_err());
    }
    #[test]
    fn chat_token_alias_and_developer_conversion_do_not_rewrite_identity() {
        let v=normalize_chat(json!({"model":"keep","max_completion_tokens":16,"messages":[{"role":"developer","content":"You are ZCode"},{"role":"user","content":"x"},{"role":"system","content":"late rule"}]})).unwrap();
        assert_eq!(v["model"], "keep");
        assert_eq!(v["max_tokens"], 16);
        assert!(v.get("max_completion_tokens").is_none());
        assert_eq!(v["messages"][0]["content"], "You are ZCode\n\nlate rule");
        assert!(normalize_chat(json!({"max_tokens":12,"max_completion_tokens":16})).is_err());
    }
    #[test]
    fn streaming_utf8_text_reasoning_late_usage_and_sequences_are_consistent() {
        let bytes = wire(
            vec![
                json!({"choices":[{"index":0,"delta":{"reasoning_content":"思考","content":"你好"}}]}),
                json!({"choices":[{"index":0,"delta":{"content":"世界"},"finish_reason":"stop"}]}),
                json!({"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":4,"total_tokens":16,"prompt_tokens_details":{"cached_tokens":3}}}),
            ],
            true,
        );
        let mut adapter = Stream::new("m");
        let mut output = vec![];
        for b in bytes.chunks(1) {
            output.extend(adapter.feed(b));
        }
        output.extend(adapter.eof());
        let e = events(&output);
        for (i, v) in e.iter().enumerate() {
            assert_eq!(v["sequence_number"], i);
        }
        let completed = e.last().unwrap();
        assert_eq!(completed["type"], "response.completed");
        assert_eq!(
            completed["response"]["output"][1]["content"][0]["text"],
            "你好世界"
        );
        assert_eq!(
            completed["response"]["usage"]["input_tokens_details"]["cached_tokens"],
            3
        );
        assert_eq!(
            e.iter().filter(|v| v["type"] == "response.created").count(),
            1
        );
    }
    #[test]
    fn parallel_function_streams_roundtrip_without_changing_call_ids() {
        let bytes = wire(
            vec![
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"a","type":"function","function":{"name":"read","arguments":""}}]}}]}),
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"_file","arguments":"{\"x\":"}},{"index":1,"id":"b","type":"function","function":{"name":"list","arguments":"{}"}}]}}]}),
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"1}"}}]},"finish_reason":"tool_calls"}]}),
            ],
            true,
        );
        let mut adapter = Stream::new("m");
        let out = adapter.feed(&bytes);
        let e = events(&out);
        let response = &e.last().unwrap()["response"];
        assert_eq!(response["status"], "completed");
        assert_eq!(response["output"][0]["name"], "read_file");
        let mut history = vec![json!({"role":"user","content":"run tools"})];
        history.extend(response["output"].as_array().unwrap().clone());
        history.extend([
            json!({"type":"function_call_output","call_id":"a","output":"A"}),
            json!({"type":"function_call_output","call_id":"b","output":"B"}),
        ]);
        let chat = to_chat(&json!({"model":"m","input":history})).unwrap();
        assert_eq!(chat["messages"][1]["tool_calls"][0]["id"], "a");
        assert_eq!(chat["messages"][2]["tool_call_id"], "a");
    }
    #[test]
    fn incomplete_and_failed_streams_never_emit_completed() {
        for (bytes, eof) in [
            (
                wire(
                    vec![
                        json!({"choices":[{"delta":{"content":"partial"},"finish_reason":"length"}]}),
                    ],
                    true,
                ),
                false,
            ),
            (
                wire(
                    vec![json!({"choices":[{"delta":{"content":"partial"}}]})],
                    false,
                ),
                true,
            ),
            (
                wire(vec![json!({"error":{"message":"NEVER_ECHO_SECRET"}})], true),
                false,
            ),
            (b"data: not-json\n\n".to_vec(), false),
            (wire(vec![json!({"choices":[{"delta":{}}]})], true), false),
        ] {
            let mut s = Stream::new("m");
            let mut out = s.feed(&bytes);
            if eof {
                out.extend(s.eof());
            }
            let string = String::from_utf8(out).unwrap();
            assert!(!string.contains("event: response.completed"));
            assert!(!string.contains("NEVER_ECHO_SECRET"));
            assert!(s.ended());
            assert!(
                string.contains("event: response.incomplete")
                    || string.contains("event: response.failed")
            );
        }
    }
    #[test]
    fn multiline_sse_frames_and_nonstream_objects_use_same_output_contract() {
        let mut s = Stream::new("m");
        let out=s.feed(b"data: {\"choices\":\ndata: [{\"delta\":{\"content\":\"ok\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n");
        assert_eq!(events(&out).last().unwrap()["type"], "response.completed");
        let v=from_completion(&json!({"choices":[{"message":{"content":"ok"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2,"completion_tokens":1}}),"m").unwrap();
        assert_eq!(v["status"], "completed");
        assert_eq!(v["output"][0]["content"][0]["text"], "ok");
        assert_eq!(v["usage"]["total_tokens"], 3);
        assert!(from_completion(&json!({"choices":[{"message":{"content":"x"}}]}), "m").is_err());
    }
}

//! Reversible Responses custom-tool bridge. Raw tool input is encoded as one JSON string.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Default)]
pub struct CustomTools {
    definitions: BTreeSet<(String, String)>,
    items: BTreeMap<String, Value>,
}
impl CustomTools {
    pub fn prepare(input: &Value) -> Result<(Value, Self), String> {
        let mut body = input.clone();
        let mut map = Self::default();
        if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
            for tool in tools {
                if tool["type"] == "namespace" {
                    let ns = tool["name"]
                        .as_str()
                        .ok_or("namespace 缺少 name")?
                        .to_owned();
                    for child in tool["tools"].as_array_mut().ok_or("namespace 缺少 tools")? {
                        map.definition(child, &ns)?;
                    }
                } else {
                    map.definition(tool, "")?;
                }
            }
        }
        if let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) {
            for item in items {
                match item["type"].as_str() {
                    Some("custom_tool_call") => {
                        let name = item["name"]
                            .as_str()
                            .ok_or("custom_tool_call 缺少 name")?
                            .to_owned();
                        let ns = item["namespace"].as_str().unwrap_or("").to_owned();
                        map.definitions.insert((ns, name));
                        let text = item["input"]
                            .as_str()
                            .ok_or("custom_tool_call.input 必须为字符串")?
                            .to_owned();
                        item["type"] = json!("function_call");
                        item["arguments"] = json!(json!({"input":text}).to_string());
                        item.as_object_mut().unwrap().remove("input");
                    }
                    Some("custom_tool_call_output") => item["type"] = json!("function_call_output"),
                    _ => {}
                }
            }
        }
        if body["tool_choice"]["type"] == "custom" {
            body["tool_choice"]["type"] = json!("function");
        }
        Ok((body, map))
    }
    fn definition(&mut self, tool: &mut Value, ns: &str) -> Result<(), String> {
        if tool["type"] != "custom" {
            return Ok(());
        }
        let name = tool["name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("custom 工具缺少 name")?
            .to_owned();
        if !self.definitions.insert((ns.into(), name.clone())) {
            return Err("custom 工具名称重复".into());
        }
        let mut description = tool["description"].as_str().unwrap_or("").to_owned();
        description.push_str("\nPass the complete raw tool input verbatim in the required string field `input`; preserve every newline and character.");
        if let Some(format) = tool.get("format") {
            if !format.is_object() {
                return Err("custom 工具 format 必须为对象".into());
            }
            if format.to_string().len() > 256 * 1024 {
                return Err("custom 工具语法描述过大".into());
            }
            description.push_str("\nRaw tool format constraint: ");
            description.push_str(&format.to_string());
        }
        *tool = json!({"type":"function","name":name,"description":description,"strict":true,"parameters":{"type":"object","properties":{"input":{"type":"string"}},"required":["input"],"additionalProperties":false}});
        Ok(())
    }
    fn is_custom(&self, item: &Value) -> bool {
        item["type"] == "function_call"
            && self.definitions.contains(&(
                item["namespace"].as_str().unwrap_or("").into(),
                item["name"].as_str().unwrap_or("").into(),
            ))
    }
    fn restore_item(&self, item: &mut Value, complete: bool) -> Result<(), String> {
        if !self.is_custom(item) {
            return Ok(());
        }
        let args = item["arguments"].as_str().unwrap_or("");
        let text = if complete {
            let v: Value =
                serde_json::from_str(args).map_err(|_| "custom 工具返回无效 JSON 包装")?;
            if v.as_object().is_none_or(|o| o.len() != 1) {
                return Err("custom 工具包装必须仅包含 input".into());
            }
            v["input"]
                .as_str()
                .ok_or("custom 工具包装 input 必须为字符串")?
                .to_owned()
        } else {
            String::new()
        };
        item["type"] = json!("custom_tool_call");
        item["input"] = json!(text);
        item.as_object_mut().unwrap().remove("arguments");
        Ok(())
    }
    pub fn restore_response(&self, response: &mut Value) -> Result<(), String> {
        if let Some(items) = response["output"].as_array_mut() {
            for item in items {
                self.restore_item(item, true)?;
            }
        }
        Ok(())
    }
    pub fn events(&mut self, mut event: Value) -> Result<Vec<Value>, String> {
        match event["type"].as_str().unwrap_or("") {
            "response.output_item.added" => {
                if self.is_custom(&event["item"]) {
                    let id = event["item"]["id"].as_str().unwrap_or("").to_owned();
                    self.items.insert(id, event["item"].clone());
                    self.restore_item(&mut event["item"], false)?;
                }
            }
            "response.function_call_arguments.delta" => {
                if self
                    .items
                    .contains_key(event["item_id"].as_str().unwrap_or(""))
                {
                    return Ok(vec![]);
                }
            }
            "response.function_call_arguments.done" => {
                if let Some(item) = self.items.get(event["item_id"].as_str().unwrap_or("")) {
                    let mut item = item.clone();
                    item["arguments"] = event["arguments"].clone();
                    self.restore_item(&mut item, true)?;
                    let mut delta = event.clone();
                    delta["type"] = json!("response.custom_tool_call_input.delta");
                    delta["delta"] = item["input"].clone();
                    delta.as_object_mut().unwrap().remove("arguments");
                    event["type"] = json!("response.custom_tool_call_input.done");
                    event["input"] = item["input"].clone();
                    event.as_object_mut().unwrap().remove("arguments");
                    return Ok(vec![delta, event]);
                }
            }
            "response.output_item.done" => self.restore_item(&mut event["item"], true)?,
            "response.completed" | "response.incomplete" => {
                self.restore_response(&mut event["response"])?
            }
            _ => {}
        }
        Ok(vec![event])
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn custom_history_preserves_raw_patch() {
        let raw = "*** Begin Patch\n+你好\n*** End Patch";
        let (v,m)=CustomTools::prepare(&json!({"tools":[{"type":"custom","name":"apply_patch","format":{"type":"grammar","syntax":"lark","definition":"start: /.+/"}}],"input":[{"type":"custom_tool_call","call_id":"c","name":"apply_patch","input":raw},{"type":"custom_tool_call_output","call_id":"c","output":"ok"}]})).unwrap();
        assert_eq!(v["tools"][0]["type"], "function");
        assert_eq!(
            serde_json::from_str::<Value>(v["input"][0]["arguments"].as_str().unwrap()).unwrap()
                ["input"],
            raw
        );
        let mut r = json!({"output":[{"type":"function_call","name":"apply_patch","arguments":json!({"input":raw}).to_string()}]});
        m.restore_response(&mut r).unwrap();
        assert_eq!(r["output"][0]["input"], raw);
    }
}

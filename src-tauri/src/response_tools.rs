//! Request-local aliases for Responses namespace functions. No global state.
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Default)]
pub struct ToolMap(
    BTreeMap<String, (String, String)>,
    pub crate::custom_tools::CustomTools,
);

fn name(value: &Value) -> Result<String, String> {
    value
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .map(str::to_owned)
        .ok_or_else(|| "工具和命名空间名称必须为 1–128 字节字符串".into())
}

impl ToolMap {
    fn alias(&self, namespace: &str, name: &str) -> Result<String, String> {
        self.0
            .iter()
            .find(|(_, pair)| pair.0 == namespace && pair.1 == name)
            .map(|(alias, _)| alias.clone())
            .ok_or_else(|| "命名空间工具未声明或无法映射".into())
    }
    pub fn restore(&self, item: &mut Value) {
        if item["type"] == "function_call" {
            if let Some((namespace, name)) = item["name"].as_str().and_then(|n| self.0.get(n)) {
                item["namespace"] = json!(namespace);
                item["name"] = json!(name);
            }
        }
    }
}

pub fn flatten(input: &Value) -> Result<(Value, ToolMap), String> {
    if !input.is_object() {
        return Err("请求体必须为 JSON 对象".into());
    }
    let (prepared, custom) = crate::custom_tools::CustomTools::prepare(input)?;
    let input = &prepared;
    let mut flat = input.clone();
    let mut definitions = vec![];
    let mut pairs = BTreeSet::new();
    let mut reserved = BTreeSet::new();
    let mut namespaces = BTreeSet::new();
    let mut expanded_bytes = 0usize;
    if let Some(tools) = input.get("tools") {
        for (i, tool) in tools
            .as_array()
            .ok_or("tools 必须是数组")?
            .iter()
            .enumerate()
        {
            if tool["type"] == "namespace" {
                let namespace = name(&tool["name"])?;
                if !namespaces.insert(namespace.clone()) {
                    return Err(format!("tools[{i}] 命名空间重复"));
                }
                let children = tool["tools"]
                    .as_array()
                    .ok_or("namespace.tools 必须为数组")?;
                if children.is_empty() {
                    return Err("namespace.tools 不能为空".into());
                }
                for (j, child) in children.iter().enumerate() {
                    if child["type"] != "function" {
                        return Err(format!("tools[{i}].tools[{j}]：namespace 当前仅支持 function；custom 和嵌套 namespace 尚不支持"));
                    }
                    let child_name = name(&child["name"])?;
                    if !pairs.insert((namespace.clone(), child_name.clone())) {
                        return Err(format!("tools[{i}].tools[{j}] 工具名称重复"));
                    }
                    let mut child = child.clone();
                    expanded_bytes += tool["description"].as_str().map_or(0, str::len)
                        + child.to_string().len()
                        + namespace.len()
                        + child_name.len();
                    if expanded_bytes > 4 * 1024 * 1024 {
                        return Err("命名空间工具展开后超过 4 MiB 上限".into());
                    }
                    // Preserve namespace guidance in the model-facing definition.
                    child["description"] = json!(format!(
                        "{namespace}.{child_name}\n{}\n{}",
                        tool["description"].as_str().unwrap_or(""),
                        child["description"].as_str().unwrap_or("")
                    ));
                    definitions.push((Some(namespace.clone()), child));
                }
            } else {
                if tool["type"] == "function" && !reserved.insert(name(&tool["name"])?) {
                    return Err(format!("tools[{i}] 顶层工具名称重复"));
                }
                definitions.push((None, tool.clone()));
            }
        }
    }
    // History may contain tools removed from the current tool list. Include
    // those identities so every historical call remains reversible too.
    if let Some(items) = input["input"].as_array() {
        for item in items {
            if item["type"] == "function_call" {
                let tool_name = name(&item["name"])?;
                if let Some(namespace) = item.get("namespace").filter(|v| !v.is_null()) {
                    pairs.insert((name(namespace)?, tool_name));
                } else {
                    reserved.insert(tool_name);
                }
            }
        }
    }
    let mut map = ToolMap::default();
    map.1 = custom;
    let mut index = 0;
    for pair in pairs {
        let alias = loop {
            let candidate = format!("wbns_{index}");
            index += 1;
            if reserved.insert(candidate.clone()) {
                break candidate;
            }
        };
        map.0.insert(alias, pair);
    }
    if input.get("tools").is_some() {
        let mut tools = vec![];
        for (namespace, mut tool) in definitions {
            if let Some(namespace) = namespace {
                tool["name"] = json!(map.alias(&namespace, tool["name"].as_str().unwrap())?);
            }
            tools.push(tool);
        }
        flat["tools"] = json!(tools);
    }
    if let Some(items) = flat["input"].as_array_mut() {
        for item in items {
            if item["type"] == "function_call" {
                if let Some(namespace) = item.get("namespace").filter(|v| !v.is_null()) {
                    item["name"] = json!(
                        map.alias(namespace.as_str().unwrap(), item["name"].as_str().unwrap())?
                    );
                    item.as_object_mut().unwrap().remove("namespace");
                }
            }
        }
    }
    if flat["tool_choice"]["type"] == "function" {
        if let Some(namespace) = flat["tool_choice"]
            .get("namespace")
            .filter(|v| !v.is_null())
        {
            let alias = map.alias(&name(namespace)?, &name(&flat["tool_choice"]["name"])?)?;
            flat["tool_choice"]["name"] = json!(alias);
            flat["tool_choice"]
                .as_object_mut()
                .unwrap()
                .remove("namespace");
        }
    }
    Ok((flat, map))
}

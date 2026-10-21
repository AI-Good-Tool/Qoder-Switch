//! Public model identities and explicit regional upstream mappings.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ModelRoute {
    pub alias: String,
    pub cn: Option<String>,
    pub intl: Option<String>,
}
pub fn catalog(
    models: &HashMap<String, Vec<Value>>,
    overrides: &BTreeMap<String, ModelRoute>,
    mode: &str,
) -> Vec<Value> {
    let mut groups: BTreeMap<String, Value> = BTreeMap::new();
    for edition in ["cn", "intl"] {
        for m in
            crate::qproxy::public_catalog(models.get(edition).map(Vec::as_slice).unwrap_or(&[]))
        {
            let id = m["id"].as_str().unwrap_or("");
            let named = m["name"].as_str().is_some_and(|n| !n.trim().is_empty());
            let key = if named {
                m["publicId"].as_str().unwrap().to_owned()
            } else {
                format!("{edition}:{id}")
            };
            let row = groups.entry(key.clone()).or_insert_with(|| {
                let mut row = m.clone();
                row["routeId"] = json!(key);
                row["targets"] = json!([]);
                row
            });
            let mut target = m.clone();
            target["edition"] = json!(edition);
            row["targets"].as_array_mut().unwrap().push(target);
        }
    }
    // Keep saved mappings visible even when an upstream catalog temporarily omits a model.
    for (id, setting) in overrides {
        let row = groups.entry(id.clone()).or_insert(
            json!({"routeId":id,"id":id,"name":id,"publicId":setting.alias,"targets":[]}),
        );
        row["publicId"] = json!(setting.alias);
        row["targets"] = json!([("cn", &setting.cn), ("intl", &setting.intl)]
            .into_iter()
            .filter_map(|(ed, id)| {
                let id = id.as_ref()?;
                let mut model = models.get(ed)?.iter().find(|m| m["id"] == *id)?.clone();
                model["edition"] = json!(ed);
                Some(model)
            })
            .collect::<Vec<_>>());
        row["configured"] = json!(true);
    }
    let mut rows: Vec<_> = groups.into_values().collect();
    let original = rows.clone();
    for row in &mut rows {
        let alias = row["publicId"].as_str().unwrap_or("");
        if row["configured"] != true
            && original.iter().any(|other| {
                other["routeId"] != row["routeId"]
                    && (other["publicId"] == alias
                        || other["targets"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|t| t["id"] == alias))
            })
        {
            row["publicId"] = json!(format!("{} [{}]", alias, row["routeId"].as_str().unwrap()));
        }
        let all_targets = row["targets"].clone();
        row["allTargets"] = all_targets.clone();
        row["targets"] = json!(all_targets
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| mode == "both" || t["edition"] == mode)
            .cloned()
            .collect::<Vec<_>>());
        let targets = row["targets"].as_array().unwrap().clone();
        row["editions"] = json!(targets
            .iter()
            .map(|t| t["edition"].clone())
            .collect::<Vec<_>>());
        row["available"] = json!(!targets.is_empty());
        // Publish only capabilities shared by every possible upstream.
        for capability in ["supportsTools", "supportsImages"] {
            row[capability] =
                json!(!targets.is_empty() && targets.iter().all(|t| t[capability] == true));
        }
        let windows: Vec<_> = targets
            .iter()
            .filter_map(|t| t["contextWindow"].as_u64())
            .collect();
        row["contextWindow"] = if !targets.is_empty() && windows.len() == targets.len() {
            json!(windows.into_iter().min())
        } else {
            Value::Null
        };
        let efforts: Vec<Value> = row["targets"]
            .as_array()
            .unwrap()
            .first()
            .and_then(|t| t["reasoning"]["supportedEfforts"].as_array())
            .map(|a| {
                a.iter()
                    .filter(|v| {
                        row["targets"].as_array().unwrap().iter().all(|t| {
                            t["reasoning"]["supportedEfforts"]
                                .as_array()
                                .is_some_and(|a| a.contains(v))
                        })
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        row["reasoning"] = json!({"supportedEfforts":efforts});
    }
    rows
}
pub fn resolve(rows: &[Value], requested: &str) -> Option<Value> {
    if let Some(row) = rows.iter().find(|r| r["publicId"] == requested) {
        return Some(row.clone());
    }
    let candidates: Vec<_> = rows
        .iter()
        .filter(|r| {
            r["targets"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["id"] == requested)
        })
        .collect();
    if candidates.len() == 1 {
        Some(candidates[0].clone())
    } else {
        None
    }
}
pub fn validate_aliases(rows: &[Value]) -> Result<(), String> {
    let mut used = HashSet::new();
    for row in rows {
        let alias = row["publicId"].as_str().unwrap_or("");
        if alias.trim() != alias
            || alias.is_empty()
            || alias.chars().count() > 128
            || alias.chars().any(char::is_control)
        {
            return Err("模型别名需为 1–128 个字符，不能包含首尾空格或控制字符".into());
        }
        if !used.insert(alias) {
            return Err(format!("模型别名重复：{alias}"));
        }
        if row["configured"] == true
            && rows.iter().any(|r| {
                r["routeId"] != row["routeId"]
                    && (r["name"] == alias
                        || r["routeId"] == alias
                        || r["allTargets"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|t| t["id"] == alias))
            })
        {
            return Err("别名不能与其他模型的内部 ID 冲突".into());
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn models() -> HashMap<String, Vec<Value>> {
        HashMap::from([
            (
                "cn".into(),
                vec![
                    json!({"id":"c","name":"Shared"}),
                    json!({"id":"same","name":"CN only"}),
                ],
            ),
            (
                "intl".into(),
                vec![
                    json!({"id":"i","name":"Shared"}),
                    json!({"id":"same","name":"Intl only"}),
                ],
            ),
        ])
    }
    #[test]
    fn merge_names_not_internal_ids() {
        let rows = catalog(&models(), &BTreeMap::new(), "both");
        assert_eq!(rows.len(), 3);
        assert_eq!(
            resolve(&rows, "Shared").unwrap()["targets"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(resolve(&rows, "same").is_none());
        assert_eq!(
            resolve(&rows, "CN only").unwrap()["targets"][0]["edition"],
            "cn"
        );
    }
    #[test]
    fn aliases_and_explicit_mappings_roundtrip() {
        let settings: BTreeMap<String, ModelRoute> = BTreeMap::from([(
            "Shared".into(),
            ModelRoute {
                alias: "my-model".into(),
                cn: Some("c".into()),
                intl: None,
            },
        )]);
        let settings = serde_json::from_str(&serde_json::to_string(&settings).unwrap()).unwrap();
        let rows = catalog(&models(), &settings, "both");
        let row = resolve(&rows, "my-model").unwrap();
        assert_eq!(row["targets"].as_array().unwrap().len(), 1);
        assert!(resolve(&rows, "Shared").is_none());
        assert!(validate_aliases(&rows).is_ok());
        let limited = catalog(&models(), &settings, "intl");
        assert_eq!(resolve(&limited, "my-model").unwrap()["available"], false);
    }
    #[test]
    fn common_capabilities_use_intersection() {
        let mut models = models();
        models.get_mut("cn").unwrap()[0] = json!({"id":"c","name":"Shared","contextWindow":200000,"supportsImages":true,"reasoning":{"supportedEfforts":["none","high"]}});
        models.get_mut("intl").unwrap()[0] = json!({"id":"i","name":"Shared","contextWindow":100000,"supportsImages":false,"reasoning":{"supportedEfforts":["none"]}});
        let row = resolve(&catalog(&models, &BTreeMap::new(), "both"), "Shared").unwrap();
        assert_eq!(row["contextWindow"], 100000);
        assert_eq!(row["supportsImages"], false);
        assert_eq!(row["reasoning"]["supportedEfforts"], json!(["none"]));
    }
    #[test]
    fn duplicate_alias_is_rejected() {
        let settings: BTreeMap<String, ModelRoute> = BTreeMap::from([(
            "Shared".into(),
            ModelRoute {
                alias: "CN only".into(),
                cn: Some("c".into()),
                intl: None,
            },
        )]);
        assert!(validate_aliases(&catalog(&models(), &settings, "both")).is_err());
    }
}

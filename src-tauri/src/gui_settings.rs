//! Closed public preference deltas retained by the GUI broker, never caller-selected vault accounts.
use serde_json::{Map, Value};

#[derive(Clone)]
pub(crate) struct SettingsDelta {
    pub area: String,
    set: Map<String, Value>,
}

fn identity(value: Option<&Value>) -> Result<&str, String> {
    value
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 128 && !id.chars().any(char::is_control))
        .ok_or_else(|| "invalid_args".into())
}
fn entity(value: &Value, enabled: bool) -> Result<(), String> {
    let object = value.as_object().ok_or("invalid_args")?;
    if object.len() != if enabled { 2 } else { 1 }
        || object
            .keys()
            .any(|key| key != "id" && !(enabled && key == "enabled"))
    {
        return Err("invalid_args".into());
    }
    identity(object.get("id"))?;
    if enabled && !object.get("enabled").is_some_and(Value::is_boolean) {
        return Err("invalid_args".into());
    }
    Ok(())
}

impl SettingsDelta {
    pub fn from_args(args: &Value) -> Result<Self, String> {
        let area = args
            .get("area")
            .and_then(Value::as_str)
            .ok_or("invalid_args")?;
        let set = args
            .get("set")
            .and_then(Value::as_object)
            .ok_or("invalid_args")?;
        if set.is_empty() {
            return Err("invalid_args".into());
        }
        match area {
            "general" if crate::gui_controller::general_settings_map(set) => {}
            "ai" => {
                for (key, value) in set {
                    match key.as_str() {
                        "provider_enabled" | "model_enabled" => entity(value, true)?,
                        "model_default" => entity(value, false)?,
                        "advanced"
                            if value.as_object().is_some_and(|values| {
                                !values.is_empty()
                                    && crate::gui_controller::advanced_settings_map(values)
                            }) => {}
                        _ => return Err("invalid_args".into()),
                    }
                }
            }
            _ => return Err("invalid_args".into()),
        }
        Ok(Self {
            area: area.into(),
            set: set.clone(),
        })
    }

    pub fn account(&self) -> &'static str {
        if self.area == "general" {
            "config_app_settings"
        } else {
            "config_ai_settings"
        }
    }

    pub fn apply(&self, existing: Option<Value>) -> Result<Value, String> {
        let mut current = existing.unwrap_or_else(|| Value::Object(Map::new()));
        let blob = current.as_object_mut().ok_or("action_failed")?;
        if self.area == "general" {
            blob.extend(self.set.clone());
            return Ok(current);
        }
        let providers = blob
            .get("providers")
            .and_then(Value::as_array)
            .ok_or("action_failed")?;
        let models = blob
            .get("models")
            .and_then(Value::as_array)
            .ok_or("action_failed")?;
        if providers.len() > 32
            || models.len() > 64
            || providers.iter().any(|p| {
                !p.is_object()
                    || p.get("apiKey")
                        .is_some_and(|key| !key.is_null() && key.as_str() != Some(""))
            })
            || models.iter().any(|model| !model.is_object())
        {
            // Do not clear a legacy embedded secret that has not been migrated by the human UI.
            return Err("action_failed".into());
        }
        for (key, collection) in [
            ("provider_enabled", "providers"),
            ("model_enabled", "models"),
            ("model_default", "models"),
        ] {
            if let Some(change) = self.set.get(key) {
                let id = change["id"].as_str().ok_or("invalid_args")?;
                let records = blob
                    .get_mut(collection)
                    .and_then(Value::as_array_mut)
                    .ok_or("action_failed")?;
                let matches: Vec<usize> = records
                    .iter()
                    .enumerate()
                    .filter_map(|(index, record)| {
                        (record.get("id").and_then(Value::as_str) == Some(id)).then_some(index)
                    })
                    .collect();
                if matches.len() != 1 {
                    return Err("invalid_args".into());
                }
                let index = matches[0];
                if key == "model_default" {
                    let provider = records[index]
                        .get("providerId")
                        .and_then(Value::as_str)
                        .ok_or("action_failed")?
                        .to_owned();
                    for (i, record) in records.iter_mut().enumerate() {
                        if record.get("providerId").and_then(Value::as_str)
                            == Some(provider.as_str())
                        {
                            record["isDefault"] = Value::Bool(i == index);
                        }
                    }
                } else {
                    records[index]["isEnabled"] = change["enabled"].clone();
                    if key == "provider_enabled" {
                        records[index]["updatedAt"] =
                            Value::String(chrono::Utc::now().to_rfc3339());
                    }
                }
            }
        }
        if let Some(change) = self.set.get("advanced") {
            if !blob.contains_key("advancedSettings") {
                blob.insert("advancedSettings".into(), Value::Object(Map::new()));
            }
            let advanced = blob
                .get_mut("advancedSettings")
                .and_then(Value::as_object_mut)
                .ok_or("action_failed")?;
            for (wire, value) in change.as_object().ok_or("invalid_args")? {
                let key = match wire.as_str() {
                    "temperature" => "temperature",
                    "max_tokens" => "maxTokens",
                    "top_p" => "topP",
                    "top_k" => "topK",
                    "conversation_style" => "conversationStyle",
                    "response_style" => "responseStyle",
                    _ => return Err("invalid_args".into()),
                };
                advanced.insert(key.into(), value.clone());
            }
        }
        for provider in blob
            .get_mut("providers")
            .and_then(Value::as_array_mut)
            .ok_or("action_failed")?
        {
            provider
                .as_object_mut()
                .ok_or("action_failed")?
                .remove("apiKey");
        }
        Ok(current)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn fixture() -> Value {
        json!({"providers":[{"id":"p","isEnabled":true}],"models":[
            {"id":"m1","providerId":"p","isDefault":true},{"id":"m2","providerId":"p","isDefault":false}],
            "advancedSettings":{"temperature":0.7,"customSystemPrompt":"preserved"}})
    }
    #[test]
    fn closed_deltas_reject_secret_unknown_prototype_and_wrong_shapes() {
        for args in [
            json!({"area":"general","set":{"password":"secret"}}),
            json!({"area":"general","set":{"constructor":true}}),
            json!({"area":"ai","set":{"advanced":{"apiKey":"secret"}}}),
            json!({"area":"ai","set":{"model_default":{"id":"m","password":"secret"}}}),
            json!({"area":"ai","set":{"provider_enabled":{"id":"p","enabled":"true"}}}),
        ] {
            assert!(SettingsDelta::from_args(&args).is_err());
        }
    }
    #[test]
    fn bounded_delta_merges_without_changing_unrequested_policy_or_secret_records() {
        let general =
            SettingsDelta::from_args(&json!({"area":"general","set":{"fontSize":16}})).unwrap();
        assert_eq!(
            general
                .apply(Some(json!({"confirmBeforeDelete":true,"fontSize":14})))
                .unwrap(),
            json!({"confirmBeforeDelete":true,"fontSize":16})
        );
        let ai = SettingsDelta::from_args(&json!({"area":"ai","set":{"model_default":{"id":"m2"},
            "provider_enabled":{"id":"p","enabled":false},"advanced":{"temperature":0.2}}}))
        .unwrap();
        let applied = ai.apply(Some(fixture())).unwrap();
        assert_eq!(
            applied["advancedSettings"]["customSystemPrompt"],
            "preserved"
        );
        assert_eq!(applied["advancedSettings"]["temperature"], 0.2);
        assert_eq!(applied["models"][0]["isDefault"], false);
        assert_eq!(applied["models"][1]["isDefault"], true);
        assert_eq!(applied["providers"][0]["isEnabled"], false);
    }
    #[test]
    fn rejects_missing_duplicate_identity_and_unmigrated_embedded_keys() {
        let delta = SettingsDelta::from_args(
            &json!({"area":"ai","set":{"provider_enabled":{"id":"p","enabled":false}}}),
        )
        .unwrap();
        assert!(delta.apply(None).is_err());
        let mut missing = fixture();
        missing["providers"][0]["id"] = json!("other");
        assert!(delta.apply(Some(missing)).is_err());
        let mut duplicate = fixture();
        duplicate["providers"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":"p"}));
        assert!(delta.apply(Some(duplicate)).is_err());
        let mut legacy = fixture();
        legacy["providers"][0]["apiKey"] = json!("SECRET");
        assert!(delta.apply(Some(legacy)).is_err());
    }
}

use super::*;
use crate::config::TransformConfig;

#[test]
fn responses_text_format_to_chat_supplies_codex_defaults() {
    let format = responses_text_format_to_chat(&json!({
        "type": "json_schema",
        "schema": {"type": "object"}
    }))
    .expect("json schema format converts");

    assert_eq!(format["type"], "json_schema");
    assert_eq!(format["json_schema"]["name"], "codex_output_schema");
    assert_eq!(format["json_schema"]["strict"], false);
    assert_eq!(format["json_schema"]["schema"]["type"], "object");
}

#[test]
fn reasoning_effort_to_thinking_type_maps_supported_values() {
    assert_eq!(
        reasoning_effort_to_thinking_type(&json!("off")),
        Some(json!("disabled"))
    );
    assert_eq!(
        reasoning_effort_to_thinking_type(&json!("HIGH")),
        Some(json!("enabled"))
    );
    assert_eq!(reasoning_effort_to_thinking_type(&json!("custom")), None);
}

#[test]
fn apply_reasoning_effort_none_value_remaps_disable_synonyms() {
    let mut transform = TransformConfig::default();
    transform.reasoning_effort_none_value = Some("low".to_string());
    let mut body = json!({"reasoning_effort": "off"});
    apply_reasoning_effort_none_value(&mut body, &transform);
    assert_eq!(body["reasoning_effort"], "low");
}

#[test]
fn strip_disabled_reasoning_effort_removes_disable_values() {
    let mut body = json!({"reasoning_effort": "none", "thinking": {"type": "disabled"}});
    strip_disabled_reasoning_effort(&mut body);
    assert!(body.get("reasoning_effort").is_none());
    assert_eq!(body["thinking"]["type"], "disabled");
}

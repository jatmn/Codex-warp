use super::*;
use crate::config::RequestMorphKind;
use crate::config::TransformConfig;

#[test]
fn reasoning_effort_none_value_remaps_none_in_top_level_and_reasoning_object() {
    let transform = TransformConfig {
        reasoning_effort_none_value: Some("no_think".to_string()),
        ..TransformConfig::default()
    };

    let mut body = json!({"reasoning_effort": "none", "model": "hy3"});
    apply_reasoning_effort_none_value(&mut body, &transform);
    assert_eq!(body["reasoning_effort"], "no_think");

    let mut body = json!({"reasoning": {"effort": "none"}});
    apply_reasoning_effort_none_value(&mut body, &transform);
    assert_eq!(body["reasoning"]["effort"], "no_think");
}

#[test]
fn reasoning_effort_none_value_keeps_other_levels() {
    let transform = TransformConfig {
        reasoning_effort_none_value: Some("no_think".to_string()),
        ..TransformConfig::default()
    };

    let mut body = json!({"reasoning_effort": "high"});
    apply_reasoning_effort_none_value(&mut body, &transform);
    assert_eq!(body["reasoning_effort"], "high");
}

#[test]
fn reasoning_effort_none_value_is_noop_when_unset() {
    let transform = TransformConfig::default();

    let mut body = json!({"reasoning_effort": "none"});
    apply_reasoning_effort_none_value(&mut body, &transform);
    assert_eq!(body["reasoning_effort"], "none");
}

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
    let transform = TransformConfig {
        reasoning_effort_none_value: Some("low".to_string()),
        ..TransformConfig::default()
    };
    let mut body = json!({"reasoning_effort": "off"});
    apply_reasoning_effort_none_value(&mut body, &transform);
    assert_eq!(body["reasoning_effort"], "low");
}

#[test]
fn strip_disabled_reasoning_effort_removes_disable_values_for_thinking_type_families() {
    let mut transform = TransformConfig::default();
    transform
        .chat_request_morphs
        .push(crate::config::RequestMorph {
            from: "reasoning.effort".to_string(),
            to: Some("thinking.type".to_string()),
            value: None,
            kind: RequestMorphKind::ThinkingType,
        });
    let mut body = json!({"reasoning_effort": "none", "thinking": {"type": "disabled"}});
    strip_disabled_reasoning_effort(&mut body, &transform);
    assert!(body.get("reasoning_effort").is_none());
    assert_eq!(body["thinking"]["type"], "disabled");
}

#[test]
fn strip_disabled_reasoning_effort_removes_none_value_for_thinking_type_families() {
    let mut transform = TransformConfig {
        reasoning_effort_none_value: Some("no_think".to_string()),
        ..TransformConfig::default()
    };
    transform
        .chat_request_morphs
        .push(crate::config::RequestMorph {
            from: "reasoning.effort".to_string(),
            to: Some("thinking.type".to_string()),
            value: None,
            kind: RequestMorphKind::ThinkingType,
        });
    let mut body = json!({"reasoning_effort": "no_think", "thinking": {"type": "disabled"}});
    strip_disabled_reasoning_effort(&mut body, &transform);
    assert!(body.get("reasoning_effort").is_none());
    assert_eq!(body["thinking"]["type"], "disabled");
}

#[test]
fn strip_disabled_reasoning_effort_preserves_disable_values_for_rename_only_families() {
    let transform = TransformConfig::default();
    let mut body = json!({"reasoning_effort": "none"});
    strip_disabled_reasoning_effort(&mut body, &transform);
    assert_eq!(body["reasoning_effort"], "none");
}

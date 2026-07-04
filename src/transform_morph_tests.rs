use super::*;

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

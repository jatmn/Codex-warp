use super::bundled_provider_templates;
use super::find_provider_template;

#[test]
fn bundled_templates_include_example_profiles() {
    let templates = bundled_provider_templates();
    let keys: Vec<&str> = templates
        .iter()
        .map(|template| template.key.as_str())
        .collect();
    assert!(keys.contains(&"openrouter"));
    assert!(keys.contains(&"moonshot_kimicode"));
    assert!(keys.contains(&"opencode_go"));
    assert!(keys.contains(&"cline_pass"));
    assert!(keys.contains(&"hicap"));
    assert!(keys.contains(&"xiaomi_token_plan"));
    assert!(keys.contains(&"custom"));
    assert_eq!(
        keys[0], "custom",
        "custom template is the default picker entry"
    );
    let named_labels: Vec<String> = templates
        .iter()
        .skip(1)
        .map(|template| template.label.to_ascii_lowercase())
        .collect();
    let mut sorted_labels = named_labels.clone();
    sorted_labels.sort();
    assert_eq!(named_labels, sorted_labels);
    assert!(templates.iter().any(|template| template.id.is_empty()));

    let kimi = find_provider_template("moonshot_kimicode").expect("kimi template");
    assert_eq!(kimi.api_key_env.as_deref(), Some("KIMICODE_API_KEY"));
    assert!(!kimi.model_catalog.is_empty());
    assert!(kimi.base_url.contains("kimi.com"));
    assert_eq!(
        kimi.provider.model_catalog.len(),
        kimi.model_catalog.len(),
        "server snapshot must keep the full example catalog"
    );

    let go = find_provider_template("opencode_go").expect("opencode go template");
    assert!(go.model_catalog_only);
    assert!(go.provider.model_catalog_only);
    assert!(
        go.model_catalog
            .iter()
            .any(|entry| entry.upstream_id.as_deref() == Some("glm-5.2"))
    );

    let hicap = find_provider_template("hicap").expect("hicap template");
    assert_eq!(hicap.api_key_env.as_deref(), Some("HICAP_API_KEY"));
    assert_eq!(hicap.auth_header, "api-key");
    assert!(hicap.auth_scheme.is_empty());
    assert_eq!(
        hicap.headers.get("x-hicap-tag").map(String::as_str),
        Some("codex-warp-jatmn")
    );
    assert!(!hicap.model_catalog_only);
    assert!(
        !hicap.model_catalog.is_empty(),
        "hybrid template must keep a local catalog fallback"
    );
    assert_eq!(
        hicap
            .provider
            .headers
            .get("x-hicap-tag")
            .map(String::as_str),
        Some("codex-warp-jatmn")
    );
    assert!(
        hicap
            .model_catalog
            .iter()
            .any(|entry| entry.upstream_id.as_deref() == Some("hy3:free"))
    );
}

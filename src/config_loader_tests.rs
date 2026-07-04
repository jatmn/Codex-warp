use super::*;

use serde_json::json;

#[test]
fn config_layer_paths_keeps_default_first_without_duplicate() {
    let paths = config_layer_paths(&[
        PathBuf::from(DEFAULT_CONFIG_PATH),
        PathBuf::from("configs/xiaomi-token-plan.toml"),
    ]);

    assert_eq!(
        paths,
        vec![
            PathBuf::from(DEFAULT_CONFIG_PATH),
            PathBuf::from("configs/xiaomi-token-plan.toml"),
        ]
    );
}

#[test]
fn default_config_path_falls_back_to_explicit_config_ancestor() {
    let suffix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is valid")
        .as_nanos();
    let root = std::env::temp_dir().join(format!("codex-warp-default-path-test-{suffix}"));
    let configs = root.join("configs");
    std::fs::create_dir_all(&configs).expect("config dir created");
    std::fs::write(root.join(DEFAULT_CONFIG_PATH), "listen = \"127.0.0.1:0\"\n")
        .expect("default config written");
    let explicit = configs.join("provider.toml");
    std::fs::write(
        &explicit,
        "[provider]\nbase_url = \"https://provider.example/v1\"\n",
    )
    .expect("provider config written");
    let missing_cwd_default = root.join("run-from-elsewhere").join(DEFAULT_CONFIG_PATH);

    let resolved = default_config_path_for(&[explicit], &missing_cwd_default);

    assert_eq!(resolved, root.join(DEFAULT_CONFIG_PATH));
}

#[test]
fn config_includes_resolve_relative_to_declaring_file() {
    let value: TomlValue = toml::from_str(
        r#"
        [config]
        include = ["profiles/provider.toml", "/opt/codex-warp/global.toml"]
        model_family_include = ["families/models.toml"]
        "#,
    )
    .expect("include config parses");

    let includes = config_includes(Path::new("/tmp/codex-warp/base.toml"), &value);

    assert_eq!(
        includes,
        vec![
            PathBuf::from("/tmp/codex-warp/profiles/provider.toml"),
            PathBuf::from("/opt/codex-warp/global.toml"),
            PathBuf::from("/tmp/codex-warp/families/models.toml"),
        ]
    );
}

#[test]
fn merge_toml_recurses_tables_and_replaces_scalar_values() {
    let mut base: TomlValue = toml::from_str(
        r#"
        [provider]
        base_url = "https://old.example/v1"
        model_catalog = ["old"]

        [provider.headers]
        x-old = "1"
        "#,
    )
    .expect("base toml parses");
    let overlay: TomlValue = toml::from_str(
        r#"
        [provider]
        base_url = "https://new.example/v1"
        model_catalog = ["new"]

        [provider.headers]
        x-new = "2"
        "#,
    )
    .expect("overlay toml parses");

    merge_toml(&mut base, overlay);

    assert_eq!(
        base["provider"]["base_url"].as_str(),
        Some("https://new.example/v1")
    );
    assert_eq!(base["provider"]["headers"]["x-old"].as_str(), Some("1"));
    assert_eq!(base["provider"]["headers"]["x-new"].as_str(), Some("2"));
    assert_eq!(
        base["provider"]["model_catalog"],
        TomlValue::try_from(json!(["new"])).expect("json array converts to toml")
    );
}

#[test]
fn merge_toml_appends_tool_policy_rules_only() {
    let mut base: TomlValue = toml::from_str(
        r#"
        [provider]
        model_catalog = ["old"]

        [[tool_policy.rules]]
        id = "first"
        "#,
    )
    .expect("base toml parses");
    let overlay: TomlValue = toml::from_str(
        r#"
        [provider]
        model_catalog = ["new"]

        [[tool_policy.rules]]
        id = "second"
        "#,
    )
    .expect("overlay toml parses");

    merge_toml(&mut base, overlay);

    assert_eq!(
        base["provider"]["model_catalog"],
        TomlValue::try_from(json!(["new"])).expect("json array converts to toml")
    );
    assert_eq!(
        base["tool_policy"]["rules"][0]["id"].as_str(),
        Some("first")
    );
    assert_eq!(
        base["tool_policy"]["rules"][1]["id"].as_str(),
        Some("second")
    );
}

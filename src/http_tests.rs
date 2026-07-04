use super::*;

use axum::http::HeaderMap;
use reqwest::Client;

use crate::config::ProviderConfig;
use crate::version::user_agent;

#[test]
fn upstream_requests_report_codex_warp_user_agent() {
    let mut provider = ProviderConfig::default();
    provider
        .headers
        .insert("user-agent".to_string(), "masked-client/9.9.9".to_string());
    let request = Client::new().post("https://provider.example/v1/models");

    let request =
        apply_headers_with_accept(request, &provider, &HeaderMap::new(), "application/json")
            .build()
            .expect("request builds");
    let expected = user_agent();

    assert_eq!(
        request
            .headers()
            .get(axum::http::header::USER_AGENT)
            .and_then(|value| value.to_str().ok()),
        Some(expected.as_str())
    );
}

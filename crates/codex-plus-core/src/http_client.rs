use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

static CLIENT_CACHE: OnceLock<Mutex<HashMap<String, reqwest::Client>>> = OnceLock::new();

pub fn proxied_client(user_agent: &str) -> anyhow::Result<reqwest::Client> {
    let ua = if user_agent.trim().is_empty() {
        format!("CodexPlusPlus/{}", env!("CARGO_PKG_VERSION"))
    } else {
        user_agent.trim().to_string()
    };

    let cache = CLIENT_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut guard = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(client) = guard.get(&ua) {
        return Ok(client.clone());
    }

    let client = reqwest::Client::builder().user_agent(&ua).build()?;
    guard.insert(ua, client.clone());
    Ok(client)
}

/// VLM 专用 HTTP client（带超时）。
/// 不复用通用 proxied_client，避免 VLM 服务无响应时永久阻塞整个代理。
pub fn vlm_http_client() -> anyhow::Result<reqwest::Client> {
    vlm_http_client_with_timeout(
        std::time::Duration::from_secs(5),
        std::time::Duration::from_secs(30),
    )
}

pub(crate) fn vlm_http_client_with_timeout(
    connect: std::time::Duration,
    total: std::time::Duration,
) -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(format!("CodexPlusPlus-VLM/{}", env!("CARGO_PKG_VERSION")))
        .connect_timeout(connect)
        .timeout(total)
        .build()?)
}

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
    let mut guard = cache.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(client) = guard.get(&ua) {
        return Ok(client.clone());
    }

    let client = reqwest::Client::builder().user_agent(&ua).build()?;
    guard.insert(ua, client.clone());
    Ok(client)
}

use crate::config::{save, Config};
use reqwest::{Client, Method, StatusCode};
use serde_json::{json, Value};
use std::path::Path;
use std::time::Duration;

pub mod settings;

pub const PIPE: &str = include_str!("../openwebui/pipe.py");
const ID: &str = "rpi_ui_adapter";

async fn call(
    client: &Client,
    config: &Config,
    token: &str,
    method: Method,
    path: &str,
    body: Option<Value>,
) -> Result<Option<Value>, String> {
    let mut request = client
        .request(method, format!("{}{path}", config.url))
        .bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request
        .send()
        .await
        .map_err(|_| "Cannot reach Open WebUI".to_string())?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    if !response.status().is_success() {
        // Do not print response bodies or the bearer token into host logs.
        return Err(format!(
            "Open WebUI API {path} returned {} (an admin token is required)",
            response.status()
        ));
    }
    response
        .json()
        .await
        .map(Some)
        .map_err(|_| "Invalid JSON from Open WebUI".into())
}

pub async fn install(config: &Config, token: &str, state_path: &Path) -> Result<(), String> {
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let base = format!("/api/v1/functions/id/{ID}");
    // Open WebUI 0.12.x returns 401 (not 404) for an absent function ID.
    // Discover through the authenticated list so missing != invalid token.
    let list = call(
        &client,
        config,
        token,
        Method::GET,
        "/api/v1/functions/",
        None,
    )
    .await?
    .ok_or("Open WebUI Functions API not available")?;
    let entries = list.as_array().ok_or("Invalid Open WebUI function list")?;
    let existing = if entries.iter().any(|item| item["id"] == ID) {
        call(&client, config, token, Method::GET, &base, None).await?
    } else {
        None
    };
    if let Some(existing) = &existing {
        if !existing["content"]
            .as_str()
            .unwrap_or("")
            .contains("rpi-ui-adapter managed function")
        {
            return Err(
                "Function rpi_ui_adapter already exists and is not managed by this extension"
                    .into(),
            );
        }
    }
    // Save generated state before external mutation so retries reuse the same
    // adapter token. The user's .rpi/webui/<UI>.json is never overwritten.
    let mut pending = config.clone();
    pending.installed = false;
    save(state_path, &pending)?;
    let meta = existing
        .as_ref()
        .and_then(|v| v.get("meta"))
        .cloned()
        .unwrap_or_else(|| json!({"description":"rpi ACP UI adapter"}));
    let form = json!({"id":ID,"name":"rpi","content":PIPE,"meta":meta});
    let target = if existing.is_some() {
        format!("{base}/update")
    } else {
        "/api/v1/functions/create".into()
    };
    call(&client, config, token, Method::POST, &target, Some(form))
        .await?
        .ok_or("Open WebUI Functions API not available")?;
    let mut valves = call(
        &client,
        config,
        token,
        Method::GET,
        &format!("{base}/valves"),
        None,
    )
    .await?
    .filter(Value::is_object)
    .unwrap_or_else(|| json!({}));
    valves["ADAPTER_URL"] = json!(config.adapter_url);
    valves["ADAPTER_TOKEN"] = json!(config.adapter_token);
    valves["CWD"] = json!(config.cwd);
    valves["PERMISSIONS"] = json!(config.permissions);
    call(
        &client,
        config,
        token,
        Method::POST,
        &format!("{base}/valves/update"),
        Some(valves),
    )
    .await?
    .ok_or("Cannot configure UI adapter valves")?;
    let current = call(&client, config, token, Method::GET, &base, None)
        .await?
        .ok_or("Installed UI adapter disappeared")?;
    if current["is_active"] != true {
        call(
            &client,
            config,
            token,
            Method::POST,
            &format!("{base}/toggle"),
            None,
        )
        .await?
        .ok_or("Cannot enable UI adapter")?;
    }
    Ok(())
}

//! Each UI owns its own schema; this one belongs to Open WebUI.
use crate::config::{endpoint, write_json};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::path::Path;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Settings {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    token: Option<String>,
    #[serde(default = "token_env")]
    token_env: String,
    #[serde(default)]
    pub adapter: Adapter,
    #[serde(default = "permission_policy")]
    pub permissions: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Adapter {
    #[serde(default = "bind")]
    pub bind: SocketAddr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}
fn token_env() -> String {
    "OPENWEBUI_TOKEN".into()
}
fn permission_policy() -> String {
    "ask".into()
}
fn bind() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 5234))
}
impl Default for Adapter {
    fn default() -> Self {
        Self {
            bind: bind(),
            url: None,
        }
    }
}

pub fn load(path: &Path) -> Result<Settings, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let template = Settings {
                url: "http://127.0.0.1:8080".into(),
                token: None,
                token_env: token_env(),
                adapter: Adapter::default(),
                permissions: permission_policy(),
            };
            write_json(path, &template, false)?;
            return Err(format!("Created {}. Set the Open WebUI URL and token/tokenEnv, then run rpi --webui=openwebui again",path.display()));
        }
        Err(e) => return Err(format!("Cannot read {}: {e}", path.display())),
    };
    // Detailed serde errors can echo a mistyped secret. Report a location and
    // the supported schema without returning configuration contents.
    let mut settings:Settings=serde_json::from_str(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("Invalid {} at line {}, column {}. Expected Open WebUI fields: url, token, tokenEnv, adapter {{bind,url}}, permissions",path.display(),e.line(),e.column()))?;
    settings.url = endpoint(&settings.url)?;
    if settings.adapter.bind.port() == 0 {
        return Err("adapter.bind requires a nonzero port".into());
    }
    let adapter_url = endpoint(
        settings
            .adapter
            .url
            .as_deref()
            .unwrap_or(&format!("http://{}", settings.adapter.bind)),
    )?;
    let host = url::Url::parse(&adapter_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned));
    if host.is_some_and(|host| host == "0.0.0.0" || host == "[::]") {
        return Err("Set adapter.url in .rpi/webui/openwebui.json to an address reachable by the Open WebUI backend".into());
    }
    settings.adapter.url = Some(adapter_url);
    if !matches!(settings.permissions.as_str(), "ask" | "bypass" | "deny") {
        return Err("permissions in .rpi/webui/openwebui.json must be ask, bypass or deny".into());
    }
    Ok(settings)
}

impl Settings {
    pub fn admin_token(
        &self,
        env: &impl Fn(&str) -> Option<String>,
    ) -> Result<Option<String>, String> {
        if let Some(token) = &self.token {
            if token.trim().is_empty() {
                return Err("token in .rpi/webui/openwebui.json must not be blank".into());
            }
            return Ok(Some(token.clone()));
        }
        Ok(if self.token_env.is_empty() {
            None
        } else {
            env(&self.token_env).filter(|v| !v.trim().is_empty())
        })
    }
}

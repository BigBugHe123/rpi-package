use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

pub const FLAGS: &[(&str, &str)] = &[
    ("webui", "UI adapter to start (openwebui)"),
    ("webui-url", "Existing Open WebUI instance URL"),
    ("webui-token", "Open WebUI admin token (or OPENWEBUI_TOKEN)"),
    (
        "webui-bind",
        "Adapter listen address (default 127.0.0.1:5234)",
    ),
    (
        "webui-adapter-url",
        "Adapter URL reachable from Open WebUI's backend",
    ),
    (
        "webui-permissions",
        "Permission policy: ask, bypass or deny (default ask)",
    ),
];

#[derive(Clone, Serialize, Deserialize)]
pub struct Config {
    pub ui: String,
    pub url: String,
    pub bind: SocketAddr,
    pub adapter_url: String,
    pub adapter_token: String,
    pub permissions: String,
    pub cwd: PathBuf,
    #[serde(default)]
    pub installed: bool,
    #[serde(default)]
    pub installed_version: String,
}

pub struct Launch {
    pub config: Config,
    pub path: PathBuf,
    pub admin_token: Option<String>,
    pub executable: PathBuf,
    pub args: Vec<String>,
}

fn text(value: Option<Value>, name: &str) -> Result<Option<String>, String> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.trim().is_empty() => Ok(Some(s)),
        _ => Err(format!("--{name} requires a value")),
    }
}

pub fn directory() -> Result<PathBuf, String> {
    if let Some(dir) = std::env::var_os("RPI_CODING_AGENT_DIR") {
        return Ok(PathBuf::from(dir).join("ui-adapter"));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or("Cannot locate rpi configuration directory")?;
    Ok(PathBuf::from(home).join(".rpi/agent/ui-adapter"))
}

fn endpoint(raw: &str) -> Result<String, String> {
    let parsed = url::Url::parse(raw).map_err(|_| "Invalid UI/adapter URL")?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(
            "UI/adapter URLs must be HTTP(S) URLs without credentials, query or fragment".into(),
        );
    }
    Ok(raw.trim_end_matches('/').to_owned())
}

pub fn resolve(mut flag: impl FnMut(&str) -> Option<Value>) -> Result<Launch, String> {
    let ui = text(flag("webui"), "webui")?.ok_or("Use --webui=openwebui")?;
    if ui != "openwebui" {
        return Err(format!("Unsupported UI '{ui}'; supported: openwebui"));
    }
    let path = directory()?.join(format!("{ui}.json"));
    let saved: Option<Config> = match std::fs::read(&path) {
        Ok(data) => Some(
            serde_json::from_slice(&data).map_err(|e| format!("Invalid saved UI config: {e}"))?,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("Cannot read saved UI config: {e}")),
    };
    let mut setting = |name: &str, env: &str| -> Result<Option<String>, String> {
        Ok(text(flag(name), name)?.or_else(|| std::env::var(env).ok().filter(|s| !s.is_empty())))
    };
    let url = endpoint(
        &setting("webui-url", "RPI_WEBUI_URL")?
            .or_else(|| saved.as_ref().map(|s| s.url.clone()))
            .ok_or("First start requires --webui-url <Open WebUI URL> or RPI_WEBUI_URL")?,
    )?;
    let bind: SocketAddr = setting("webui-bind", "RPI_WEBUI_BIND")?
        .or_else(|| saved.as_ref().map(|s| s.bind.to_string()))
        .unwrap_or_else(|| "127.0.0.1:5234".into())
        .parse()
        .map_err(|_| "Invalid --webui-bind socket address")?;
    if bind.port() == 0 {
        return Err("--webui-bind requires a nonzero port".into());
    }
    let adapter_url = endpoint(
        &setting("webui-adapter-url", "RPI_WEBUI_ADAPTER_URL")?
            .or_else(|| saved.as_ref().map(|s| s.adapter_url.clone()))
            .unwrap_or_else(|| format!("http://{bind}")),
    )?;
    if url::Url::parse(&adapter_url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .is_some_and(|h| h == "0.0.0.0" || h == "[::]")
    {
        return Err(
            "Set --webui-adapter-url to an address reachable by the Open WebUI backend".into(),
        );
    }
    let permissions = setting("webui-permissions", "RPI_WEBUI_PERMISSIONS")?
        .or_else(|| saved.as_ref().map(|s| s.permissions.clone()))
        .unwrap_or_else(|| "ask".into());
    if !matches!(permissions.as_str(), "ask" | "bypass" | "deny") {
        return Err("--webui-permissions must be ask, bypass or deny".into());
    }
    let admin_token = setting("webui-token", "OPENWEBUI_TOKEN")?;
    let previous = saved.as_ref();
    let installed = previous.is_some_and(|s| s.url == url && s.installed);
    let installed_version = previous
        .map(|s| s.installed_version.clone())
        .unwrap_or_default();
    let config = Config {
        ui,
        url,
        bind,
        adapter_url,
        permissions,
        adapter_token: previous
            .map(|s| s.adapter_token.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        cwd: std::env::current_dir().map_err(|e| e.to_string())?,
        installed,
        installed_version,
    };
    // Every run refreshes valves (cwd/address/policy). The admin token is read
    // for installation only and is never persisted in this config.
    let unchanged = previous.is_some_and(|s| {
        s.installed
            && s.installed_version == env!("CARGO_PKG_VERSION")
            && s.url == config.url
            && s.adapter_url == config.adapter_url
            && s.permissions == config.permissions
            && s.cwd == config.cwd
    });
    if admin_token.is_none() && !unchanged {
        return Err(
            "Automatic UI configuration requires --webui-token <admin token> or OPENWEBUI_TOKEN"
                .into(),
        );
    }
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut args = vec!["--acp".into()];
    for name in ["extensions-dir", "model", "provider"] {
        if let Some(value) = text(flag(name), name)? {
            args.extend([format!("--{name}"), value]);
        }
    }
    for name in ["offline", "local-only"] {
        if flag(name) == Some(Value::Bool(true)) {
            args.push(format!("--{name}"));
        }
    }
    Ok(Launch {
        config,
        path,
        admin_token,
        executable,
        args,
    })
}

pub fn save(path: &Path, config: &Config) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid config path")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let tmp = parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let result = (|| {
        let mut file = options.open(&tmp).map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn url_validation_rejects_credential_and_query_leaks() {
        for raw in [
            "file:///etc/passwd",
            "http://u:p@localhost",
            "https://x/?token=secret",
            "https://x/#x",
        ] {
            assert!(endpoint(raw).is_err());
        }
        assert_eq!(
            endpoint("https://example.com/webui/").unwrap(),
            "https://example.com/webui"
        );
    }
    #[test]
    fn missing_value_is_not_a_ui_name() {
        assert!(text(Some(Value::Bool(true)), "webui").is_err());
        assert!(
            resolve(|name| (name == "webui").then(|| Value::String("unknown".into())))
                .err()
                .unwrap()
                .contains("Unsupported UI")
        );
    }
}

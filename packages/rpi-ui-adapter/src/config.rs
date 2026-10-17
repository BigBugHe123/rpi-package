use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

pub const FLAGS: &[(&str, &str)] = &[(
    "webui",
    "UI adapter to start; reads .rpi/webui/<UI>.json (openwebui)",
)];

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
    /// Generated state, never the user's UI configuration file.
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

fn check_ui(ui: &str) -> Result<(), String> {
    match ui {
        "openwebui" => Ok(()),
        _ => Err(format!("Unsupported UI '{ui}'; supported: openwebui")),
    }
}

pub(super) fn endpoint(raw: &str) -> Result<String, String> {
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

fn resolve_project(
    ui: &str,
    cwd: PathBuf,
    env: impl Fn(&str) -> Option<String>,
) -> Result<(Config, PathBuf, Option<String>), String> {
    check_ui(ui)?;
    let directory = cwd.join(".rpi/webui");
    let source = directory.join(format!("{ui}.json"));
    // Each UI owns its own schema and validation. More UIs do not grow a
    // universal CLI flag list or overwrite another adapter's settings.
    let settings = crate::openwebui::settings::load(&source)?;
    let path = directory.join(".state").join(format!("{ui}.json"));
    let saved: Option<Config> = match std::fs::read(&path) {
        Ok(data) => Some(
            serde_json::from_slice(&data).map_err(|e| format!("Invalid UI adapter state: {e}"))?,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("Cannot read UI adapter state: {e}")),
    };
    let admin_token = settings.admin_token(&env)?;
    let previous = saved.as_ref();
    let config = Config {
        ui: ui.into(),
        url: settings.url,
        bind: settings.adapter.bind,
        adapter_url: settings.adapter.url.expect("validated adapter URL"),
        permissions: settings.permissions,
        adapter_token: previous
            .map(|s| s.adapter_token.clone())
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
        cwd,
        installed: previous.is_some_and(|s| s.installed),
        installed_version: previous
            .map(|s| s.installed_version.clone())
            .unwrap_or_default(),
    };
    let unchanged = previous.is_some_and(|s| {
        s.ui == config.ui
            && s.installed
            && s.installed_version == env!("CARGO_PKG_VERSION")
            && s.url == config.url
            && s.adapter_url == config.adapter_url
            && s.permissions == config.permissions
            && s.cwd == config.cwd
    });
    if admin_token.is_none() && !unchanged {
        return Err(format!("Open WebUI installation/configuration needs an admin token: set token or tokenEnv in {}", source.display()));
    }
    Ok((config, path, admin_token))
}

pub fn resolve(mut flag: impl FnMut(&str) -> Option<Value>) -> Result<Launch, String> {
    let ui = text(flag("webui"), "webui")?.ok_or("Use --webui=openwebui")?;
    check_ui(&ui)?;
    for name in [
        "webui-url",
        "webui-token",
        "webui-bind",
        "webui-adapter-url",
        "webui-permissions",
    ] {
        if flag(name).is_some_and(|v| !v.is_null()) {
            return Err(format!(
                "--{name} moved to .rpi/webui/{ui}.json; start with --webui={ui}"
            ));
        }
    }
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let (config, path, admin_token) = resolve_project(&ui, cwd, |name| std::env::var(name).ok())?;
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
    write_json(path, config, true)
}

pub(super) fn write_json(path: &Path, value: &impl Serialize, replace: bool) -> Result<(), String> {
    let parent = path.parent().ok_or("Invalid config path")?;
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let target = if replace {
        parent.join(format!(".{}.tmp", uuid::Uuid::new_v4()))
    } else {
        path.to_owned()
    };
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    use std::io::Write;
    let result = (|| {
        let mut file = options.open(&target).map_err(|e| e.to_string())?;
        file.write_all(&serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        file.write_all(b"\n").map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        if replace {
            std::fs::rename(&target, path).map_err(|e| e.to_string())?;
        }
        Ok(())
    })();
    if result.is_err() && replace {
        let _ = std::fs::remove_file(&target);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    struct Project(PathBuf);
    impl Project {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("rpi-webui-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn configure(&self, value: Value) -> PathBuf {
            let path = self.0.join(".rpi/webui/openwebui.json");
            write_json(&path, &value, false).unwrap();
            path
        }
    }
    impl Drop for Project {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

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
    fn unknown_ui_and_old_flags_fail_before_creating_files() {
        assert!(text(Some(Value::Bool(true)), "webui").is_err());
        assert!(resolve(|name| (name == "webui").then(|| json!("unknown")))
            .err()
            .unwrap()
            .contains("Unsupported UI"));
        let error = resolve(|name| match name {
            "webui" => Some(json!("openwebui")),
            "webui-url" => Some(json!("http://old")),
            _ => None,
        })
        .err()
        .unwrap();
        assert!(error.contains("moved to .rpi/webui/openwebui.json"));
    }

    #[test]
    fn first_start_creates_template_and_stops() {
        let root = Project::new();
        assert!(resolve_project("openwebui", root.0.clone(), |_| Some(
            "should-not-install".into()
        ))
        .err()
        .unwrap()
        .contains("Created"));
        let value: Value = serde_json::from_slice(
            &std::fs::read(root.0.join(".rpi/webui/openwebui.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(value["tokenEnv"], "OPENWEBUI_TOKEN");
        assert!(!root.0.join(".rpi/webui/.state").exists());
    }

    #[test]
    fn ui_specific_json_and_custom_token_env_are_authoritative() {
        let root = Project::new();
        let source=root.configure(json!({"url":"https://ui.example/", "tokenEnv":"MY_UI_TOKEN", "adapter":{"bind":"0.0.0.0:7777", "url":"http://host.docker.internal:7777"},"permissions":"bypass"}));
        let original = std::fs::read(&source).unwrap();
        let (mut config, state, token) = resolve_project("openwebui", root.0.clone(), |name| {
            (name == "MY_UI_TOKEN").then(|| "admin-secret".into())
        })
        .unwrap();
        assert_eq!(token.as_deref(), Some("admin-secret"));
        assert_eq!(config.bind.port(), 7777);
        assert_eq!(config.url, "https://ui.example");
        assert_eq!(state, root.0.join(".rpi/webui/.state/openwebui.json"));
        config.installed = true;
        config.installed_version = env!("CARGO_PKG_VERSION").into();
        save(&state, &config).unwrap();
        let (restored, _, token) = resolve_project("openwebui", root.0.clone(), |_| None).unwrap();
        assert!(token.is_none());
        assert_eq!(restored.adapter_token, config.adapter_token);
        assert_eq!(std::fs::read(&source).unwrap(), original);
        assert!(!std::fs::read_to_string(state)
            .unwrap()
            .contains("admin-secret"));
    }

    #[test]
    fn config_changes_require_installation_token() {
        let root = Project::new();
        let source = root.configure(json!({"url":"http://localhost:8080"}));
        let (mut config, state, _) =
            resolve_project("openwebui", root.0.clone(), |_| Some("first-token".into())).unwrap();
        config.installed = true;
        config.installed_version = env!("CARGO_PKG_VERSION").into();
        save(&state, &config).unwrap();
        std::fs::write(source, r#"{"url":"http://localhost:8081"}"#).unwrap();
        assert!(resolve_project("openwebui", root.0.clone(), |_| None)
            .err()
            .unwrap()
            .contains("admin token"));
    }

    #[test]
    fn defaults_literal_token_and_bom_are_supported() {
        let root = Project::new();
        let source = root.configure(json!({"url":"http://localhost:8080","token":"literal"}));
        let contents = std::fs::read_to_string(&source).unwrap();
        std::fs::write(source, format!("\u{feff}{contents}")).unwrap();
        let (config, _, token) =
            resolve_project("openwebui", root.0.clone(), |_| Some("from-env".into())).unwrap();
        assert_eq!(token.as_deref(), Some("literal"));
        assert_eq!(config.bind.to_string(), "127.0.0.1:5234");
        assert_eq!(config.adapter_url, "http://127.0.0.1:5234");
        assert_eq!(config.permissions, "ask");
    }

    #[test]
    fn invalid_ui_settings_are_rejected_without_echoing_secrets() {
        for value in [
            json!({"url":"http://ui","typo":"secret-value"}),
            json!({"url":"http://ui","adapter":{"bind":"0.0.0.0:5234"}}),
            json!({"url":"http://ui","permissions":"allow-everything"}),
            json!({"url":"http://ui","adapter":{"bind":"127.0.0.1:0"}}),
            json!({"url":"http://ui","token":42}),
        ] {
            let root = Project::new();
            root.configure(value);
            assert!(!resolve_project("openwebui", root.0.clone(), |_| None)
                .err()
                .unwrap()
                .contains("secret-value"));
        }
    }
}

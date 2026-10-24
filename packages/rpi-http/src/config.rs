//! Standalone API configuration. Frontend settings remain in rpi-web.
use crate::AppConfig;
use std::path::{Path, PathBuf};

pub const HELP: &str = "rpi-http [--config PATH] [--project PATH] [--rpi-bin PATH]
         [--bind IP] [--port N] [--registry PATH] [--session-dir PATH]
         [--add-project PATH]... [--projects-root PATH] [--help] [--version]

Serves /api/v1 over HTTP/SSE without frontend assets.
JSON paths are relative to the config file; CLI options override JSON.
Authentication: RPI_HTTP_PASSWORD and RPI_HTTP_ALLOWED_HOSTS.
Legacy RPI_WEB_* variables remain accepted.";

#[derive(Debug)]
pub enum Launch {
    Run(Box<AppConfig>),
    Help,
    Version,
}

pub fn from_args(args: impl IntoIterator<Item = String>) -> Result<Launch, String> {
    let args = args.into_iter().collect::<Vec<_>>();
    let mut config_file = None;
    let mut options = Vec::new();
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        let (name, inline) = argument
            .split_once('=')
            .map_or((argument.as_str(), None), |(name, value)| {
                (name, Some(value.to_owned()))
            });
        match name {
            "--help" | "-h" => return Ok(Launch::Help),
            "--version" | "-V" => return Ok(Launch::Version),
            "--config" | "--project" | "--rpi-bin" | "--bind" | "--port" | "--registry"
            | "--session-dir" | "--add-project" | "--projects-root" => {
                let value = if let Some(value) = inline {
                    value
                } else {
                    index += 1;
                    args.get(index)
                        .filter(|value| !value.starts_with("--"))
                        .cloned()
                        .ok_or_else(|| format!("{name} requires a value"))?
                };
                if value.is_empty() {
                    return Err(format!("{name} requires a value"));
                }
                if name == "--config" {
                    config_file = Some(PathBuf::from(value));
                } else {
                    options.push((name.to_owned(), value));
                }
            }
            _ => return Err(format!("unknown option: {argument}")),
        }
        index += 1;
    }
    let mut config = if let Some(path) = config_file {
        let bytes = std::fs::read(&path)
            .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
        let mut config: AppConfig = serde_json::from_slice(&bytes)
            .map_err(|error| format!("invalid {}: {error}", path.display()))?;
        let base = path.parent().unwrap_or(Path::new("."));
        config.project = resolve(base, &config.project);
        config.registry = resolve(base, &config.registry);
        for project in &mut config.projects {
            *project = resolve(base, project);
        }
        if config.rpi_bin.components().count() > 1 {
            config.rpi_bin = resolve(base, &config.rpi_bin);
        }
        for path in [&mut config.session_dir, &mut config.projects_root]
            .into_iter()
            .flatten()
        {
            *path = resolve(base, path);
        }
        config
    } else {
        AppConfig::default()
    };
    for (name, value) in options {
        match name.as_str() {
            "--project" => config.project = PathBuf::from(value),
            "--rpi-bin" => config.rpi_bin = PathBuf::from(value),
            "--bind" => config.bind = value,
            "--port" => {
                config.port = value
                    .parse()
                    .map_err(|_| "--port must be between 0 and 65535")?
            }
            "--registry" => config.registry = PathBuf::from(value),
            "--session-dir" => config.session_dir = Some(PathBuf::from(value)),
            "--add-project" => config.projects.push(PathBuf::from(value)),
            "--projects-root" => config.projects_root = Some(PathBuf::from(value)),
            _ => unreachable!(),
        }
    }
    Ok(Launch::Run(Box::new(config)))
}

fn resolve(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn json_paths_and_cli_precedence_are_independent_of_argument_order() {
        let directory =
            std::env::temp_dir().join(format!("rpi-http-config-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&directory).unwrap();
        let file = directory.join("http.json");
        std::fs::write(&file,r#"{"project":"workspace","rpiBin":"bin/rpi","port":3030,"projects":["extra"],"sessionDir":"history"}"#).unwrap();
        let Launch::Run(config) = from_args([
            "--port".into(),
            "0".into(),
            "--config".into(),
            file.to_string_lossy().into(),
            "--add-project=other".into(),
        ])
        .unwrap() else {
            panic!("expected server config")
        };
        assert_eq!(config.port, 0);
        assert_eq!(config.service_name, "rpi-http");
        assert_eq!(config.project, directory.join("workspace"));
        assert_eq!(config.rpi_bin, directory.join("bin/rpi"));
        assert_eq!(
            config.projects,
            [directory.join("extra"), PathBuf::from("other")]
        );
        assert_eq!(config.session_dir, Some(directory.join("history")));
        std::fs::remove_dir_all(&directory).unwrap();
    }
}

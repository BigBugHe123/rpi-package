#![forbid(unsafe_code)]
use rpi_plugin_sdk::handles::ToolHandles;
use rpi_plugin_sdk::{
    FreeStringFn, StableToolSchema, StbString, StbStringRef, StepHandle, StepResult, ToolPartialCb,
};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    ffi::c_void,
    path::{Path, PathBuf},
};

fn publish(params: &Value) -> Result<String, String> {
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    publish_from(&root, params)
}

fn publish_from(root: &Path, params: &Value) -> Result<String, String> {
    let files = params["files"].as_array().ok_or("files must be an array")?;
    if files.is_empty() || files.len() > 64 {
        return Err("Publish requires 1 to 64 files".into());
    }
    let dir = cap_std::fs::Dir::open_ambient_dir(root, cap_std::ambient_authority())
        .map_err(|e| e.to_string())?;
    let mut seen = BTreeSet::new();
    let mut total = 0;
    let mut cards = Vec::new();
    for file in files {
        let file = file.as_str().ok_or("Each file must be a string")?;
        if file.is_empty()
            || file.len() > 1024
            || file.contains(['\\', ':', '\0'])
            || file
                .split('/')
                .any(|p| p.is_empty() || p == "." || p == "..")
            || file
                .split('/')
                .next()
                .is_some_and(|p| p.eq_ignore_ascii_case(".rpi"))
            || Path::new(file).is_absolute()
            || !seen.insert(file)
        {
            return Err(format!("Invalid or duplicate publication path: {file}"));
        }
        let mut component = PathBuf::new();
        for part in Path::new(file).components() {
            component.push(part);
            if dir
                .symlink_metadata(&component)
                .map_err(|e| e.to_string())?
                .is_symlink()
            {
                return Err("Publication paths cannot contain symlinks".into());
            }
        }
        let metadata = dir
            .open(file)
            .and_then(|f| f.metadata())
            .map_err(|e| e.to_string())?;
        if !metadata.is_file() || metadata.len() > 16 * 1024 * 1024 {
            return Err(format!("Not a regular file or exceeds 16 MiB: {file}"));
        }
        total += metadata.len();
        if total > 64 * 1024 * 1024 {
            return Err("Publication exceeds 64 MiB".into());
        }
        let name = Path::new(file).file_name().unwrap().to_string_lossy();
        cards.push(
            json!({"path":file,"name":name,"size":metadata.len(),"mimeType":mime_type(file)}),
        );
    }
    Ok(json!({"content":[{"type":"text","text":"Files ready."}],
        "details":{"type":"artifact_publication","version":1,"files":cards}})
    .to_string())
}

fn mime_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("txt" | "md" | "csv" | "json" | "log") => "text/plain; charset=utf-8",
        Some("pdf") => "application/pdf",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("mp3") => "audio/mpeg",
        Some("wav") => "audio/wav",
        Some("ogg") => "audio/ogg",
        Some("m4a") => "audio/mp4",
        Some("mp4") => "video/mp4",
        Some("webm") => "video/webm",
        _ => "application/octet-stream",
    }
}

static HANDLES: ToolHandles<Drive> = ToolHandles::new();

struct Drive {
    params: Value,
    done: bool,
}
extern "C" fn execute(
    _: StbStringRef,
    params: StbString,
    free: Option<FreeStringFn>,
) -> StepHandle {
    let text = params.to_string_lossy();
    params.free_with(free);
    HANDLES.insert(Drive {
        params: serde_json::from_str(&text).unwrap_or(Value::Null),
        done: false,
    })
}
extern "C" fn poll(handle: StepHandle, _: Option<ToolPartialCb>, _: *mut c_void) -> StepResult {
    if handle.is_null() {
        return StepResult::err(StbString::from_string("null publication handle".into()));
    }
    let Some(entry) = HANDLES.get(handle) else {
        return StepResult::err(StbString::from_string(
            "invalid or destroyed tool handle".into(),
        ));
    };
    let mut drive = entry.lock();
    if entry.cancelled.load(std::sync::atomic::Ordering::SeqCst) {
        return StepResult::err(StbString::from_string("publication cancelled".into()));
    }
    if drive.done {
        return StepResult::err(StbString::from_string(
            "publication polled after completion".into(),
        ));
    }
    drive.done = true;
    match publish(&drive.params) {
        Ok(value) => StepResult::done(StbString::from_string(value)),
        Err(error) => StepResult::err(StbString::from_string(error)),
    }
}
extern "C" fn cancel(handle: StepHandle) {
    HANDLES.cancel(handle);
}
extern "C" fn destroy(handle: StepHandle) {
    drop(HANDLES.remove(handle));
}
use rpi_plugin_sdk::callbacks::free_string;

// The pinned SDK generates a safe C ABI entrypoint that validates the host pointer.
// Keep this exception scoped to that generated entrypoint.
#[allow(clippy::not_unsafe_ptr_arg_deref)]
mod entrypoint {
    use super::*;
    rpi_plugin_sdk::export_plugin!(|api| {
        let Some(register) = api.register_tool else {
            return 1;
        };
        let schema = StableToolSchema {
        name: StbString::from_string("publish_artifacts".into()),
        description: StbString::from_string("Publish existing workspace files as downloadable file cards. References use the original files: later edits change downloads and deletion makes them unavailable. Accepts 1 to 64 workspace-relative paths, outside .rpi, without symlinks; limits are 16 MiB per file and 64 MiB total. Creates no copies.".into()),
        parameters: StbString::from_string(r#"{"type":"object","properties":{"files":{"type":"array","items":{"type":"string"},"minItems":1,"maxItems":64}},"required":["files"],"additionalProperties":false}"#.into()),
    };
        register(&schema, execute, poll, cancel, destroy, free_string)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_complete_cards_without_writing_anything() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("report.pdf"), b"report").unwrap();
        let result: Value = serde_json::from_str(
            &publish_from(root.path(), &json!({"files":["report.pdf"]})).unwrap(),
        )
        .unwrap();
        assert_eq!(
            result["details"]["files"],
            json!([{"path":"report.pdf","name":"report.pdf","mimeType":"application/pdf","size":6}])
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
        assert_eq!(
            std::fs::read(root.path().join("report.pdf")).unwrap(),
            b"report"
        );
    }

    #[test]
    fn rejects_invalid_batches_and_workspace_escape() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("good.txt"), b"good").unwrap();
        for path in [
            "../outside",
            "/absolute",
            "C:/absolute",
            ".rpi/settings.json",
            "missing",
            "good.txt",
            "folder/../good.txt",
            "folder\\good.txt",
        ] {
            assert!(
                publish_from(root.path(), &json!({"files":["good.txt",path]})).is_err(),
                "{path}"
            );
        }
        assert!(publish_from(root.path(), &json!({"files":[]})).is_err());
        assert!(publish_from(root.path(), &json!({"files":vec!["good.txt";65]})).is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }

    #[test]
    fn rejects_file_and_batch_size_limits() {
        let root = tempfile::tempdir().unwrap();
        let mut paths = Vec::new();
        for i in 0..5 {
            let path = format!("{i}.bin");
            std::fs::File::create(root.path().join(&path))
                .unwrap()
                .set_len(16 * 1024 * 1024)
                .unwrap();
            paths.push(path);
        }
        assert!(publish_from(root.path(), &json!({"files":paths})).is_err());
        std::fs::File::create(root.path().join("large.bin"))
            .unwrap()
            .set_len(16 * 1024 * 1024 + 1)
            .unwrap();
        assert!(publish_from(root.path(), &json!({"files":["large.bin"]})).is_err());
    }
}

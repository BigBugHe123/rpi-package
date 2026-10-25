# rpi-artifacts

Native `publish_artifacts` tool for rpi-webui-adapte. No prompt injection, HTTP callback, manifest files, file copies, or publication directories.

Build independently from the agent workspace root (do not build the parent plugin workspace):

```powershell
cargo build --manifest-path rpi-package/packages/rpi-artifacts/Cargo.toml
```

Load the resulting `rpi-package/packages/rpi-artifacts/target/debug/rpi_artifacts.dll` through the host's native extension configuration (`--extension <absolute-dll-path>` for standalone use). For managed sessions register and select this native plugin through management, using the same extension packaging as other plugins. The plugin must be loaded before a session worker starts.

Tool arguments: `{"files":["output/report.pdf"]}`. Successful tool results carry `details: {"type":"artifact_publication","version":1,"files":[{path,name,size,mimeType}]}`. The adapter accepts only a successful matching `publish_artifacts` RPC execution, places the returned complete file cards into the owning message and emits `artifact.created` without filesystem access or separate publication registration. The adapter deduplicates by run ID and tool-call ID. Other tool results and assistant text cannot publish files.

Paths are workspace-relative. No session IDs, absolute paths or credentials are supplied by the model. Published content changes with the original file; deletion or workspace cleanup makes it unavailable. Downloads map a session to its workspace and requested relative file path; no publication lookup or publication expiry is required. Sessions sharing a workspace share its files.

The native tool result has both `content` and `details`. Its text content contains the same file-result JSON as `details`: providers send `content` to the model, while native history also preserves `details` for structured consumers. Keeping file paths and metadata in content lets the Agent use the result after reopening a session. This is tool result data, with no behavior instructions or frontend card fields.

Recovery is tested directly against the existing host using `rpi-webui-adapte/tests/publication_recovery.py`, without the adapter or UI. Completed history can continue with read/write calls using its saved tool result. Interrupted runs can use `resume_pending`; the tested host may call publication again before finishing. This tool only reads file metadata, so repeat execution creates no copies and changes no source files.

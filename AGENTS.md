# rpi-package project guidance

This repository contains the shared native runtime and protocol adapters for rpi. Before changing architecture, session loading, transcripts, streaming, or adapter boundaries, read:

- `skills/rpi-architecture-boundaries/SKILL.md`
- `docs/architecture/extension-core.md`
- the relevant package README and tests

## Architecture rules

- Keep the dependency direction `rpi-web -> rpi-http -> rpi-extcore -> native rpi --mode rpc` and `rpi-acp -> rpi-extcore`.
- Put protocol-neutral runtime semantics, native child lifecycle, request correlation, runs, cancellation, event replay, bounded transcript snapshots, history validation, project/workspace services, registry metadata, and timings in `rpi-extcore`.
- Keep HTTP extraction/serialization, status mapping, cookies/authentication, Host/Origin checks, SSE, and listener lifecycle in `rpi-http`.
- Keep ACP JSON-RPC, capabilities, content conversion, stop reasons, tool/diff/plan formatting, ACP transport tasks, and Zed integration in `rpi-acp`.
- Keep React/Vite resources, UI row projection, Markdown/tool-card rendering, scrolling/virtualization, Web configuration, assets, and Web entrypoints in `rpi-web`.
- Native transcripts remain owned by the native host; core reads and validates them. Registry/timing files are supplemental metadata, not replacement transcripts.
- Do not put Axum, ACP, frontend, or plugin ABI types in extcore. Do not move `rowsFromMessages()` or presentation logic into extcore.
- Preserve `--webserver`, `--web`, `.rpi/web.json`, environment compatibility, explicit assets, `/api/v1`, HTTP/SSE, and ACP v1 behavior unless a change explicitly includes compatibility tests.

## Review and change discipline

- Inspect actual source, call sites, diffs, and tests; commit titles are not evidence.
- For shared behavior, define a neutral core API first, then add thin adapter mappings.
- Keep message/history reads bounded and resumable, and preserve the event sequence boundary so snapshot plus live replay cannot lose events.
- The resource creator owns cancellation, timeout, shutdown, and cleanup.
- Do not claim a runtime upgrade or performance fix until the running binary/extension source has been identified and the service has been restarted and measured.
- Architecture-only requests must not modify runtime implementation code. Documentation and tests are allowed when explicitly requested.

Run relevant checks after implementation, including package tests, frontend `npm run typecheck`, and native smoke/integration checks for snapshot, SSE, startup, authentication, ACP streaming, or lifecycle changes.

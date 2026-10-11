# ACP Registry packaging

Everything needed to publish rpi as an entry in the
[Agent Client Protocol registry](https://github.com/agentclientprotocol/registry),
which Zed surfaces as the **Agent Servers** store.

The registry only accepts two distribution shapes:

* `npx` — an npm package, or
* `binary` — a per-platform archive with a `cmd` entry point and a `sha256`.

A `cdylib` rpi extension cannot be published, so the registry entry ships the
**standalone bridge binary** [`rpi-acp-bridge`](../packages/rpi-acp/README.md#standalone-binary-acp-registry)
instead. It speaks ACP on its own stdin/stdout and spawns `rpi --mode rpc`
itself, so it needs no host extension loading.

## Produce the archives

Per platform (run on that platform — the archive contains a native binary):

```bash
task pack-acp
```

This stages `dist/rpi-acp-<slug>/` and builds a **flat** archive
(`dist/rpi-acp-<slug>.zip` on Windows, `dist/rpi-acp-<slug>.tar.gz` elsewhere),
then prints the new `sha256` and a ready-to-paste `distribution.binary` stanza.

`task pack-acp` names archives with the local cargo slug; the registry keys use
Go's `GOOS-GOARCH` spelling. Map them when filling in `agent.json`:

| archive (`pack-acp`)          | registry key      |
| ----------------------------- | ----------------- |
| `rpi-acp-linux-x86_64`        | `linux-x86_64`    |
| `rpi-acp-linux-aarch64`       | `linux-aarch64`   |
| `rpi-acp-macos-x86_64`        | `darwin-x86_64`   |
| `rpi-acp-macos-aarch64`       | `darwin-aarch64`  |
| `rpi-acp-windows-x86_64`      | `windows-x86_64`  |

## Fill in `rpi-acp/agent.json`

`rpi-acp/agent.json` is the entry to submit. Replace every placeholder:

1. `version` — the release tag the archives were built from.
2. `<release-url>` — e.g.
   `https://github.com/pi-rust/rpi-package/releases/download/v0.1.0`.
3. `sha256` for each platform — printed by `task pack-acp` (a value not in the
   table below means that platform's archive has not been built yet).

Only include the platforms you actually built; the schema allows a subset.
`icon.svg` is served from the registry CDN
(`https://cdn.agentclientprotocol.com/registry/v1/latest/rpi-acp.svg`), so it is
not referenced from `agent.json`.

## Before opening the PR

The registry is curated. Checklist:

- [ ] Release assets uploaded for every listed platform (`rpi-acp-<slug>.{zip,tar.gz}`),
      and each `sha256` re-checked against the downloaded asset.
- [ ] `rpi-acp-bridge` starts and answers `initialize` on the release binary
      (not just a local debug build).
- [ ] `LICENSE` is present (MIT, `license_url` set).
- [ ] `icon.svg` is the real mark, not the placeholder in this folder.
- [ ] Decide on `authMethods`: the bridge currently advertises **none**, so rpi
      relies on provider credentials already present in the environment. A
      curated entry that expects interactive sign-in must implement and advertise
      ACP auth — treat that as an explicit gap rather than an oversight.

## Submit

1. Fork `agentclientprotocol/registry`.
2. Add `rpi-acp/agent.json` and `rpi-acp/icon.svg` (copy from this folder).
3. Open a PR; the registry validates the schema and re-hashes the archives.

Until it is accepted, the local path still works: register the binary (or the
`rpi --acp` extension) under `agent_servers` in Zed's `settings.json` — see the
[package README](../packages/rpi-acp/README.md#configure-zed).

# rpi-acp

Run **rpi as an external agent in [Zed](https://zed.dev)** (or any client that
speaks the [Agent Client Protocol](https://agentclientprotocol.com)) over a
single `rpi --acp` process.

```
Zed  ──ACP (JSON-RPC over stdio)──▶  rpi --acp  ──spawn──▶  rpi --mode rpc  (per session)
```

## How it works

`rpi-acp` is a `cdylib` extension. It does three things:

1. **Claims the process stdio.** When `--acp` is present, the extension reads
   the flag *during plugin registration* and `declare`s
   `{"ownsStdio": true}`. The host then (a) does **not** drain piped stdin, and
   (b) runs its headless `Rpc` lifecycle instead of the TUI/print modes, waiting
   for the owner to finish or Ctrl+C. From that point the process's stdin/stdout belong to this
   extension. This is what makes an ACP stdio server possible inside an `rpi`
   process at all — see *Requirements* below.
2. **Serves ACP v1** on stdio (`initialize`, `session/new`, `session/prompt`,
   `session/cancel`). See `src/protocol.rs`.
3. **Backs each ACP session with a child `rpi --mode rpc` process** rooted at
   the session's `cwd`, translating its event stream into ACP `session/update`
   notifications (streamed text, thinking, tool calls). See `src/session.rs`.

The host process itself runs no model call: every prompt is served by the child,
so tools, models, auth, skills and extensions behave exactly as in an
interactive `rpi` session.

### Event mapping

| rpi `--mode rpc` event | ACP `session/update` |
| --- | --- |
| `message_update` / `text_delta` | `agent_message_chunk` |
| `message_update` / `thinking_delta` | `agent_thought_chunk` |
| `tool_execution_start` (`read`, `bash`, …) | `tool_call` (`in_progress`, `rawInput`) |
| `tool_execution_start` (`edit`/`write`) | `tool_call` with ACP `diff` content |
| `tool_execution_update` | `tool_call_update` (`in_progress`, content) |
| `tool_execution_end` | `tool_call_update` (`completed` / `failed`, `rawOutput`) |
| `tool_execution_end` (`plan_mode_complete`) | `plan` (step list + done markers) |
| response `outcome: aborted` | prompt `stopReason: cancelled` |
| response `outcome: completed` | prompt `stopReason: end_turn` |

## Requirements

* `rpi` whose plugin ABI supports the `ownsStdio` declaration (the field is
  additive on the `declare` callback) and `StdioLifecycle` runtime action (21).
  The owner reports EOF/completion to the host, which dispatches
  `SessionShutdown` and returns its exit code. Conflicting owners are rejected.
* A configured rpi provider/model. The ACP host process resolves the default
  model at startup (like `rpi --server`); a missing credential is reported on
  stderr and the process exits before the ACP server starts.

## Install

```powershell
rpi install rpi-acp
# or, in this repo:
task install
```

## Configure Zed

Add a custom external agent to Zed's `settings.json`
(`agent: open settings` → *External Agents* → *Add Agent* → *Custom*):

```json
{
  "agent_servers": {
    "rpi": {
      "type": "custom",
      "command": "rpi",
      "args": ["--acp"]
    }
  }
}
```

On Windows, point `command` at the full path to `rpi.exe` if it is not on
`PATH`. Start an rpi thread from the Agent Panel's new-thread menu.

You can pass ordinary rpi flags through `args`, e.g.
`["--acp", "--model", "gateway/model"]`. The child session inherits the process
environment, so provider API keys work as usual.

## Standalone binary (ACP Registry)

The same ACP server also builds as a standalone binary, `rpi-acp-bridge`. Unlike
the `cdylib` extension it **owns its stdin/stdout natively** — it does not need
the host `ownsStdio` declaration above, so it runs against a stock `rpi`:

```
Zed  ──ACP (JSON-RPC over stdio)──▶  rpi-acp-bridge  ──spawn──▶  rpi --mode rpc  (per session)
```

```bash
cargo build --release -p rpi-acp --bin rpi-acp-bridge
```

`rpi-acp-bridge` locates the agent binary to spawn in this order:

1. `RPI_BIN` (explicit path),
2. itself, when the process *is* `rpi` (the `cdylib`-extension case),
3. a sibling `rpi` / `rpi.exe` next to the bridge,
4. `rpi` on `PATH`.

Point Zed straight at the binary if you would rather not load the extension:

```json
{
  "agent_servers": {
    "rpi": { "type": "custom", "command": "rpi-acp-bridge", "args": [] }
  }
}
```

### Registering with Zed

Zed learns about external agents from the `agent_servers` key in its
`settings.json`. You do not have to edit that by hand:

```bash
rpi-acp-bridge --install-zed     # add agent_servers.rpi
rpi-acp-bridge --print-zed       # print the snippet instead of writing it
```

`--install-zed` is idempotent, writes atomically, keeps a `settings.json.rpi-bak`
copy the first time it changes the file, and **preserves the file's comments and
formatting**: Zed's `settings.json` is JSONC (it allows `//` comments and
trailing commas), so the merge splices in one member rather than reparsing and
re-emitting the document. `--settings <FILE>`, `--command <FILE>` and
`--dry-run` are also accepted.

Running **`rpi --acp` does this automatically**: the extension calls the same
merge at startup, because `--acp` is exactly the signal that the user wants Zed
integration. It is a no-op once `agent_servers.rpi` exists, so a Zed-launched
child never writes. Set `RPI_ACP_NO_AUTO_REGISTER_ZED=1` to refuse the write and
print the snippet instead (useful if your editor config is under version
control).

### Packaging for the ACP Registry

```bash
task pack-acp
```

stages `dist/rpi-acp-<platform>/` and builds a **flat** archive
(`dist/rpi-acp-<platform>.zip` on Windows, `.tar.gz` elsewhere) whose root holds
the binary — matching the registry's `cmd` convention (e.g. `./rpi-acp-bridge`).
It prints the sha256 and a ready-to-paste `distribution.binary` stanza; see
[`registry/rpi-acp/agent.json`](../../registry/rpi-acp/agent.json) and
[`registry/README.md`](../../registry/README.md).

## Capabilities and limitations

* **Stable ACP v1** only. `loadSession` is not advertised, so `session/load` and
  `session/resume` return an error. Each ACP session is a fresh child process
  (in-memory history for the lifetime of the ACP connection).
* The client-side `fs/*` and `terminal/*` methods are **not** used: rpi runs its
  own `read`/`write`/`edit`/`bash` tools in the session `cwd`. This keeps the
  agent behavior identical to the terminal.
* Prompt input: `text`, `resource_link`, and embedded `resource` blocks are
  supported (`promptCapabilities.embeddedContext: true`). Images/audio are not
  (`rpi --mode rpc` takes a single text prompt).
* Only `agent_message_chunk`, `agent_thought_chunk`, `tool_call` (with `diff`
  content for `edit`/`write`), `tool_call_update`, and `plan` updates are
  emitted.
* **`session/request_permission`** is used: when a child's plugin opens a UI
  dialog mid-run (e.g. the `rpi-permission-gate` extension confirming a `bash`
  command), `rpi-acp` maps it to a `session/request_permission` request and
  sends the user's choice back to the plugin. The child is launched with
  `--permission-gate` by default so Zed shows approval prompts; set
  `RPI_ACP_NO_PERMISSION_GATE=1` to opt out. Only `confirm`/`selector` dialogs
  with explicit `purpose: "permission"` and per-option `permission` decisions
  map to a permission request. Option titles are preserved in the returned
  answer. Ordinary questions are cancelled because this adapter has no generic
  question UI. Closure, prompt cancellation and disconnect retire pending
  requests, including when the client never answers.
* Prompt templates / slash commands are not advertised as
  `available_commands_update`: `--mode rpc` does not expand prompt templates
  (only the host TUI does), so a client-side command list would be misleading.

## Manual smoke test

```powershell
$ext = "target\debug"   # dir containing rpi_acp.dll / librpi_acp.so
'{ "jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}} }' |
  rpi --acp --no-session --extensions-dir $ext
```

The only thing on stdout must be the `initialize` result JSON — no banner, no
log line (all diagnostics go to stderr).

## License

MIT

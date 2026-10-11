# rpi-permission-gate

A **generic tool-call permission gate** for `rpi`. Before a potentially
dangerous tool runs (`bash`, `write`, `edit`), it asks the user to confirm
through the host's extension UI dialog and blocks the call if declined.

```
agent loop ──before_tool_call──▶ pi on("tool_call")  ──▶ rpi-permission-gate
                                                          │  UiDialog confirm
                                                          ▼
                             TUI dialog   or   `--mode rpc` UI bridge ──▶ client
```

It is deliberately policy-only: it depends on two generic host capabilities —
the `on("tool_call")` veto (`before_tool_call`) and the `UiDialog` runtime
action (17) — and knows nothing about any protocol. The same plugin works in the
interactive TUI and, via the rpc UI bridge, under an ACP client such as
[`rpi-acp`](../rpi-acp) (which renders the confirm as
`session/request_permission`).

## Enable

Off by default, because a headless client that cannot answer UI dialogs would
otherwise hang the run.

```bash
rpi --permission-gate          # flag
RPI_PERMISSION_GATE=1 rpi      # or env
```

`rpi-acp` passes `--permission-gate` to its child sessions by default (so Zed
shows approval prompts); set `RPI_ACP_NO_PERMISSION_GATE=1` to opt out.

## Behavior

| Tool | Summary shown | Gate |
| --- | --- | --- |
| `bash` | the command (truncated) | yes |
| `write` | `write <path> (<n> bytes)` | yes |
| `edit` | `edit <path> (<n> replacements)` | yes |
| anything else | — | not gated |

* **Allow** → the tool runs unchanged.
* **Decline** → the handler returns `EVENT_HANDLER_ABORT`; the host blocks the
  tool and the model sees an error result (`extension ... blocked tool ...`).
* **No UI / dialog failure / timeout** → **blocked** (fail closed).

## Install

```bash
rpi install rpi-permission-gate
```

## Limitations

* Only `bash` / `write` / `edit` are gated; the policy (which commands count as
  dangerous) is intentionally simple and local — it does not parse command
  arguments beyond the ones above.
* No "remember this choice" / per-project allowlist yet. The `UiDialog`
  contract is per-call; persistent rules would be a separate policy layer (see
  `rpi-permissions`, which stores rules but is not yet wired to
  `before_tool_call`).
* One confirm per tool call (no batching of a parallel batch).

## License

MIT

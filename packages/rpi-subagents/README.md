# rpi-subagents

`rpi-subagents` registers `delegate_task`, a bounded subagent runner for rpi.
The tool starts a child `rpi --mode rpc` process, streams text deltas through the
normal tool-progress callback, and returns the child's final report as a tool
result.

## Bounds and recursion

- `task` is required and limited to 20,000 bytes.
- `role` defaults to `worker` and is limited to 200 bytes.
- `maxTurns` is clamped to `1..=32`.
- `timeoutSeconds` is clamped to `10..=1800`.
- The child runs with `--no-session`, so one-shot delegations do not create
  persistent session files.
- Child output is capped at a shared total of 4 MiB across streamed deltas and
  the final report, with at most 8,192 partial events; individual RPC lines are
  also size-limited. Exceeding a limit aborts the child and returns an error.
- `RPI_MAX_TURNS_PER_RUN` is set for the child so the rpi harness enforces the
  requested turn budget at the end of each turn. The RPC `turn_start` watcher
  remains a best-effort compatibility fallback for older hosts.
- Child process trees are owned and terminated as a unit: Unix process groups
  and Windows Job Objects are used for cancellation, timeout, and cleanup.
- The child receives an explicit no-delegation instruction. `rpi-subagents`
  itself is loaded by the child but declines to register when
  `RPI_SUBAGENT_CHILD=1`; this prevents recursion without disabling provider
  extensions needed by the delegated agent.
- The child inherits the host's permission-gate environment. The extension no
  longer silently disables permission gating; when a gate is enabled, the host
  must provide a dialog bridge or choose an explicit headless policy.
- Cancellation terminates the child process.

The executable is resolved from `RPI_SUBAGENT_BIN`, a running `rpi` binary, a
sibling `rpi` executable, or `PATH` in that order. `RPI_SUBAGENT_BIN` is useful
for integration tests and packaged deployments where the host binary is not on
`PATH`.

The plugin ABI does not expose an in-process child-agent spawn API, so this
extension uses rpi's stable JSONL RPC interface as the compatibility bridge.
The delegation metadata remains in the final result under
`kind: "rpi-delegation"`.

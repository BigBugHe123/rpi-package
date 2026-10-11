# rpi-run-stats

Live runtime monitor for rpi. Disabled by default; enable with `/stats on`.
Monitoring occupies one row directly below the built-in token/cache/context
usage footer. Chat and the editor retain their full width. Closing monitoring
removes that row. Narrow terminals truncate the row without wrapping it.

The footer monitor works with both native scrollback (`regular`) and fullscreen
mode, including macOS's default native scrollback mode.
On Mac keyboards the shortcut may require Fn+F8; `/stats on` works independently
of function-key settings.

- `/stats on` opens the panel; `/stats off` hides it while counting continues.
- **F8** toggles the monitor without submitting a command or altering the draft.
  Holding it does not repeatedly toggle; modified F8 keys are left unclaimed.
- `/stats` or `/stats toggle` toggles visibility.
- Position is fixed below the usage footer. Legacy `/stats position ...` calls
  explain the new location; visibility preferences apply to the current process.
- Rounds count agent runs; steps count provider requests in the current session.
- TTFT measures request dispatch to the first nonempty text, thinking or tool-call delta.
- tok/s measures latest response output tokens divided by total request time, including TTFT.
- TPM is reported token usage from requests completed within the last 60 seconds, including input, output and cache tokens. It is not a provider rate-limit quota or an extrapolated speed.
- Input, output, cache read/write, errors/aborts and USD cost accumulate in the current session. Missing timing/cost is displayed as `—`.

Labels: **TPS** (output tokens per second), **TTFT** (time to first token),
**Latency** (request duration), **TPM** (rolling tokens per minute), **Err**
(errors/aborts), and **USD** (reported cost). Input/output/cache usage remains
in the built-in footer above, without being duplicated. Counts use `k` and `M`
abbreviations. **Turns** counts agent runs; **Steps** counts provider requests.

Streaming timing updates refresh once per second; completed requests and mode
changes update immediately. Unchanged panel content is not republished.

The plugin uses the host's registered `UiComponent` text interface;
the host does not interpret monitor metrics or know this plugin's identity.
It requires a host with `UiComponent` action 22 support. Older hosts do not display the
panel. Headless runs collect data without displaying a panel. Counts start on
load and reset at session start, rather than replaying history.

Build: `cargo build --release -p rpi-run-stats`; install the platform cdylib in `~/.rpi/agent/extensions` and restart rpi.

The footer position is registered once. Later messages update text only; F8 uses
show/hide and preserves the registration. Host unload cleans up the component.

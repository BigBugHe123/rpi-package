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
- Turns count agent runs; LLM Calls count provider requests in the current session.
- Tools count actual tool executions when they start, including calls that later
  fail or abort. Tool completion events do not count again. These counts also
  accumulate while the monitor is hidden and reset at session start.
- TTFT measures request dispatch to the first nonempty text, thinking or tool-call delta.
- tok/s measures latest response output tokens divided by total request time, including TTFT.
- TPM is reported token usage from requests completed within the last 60 seconds, including input, output and cache tokens. It is not a provider rate-limit quota or an extrapolated speed.
- Input, output, cache read/write, errors/aborts and USD cost accumulate in the current session. Missing timing is displayed as `—`. USD is hidden when the current model has no pricing.

The compact row has three groups: counts, response performance, and usage/cost.
Only the two group boundaries have dividers; fields have no fixed-width padding:

```text
↻ 1 ◇ 3 ⌘ 2 │ 39t/s ◴ 2.4s ◷ 16.2s │ 319k/m
```

The icons are single-cell monochrome text symbols, with one space between each
icon and its number. They do not use emoji presentation or font-specific glyphs.
**↻** counts agent runs, **◇** counts provider requests, and **⌘** counts tool
executions. **t/s** is output tokens per second, **◴** is time to first token
(TTFT), **◷** is request duration, and **/m** is rolling tokens per minute (TPM).
**!** counts model request errors/aborts and appears in the counts group only
when nonzero. **$** shows cumulative model usage cost next to usage in the last
group, and is omitted without pricing. Input/output/cache/context usage remains
in the built-in footer above. Counts use at most four
columns with `k`, `M` and larger units. Unusually large timing, speed or cost
values use scientific notation to fit. Costs use three decimal places.

Normal fields use the theme's dim color. A nonzero **!** is red. **◴** (TTFT) uses
the warning color at 10 seconds and the error color at 30 seconds; these are
waiting-time hints, not proof that the provider failed. Before the first token,
TTFT shows elapsed waiting time and refreshes each second. After the first
token it stays fixed, so a long streaming response does not trigger a wait
warning. Request duration and token speed are not treated as errors.

The USD total comes from each response's `usage.cost.total`. The host calculates
this from token usage and model rates, including input, output, cache read/write
and any applicable pricing tiers. With a host that supports custom model pricing,
add a `cost` block to the model entry in `~/.rpi/agent/models.json` (or
`$RPI_CODING_AGENT_DIR/models.json`):

```json
{
  "id": "your-model-id",
  "cost": {
    "input": 2,
    "output": 8,
    "cacheRead": 0.2,
    "cacheWrite": 2.5
  }
}
```

These example rates are **USD per million tokens**. USD shows accumulated usage
cost, rather than the model's unit price. Restart rpi after changing the config.
USD is shown only for models with known pricing (built-in rates or an explicit
`cost` block). Explicit zero rates show `$0.000`; missing pricing hides the entire
USD field, including when switching from a priced model to an unpriced model.
Accumulated costs are retained and shown again when using a priced model.
Older hosts without pricing metadata only show USD after a positive reported cost.

Streaming timing updates refresh once per second; completed requests and mode
changes update immediately. Unchanged panel content is not republished, and
show/hide actions are sent only when visibility changes.

The plugin uses the host's registered `UiComponent` text interface;
the host does not interpret monitor metrics or know this plugin's identity.
It requires a host with `UiComponent` action 22 support. Older hosts do not display the
panel. Headless runs collect data without displaying a panel. Counts start on
load and reset at session start, rather than replaying history.
Colors require the optional `highlights` field on text updates, containing
literal `{ "text": "...", "color": "warning" | "error" }` entries. The host
applies its theme colors after sanitizing text and clips the styled row to the
terminal width. Hosts without highlights fall back to the compact plain row.

Build: `cargo build --release -p rpi-run-stats`; install the platform cdylib in `~/.rpi/agent/extensions` and restart rpi.

The footer position is registered once. Later messages update text only; F8 uses
show/hide and preserves the registration. Host unload cleans up the component.

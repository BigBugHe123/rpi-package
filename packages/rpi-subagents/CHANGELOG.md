# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Run `delegate_task` through a bounded child `rpi --mode rpc` process with
  streaming progress, timeout, cancellation, turn limits, and recursion guards.
- Bound child RPC lines, total streamed/final output, and partial event count to
  prevent unbounded memory use.
- Use ephemeral child sessions, the harness turn budget, inherited permission
  policy, and process-tree cleanup for reliable cancellation and timeout handling.

## [0.1.5](https://github.com/pi-rust/rpi-package/compare/rpi-subagents-v0.1.4...rpi-subagents-v0.1.5) - 2026-10-14

### Added

- *(webui)* integrate Open WebUI and publish versioned extensions ([#4](https://github.com/pi-rust/rpi-package/pull/4))

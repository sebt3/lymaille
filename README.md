# lymaille

Library to run an LLM as a durable work agent: persistent sessions, a Rhai scratchpad, lazily discovered toolsets.

## Feature flags

- `rig` (on by default): OpenAI-compatible inference adapter over `rig-core`.
- `mcp-http` (on by default): real MCP connector over streamable HTTP. The host installs its own rustls provider.
- `fs` and `shell` (off by default): relays of the `vynil-core` feature groups the scratchpad may grant.

Requires Rust 1.95.0 or later, edition 2024.

Licensed under [BSD-3-Clause](LICENSE).

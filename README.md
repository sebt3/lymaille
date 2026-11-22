# lymaille

Library to run an LLM as a durable work agent: persistent sessions, a Rhai scratchpad, lazily discovered toolsets.

## Two layers

Raw capabilities (`Capabilities`) are the `vynil-core` modules compiled by the Cargo features listed below, plus the
host's own modules and MCP modules. They are never shown to the productive LLM: they only exist as dependencies of
toolsets. The visible face of a capability is a presentation toolset, collected into a `Presentation`. The session's
`ToolsetContext` lists the toolsets of the session (`select_toolsets`) and is the only place where access rights
live: an agent definition carries no module list, and a subagent can only restrict the context's list.

## Feature flags

- `rig` (on by default): OpenAI-compatible inference adapter over `rig-core`.
- `mcp-http` (on by default): real MCP connector over streamable HTTP. The host installs its own rustls provider.
- `embedder` (off by default): text embedding port (`Embedder`, its `rig` client, a deterministic test embedder and an
  adapter that makes an `Embedder` usable as the model of a `rig` vector index) and semantic search of Memory (`search`
  function of each Memory module) and of skills (`query` argument of the `skill` tool). The host builds and maintains the
  indexes; the library stores no vector and never reindexes by itself.
- `ts-rs` (off by default): TypeScript derivation of the definition types. The library writes no TypeScript file.
- `fs`, `shell`, `password`, `quantity`, `crypto`, `http`, `oci`, `feeds`, `prometheus`, `loki` (all off by
  default): relays of the `vynil-core` features of the same name, compiled into `Capabilities`.

Requires Rust 1.95.0 or later, edition 2024.

Licensed under [BSD-3-Clause](LICENSE).

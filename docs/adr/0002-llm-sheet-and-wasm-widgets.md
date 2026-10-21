# ADR-002: LLM agent, shared GridCraft sheet, WebAssembly widgets

- Status: accepted (2026-10-21)
- Context: the demo needs a real model with tools, a spreadsheet the user, the agent and the
  server all manipulate live, and agent-written code that keeps the sheet updated.

## Decisions

**LLM: OpenAI-compatible chat completions, streamed (SSE), via reqwest (rustls, webpki roots).**
Melious serves `deepseek-v4-flash-0731` and `qwen3.5-397b-a17b` behind this API, and so do OVH AI
Endpoints and vLLM, so one client covers them. We tested both models live:

- Both stream `reasoning_content`, which the UI shows as thinking blocks.
- Qwen streams tool arguments in pieces, so calls are accumulated by `index`.
- DeepSeek gets its reasoning back within a tool round.
- The default `max_tokens` is 16k, because a 4k budget ran out in DeepSeek's reasoning before
  the tool calls.

A turn is a run of the `chat` scenario: it uses the same run log, replay, cancel and UI as
everything else. History is kept per `conversation_id` in memory (200 conversations, LRU) and
only at consistent points, never with unanswered tool calls.

**Sheet: a server-side op log of GridCraft engine commands, replayed by every client.**

- The server runs `gridcraft_mcp::Headless`, the engine the agent's tools use, and logs what its
  session journals.
- Browsers run GridCraft's egui UI as WebAssembly (`wasm/sheet-web`) in a same-origin iframe.
  They apply remote ops through the control channel (`engine.execute`) and report their own
  journal.

Alternatives we rejected:

- Shipping workbook snapshots (xlsx) on every change: too large at one change per second.
- An OT/CRDT layer: GridCraft has no merge semantics, and last-writer-wins is fine for a demo.

Compaction drops `cell.set`s that a later `cell.set` of the same cell overwrites, with only
`cell.set`s between them, and keeps `seq`s. A `log_id` (boot time plus epoch) tells a
reconnecting tab that it must reload. The same deny list (files, network imports, UI commands)
applies to the agent and to browsers, because both run commands on the server's engine.

**Widgets: WAT compiled with `wat`, run by `wasmi` (interpreter).**

- Pure Rust and small: it builds for musl in a FROM-scratch image.
- Fuel metering bounds every call, so an infinite loop fails the call instead of hanging the
  server.
- A JIT (wasmtime) is unnecessary for a few hundred instructions per second.

WAT rather than a compiled language keeps "compile" inside the server: no toolchain, and
errors come back to the model in a few milliseconds. The ABI is a handful of imports
(`tools.<metric>`, `sheet.set_number/set_text`, `env.log`); the metric list is
`machine::METRICS`, the one table that drives both the tool and the imports.

**Container: FROM scratch, built from a staged context.**

- The UI and the sheet's wasm are built on the host, so the image needs neither Node nor a wasm
  toolchain.
- GridCraft is a path dependency, so `_stage-ctx` rsyncs both trees into `.build-ctx`.

## Consequences

- The sheet's wasm (~16 MB) is embedded in the binary and served uncompressed; fine on localhost.
- Selection commands sync too, so tabs share the cursor.
- Conversations and runs are lost on restart; settings are not.

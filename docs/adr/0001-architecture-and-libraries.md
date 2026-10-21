# ADR-001: Architecture and libraries of agent-demo

- Status: accepted (scaffold, 2026-10-21)
- Context: a GUI for demonstrating agentic features (streamed reasoning and text, plans, tool
  calls, human approval, sub-agents, usage), on a Rust + React stack.

## Decision

```
┌─────────────────────────────── agent-demo (one binary) ────────────────────────────────┐
│ Agent (trait) ── ScriptedAgent today, an LLM-backed agent later                        │
│     │ RunHandle::emit / request_approval / sleep (cancellable)                         │
│ Runs: per run an append-only Vec<RunEvent> + watch counter; summaries via broadcast    │
│     │                                                                                  │
│ axum 0.8 router                                                                        │
│  ├─ /                         proa-rendered shell (header, SSR fallback of the island) │
│  ├─ /static/*                 React islands + self-hosted React, embedded (include_dir)│
│  └─ /agentdemo.v1.*           connectrpc, binary protobuf; WatchRun / WatchRuns stream │
└────────────────────────────────────────────────────────────────────────────────────────┘
browser: proa bootstrap mounts AgentConsole ─ connect-web (binary) ─▶ WatchRun(from_seq)
```

- **Event log, not fire-and-forget.** Every run is an append-only log with gapless `seq`.
  `WatchRun(run_id, from_seq)` replays from any point and then follows live, so reconnects,
  reloads and extra tabs see exactly the same run. Watchers read the log by cursor when a `watch`
  counter moves, so a slow client never loses events and never slows the agent (a broadcast
  channel would drop what a lagging receiver missed).
- **Human in the loop** is a `oneshot` per waiting tool call (`DecideToolCall`), raced against
  the run's `CancellationToken`.
- **The UI is a pure reducer** (`web/src/agent/state.ts`) over `RunEvent`s; events with
  `seq < nextSeq` are ignored, which makes replay idempotent.
- **Sub-agents** share the run; their events carry `agent_id`, and the UI nests them.

## Libraries

Rust 1.99.1, `axum` 0.8, `connectrpc` / `buffa` / `connectrpc-build` 0.9 (needs `protoc`),
`proa_core` / `proa_macros` / `proa_react` 0.3.0, `include_dir` 0.7, `tokio`, `tracing`; React 19.3,
Vite 7, TypeScript 5.9, `@connectrpc/connect(-web)` 2.2.0, `@bufbuild/protobuf` 2.16.0, ESLint 9
with complexity limits, Vitest 5, pnpm. The design system lives in `web/public/design` and
`web/src/design` (see docs/design-system.md).

Also: `tokio-util` 0.7 (`CancellationToken` for cancelling runs), `serde_json` (tool inputs and
outputs in the scripted agent).

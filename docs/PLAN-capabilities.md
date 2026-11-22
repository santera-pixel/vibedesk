# Plan: capability layer

Status: built. Written 2026-11-17, last verified against `cargo test --lib`
(667 passed) and `npx tsc --noEmit` (clean).

Six upstream repositories become first-class VibeDesk capabilities. This
document is the contract: what each one owns, where it attaches, what it
deliberately does not do, and what closing each gap would take.

## The problem this solves

VibeDesk is an ACP client. The agent owns its own context, so the only
compaction used to be the CLI's `/compact`, reached through `src/lib/commands.ts`
and described to the user in `InfoPanels.tsx`. VibeDesk could not see the
summary, could not choose the strategy, and could not show what was dropped.

Three boundaries belong to VibeDesk, and each is a place where context is
created:

| Boundary | Where | Who runs it |
| --- | --- | --- |
| `terminal/output` | `src-tauri/src/acp/terminal.rs` | VibeDesk runs the shell |
| `fs/read_text_file` | `src-tauri/src/acp/bridge.rs` | VibeDesk reads the file |
| model request | `src-tauri/src/capabilities/proxy/` | VibeDesk is the endpoint |

The third becomes ours through a provider whose `api_base` points at a loopback
proxy — `ProviderConfig` in the Vibe config schema carries `api_base`,
`api_style`, `backend` and `preserve_api_base`, so this is a supported
configuration, not a hack.

### Correction: the first two are not reachable

`client_capabilities()` in `acp/protocol.rs` advertises `fs.readTextFile`,
`fs.writeTextFile` and `terminal: true`, and its doc comment claimed Vibe routes
its file and shell tools through the client when the client claims them. **It
does not.** With `VIBEDESK_TRACE_ACP=1`, a real turn that read a file and ran a
command produced zero `acp: agent → client:` lines. The agent used its own
`read_file` and `bash` tools and never sent a single `fs/*` or `terminal/*`
request.

So Reading and Terminal were wired to boundaries the agent never crosses. They
reported zero calls, and the panel's zero was honest about a capability that
could not run.

The fix is not a better ACP handler. It is the boundary that sees every token
unconditionally: the proxy. `capabilities/proxy/tools.rs` rewrites tool results
inside the model request, keyed by the tool name the agent actually sends
(`read_file`, `bash`), and the ACP handlers stay as a fallback for an agent that
does call them. The proxy therefore starts whenever **any** of compaction,
reading or terminal is on — `proxy::needed()` — rather than only for compaction.

Two consequences worth recording:

- A card that names `fs/read_text_file` is a dead end for an agent that only has
  `read_file`. `sparse::ReadTool` carries the tool name and its offset base, so
  the card names the agent's own tool with the agent's own numbering (zero-based
  `offset` for `read_file`, one-based `line` for ACP).
- A card that offers only "read the whole file" is not a map. The agent took the
  one road it was given, or went around it with `sed -n`. The card now lists each
  section with its line range and a call per section, and a whole-file dump
  through the shell (`cat FILE`, `type FILE`, `Get-Content FILE`) is carded by
  the same gate, so the detour costs a round trip and gains nothing.

## Capability ledger

| Repo | Capability | Attaches at | Panel |
| --- | --- | --- | --- |
| billion-context (`acp-kernel`) | context compression, three tiers | model request, via proxy | yes — compaction |
| tokenjuice | terminal output compaction, 130 rules in 70 families | model request, on `bash` results | yes — terminal |
| SparseReading | benefit gate, file card, sparse read | model request, on `read_file` and `cat` results | yes — reading |
| dream-memory | reflective memory, 4-phase consolidation | transcript + proxy system prompt | yes — memory |
| codebase-memory-mcp | code knowledge graph, MCP tools | MCP registration | yes — code graph |
| donsetch | web search and page read, MCP tools | MCP registration | yes — web |

Every capability has a panel, and no two share one. That is a property the
registry enforces in a test rather than a convention: a capability with no
surface would be a row the user can enable and never inspect.

## Why the proxy, and why it is opt-in

Compaction is the one capability that cannot attach to a boundary VibeDesk
already owns. The agent builds its request internally. The only supported way to
see that request is to be the endpoint it is sent to.

So VibeDesk runs a loopback HTTP server and adds a provider:

```toml
[[providers]]
name = "vibedesk"
api_base = "http://127.0.0.1:<port>/v1"
api_style = "openai"
backend = "generic"
preserve_api_base = true
```

`preserve_api_base` is not optional. Without it the CLI adopts the
whoami-advertised `api_base` on sign-in or `/reload` and silently takes the proxy
off the path — compaction would appear enabled and do nothing.

The mirrored models set `auto_compact_threshold = max_context_length`. That is
what actually *replaces* the CLI's own summariser rather than racing it: the CLI
never reaches its threshold, so the kernel is the only thing that compresses.

The proxy is additive: VibeDesk adds its own provider and its own models, never
edits the user's. Disabling removes exactly what it added. The user's existing
models keep working untouched, which is also what keeps this mergeable with
concurrent work on the same tree.

Compaction defaults **off**. It changes the model request, which is a bigger
promise to make on the user's behalf than rewriting terminal output.

### Relay rules

Faithful to upstream's `docs/unified-loop-spec.md`:

- `MAX_ROUNDS = 10`. A relay that has not converged by then is not converging.
- A proxy tool call is **never forwarded** to the client. The client has no such
  tool, and inventing one would be a lie the agent acts on.
- A round that produced a **client** tool call ends the relay and the proxy's
  result is dropped. Client work outranks proxy work.
- The philosophy prompt is **transient**: rebuilt per request, never entering the
  conversation.

## Module layout

```
src-tauri/src/capabilities/
  mod.rs         registry, ids, boundaries, stats, events
  settings.rs    persisted enable/disable and per-capability config
  tokenjuice/    rule engine, 130 embedded rules, 136 upstream fixtures
  sparse/        benefit gate, file card, denoise, line windows
  compact/       compression kernel: blocks, refs, prune, nudge, CCR
  proxy/         loopback server, SSE relay, session table
  provider.rs    the `vibedesk` provider and mirrored models in config.toml
  memory/        dream store, consolidation, recall
  external.rs    codebase-memory-mcp and donsetch: detect, register
src-tauri/src/commands/capabilities.rs   the fourteen IPC commands
```

Frontend:

```
src/lib/capabilities.ts                     types + pure helpers
src/lib/bridge.ts                           the `capabilities` namespace
src/components/panels/CapabilityPanels.tsx  seven drawers (six capabilities + ledger)
```

The namespace lives in `bridge.ts` with every other command family so there is
still exactly one module that owns `invoke`. `capabilities.ts` holds only types
and helpers, and it owns `CapabilityPanelId` because the Rust registry is what
decides which capabilities have a panel at all.

## The panels

Seven drawers, and the reason each one exists:

| Drawer | What only it can show |
| --- | --- |
| `/capabilities` | all six together: boundary, enabled, bytes saved, blocked reason |
| `/compaction` | proxy port, upstream, registration, per-conversation kernel state, the tier structure, and the two gates that decide which tier fires |
| `/terminal` | the 70 rule families, which are switched off, what fired |
| `/reading` | a live gate probe on any path, and the card the agent would receive |
| `/memory` | the store, the gate, every memory, and a manual consolidation pass |
| `/code-graph` | whether the binary is found, its version, its `config.toml` entry |
| `/web` | the same for donsetch |

The reading probe is what makes that drawer more than a counter: the user points
it at a file and sees the decision the gate would make and the exact
`line`/`limit` calls the agent would be handed, before a session touches it.

The compaction drawer shows the tier structure per conversation (`T1 ×2`,
`T2 ×1`) rather than a single block count, because one count cannot tell a flat
transcript from a distilled one. It also exposes both gates — `compress at` and
`distil at` — since a distillation gate above the compression gate would make
tiers 2 and 3 unreachable, and a setting the user cannot see is a setting that
silently disables a feature.

## The tier ladder

All three of billion-context's tiers are built, because a port that stops at T1
does not replace the CLI's summariser — it just adds a second, shallower one.

| Tier | Folds | Ref form | Gate |
| --- | --- | --- | --- |
| T1 compress | raw messages into one summary block | `m00012–m00040` | `min_compress_range` |
| T2 distil | several summary blocks into one | `b3–b7` | `min_distill_range` |
| T3 condense | distilled blocks again | `b3–b7` | `min_distill_range` |

A new block's tier is `max(children) + 1`, capped at 3, so the ladder cannot run
away. The nudge escalates rather than always asking for the same thing: while
pending tokens are below `min_compress_range` it asks for a distillation over a
block range, which is the cheaper ask and the only way a long session reclaims
context instead of accumulating summary lines. Both gates are user settings, and
the panel shows the resulting structure.

A distillation is lossy in one direction only: the children are deactivated, not
deleted, so `decompress` restores them and the raw messages behind them are
still reachable through the CCR store.

## What is not built, and why

- **SparseReading's `sro_read` hint protocol as an agent tool.** The hint →
  EvidencePack loop needs a tool the agent can call. ACP gives the client no way
  to add tools; that requires an MCP server. What ships instead is the part that
  works over the existing protocol: the benefit gate decides, the file card
  replaces the dump, and the agent pages with the `line`/`limit` fields
  `fs/read_text_file` already accepts. The tool surface is the next step, not a
  stub of this one.
- **SparseReading's PDF, XLSX and YAML previews.** Upstream parses these with
  Python libraries. The port detects them and reports `Structure::Unsupported`
  with the reason, which the card renders and the panel shows. It does not fake a
  preview it cannot build. Closing it means a parser per format in Rust.
- **codebase-memory-mcp and donsetch internals.** Both are native binaries with
  their own MCP servers. VibeDesk detects, registers, and reports them. Porting
  1077 C files or a Rust web stack into the app would be a second product.
- **Guessing a provider's base URL.** Only `mistral` →
  `https://api.mistral.ai/v1` is hardcoded, because that is the one the CLI
  documents. Any other provider with no `[[providers]]` entry is refused with a
  message telling the user to add one. Guessing would send a key to an unknown
  endpoint.
- **Memory consolidation through the proxy.** It calls the provider directly. A
  consolidation pass is not a conversation and must not be compacted; routing it
  through the kernel would compress the very transcripts it is trying to read.
  The key comes from the env var the provider names, falling back to
  `~/.vibe/.env`, and is never logged or returned.

## Verification

- `cargo test --lib` — 679 passed, 0 failed. 383 were the pre-existing baseline;
  296 cover this layer, including all 136 tokenjuice fixtures replayed against
  the port.
- `npx tsc --noEmit` — clean.
- `npx vite build` — succeeds.
- `npm run test:unit` — 56 passed.
- `node scripts/ui-probe.mjs` — `selftest` 8/8, `expert` 33/33, `novice` 7/7,
  `adversarial` 13/13, `game` 9/9, 0 console errors. 70/70 in one `all` run.

Verified end to end, against a live window. The proxy has carried real requests
from the real UI through the real kernel to a fake OpenAI-compatible upstream,
and the panel was read back from screenshots of the running app. That run found
four defects no unit test had caught, all now fixed and covered:

1. **The transient system prompt invalidated every ref.** The nudge was appended
   to the first system message after refs were assigned, and `base_id` hashes
   message text, so the system message's identity changed and every ref the
   nudge named stopped resolving. Every compression was rejected with "ref
   m00001 does not exist in this session". The prompt is now applied to a copy
   at encode time and never touches the kernel's array.
2. **The usage bar reported the compressed total as the live size.** A session
   showed `usage: 2.2` — 220% of the window — because the panel was handed a
   running total of everything ever folded. It now gets the conversation's size
   after the last turn.
3. **Tiers 2 and 3 were unreachable.** After escalating to a distillation the
   nudge still measured `pending_tokens` against `min_compress_range`. Summaries
   are small by construction, so a distillation could never clear it, and the
   nudge held forever with "nothing large enough left to fold". The gate is now
   the one belonging to the thing being asked for.
4. **Panel settings did not reach an open conversation.** The proxy keys sessions
   by conversation and returned the existing kernel with the config it was born
   with, so `compress at` and `distil at` were decoration until the app
   restarted. The kernel's config is now refreshed on every request.

### The second run: compaction was never on the path

The first run proved the kernel worked when a request reached it. It did not
prove a request ever did. A second run against a real agent, a real model and a
real keyring found that the whole capability was inert — registered, listening,
and bypassed by every request. Seven defects, in the order they were hiding each
other:

5. **`active_model` was written without the prefix.** `enable` chose the alias
   from the mirrored list and then returned it unprefixed, so `active_model`
   stayed on the direct model. The proxy was registered, listening, and never
   used. This is the failure mode `AGENTS.md` names: the panel read "on" and
   nothing was intercepted. `mirrored_active` now applies the prefix, once,
   where it is tested.
6. **A re-enable wrapped the proxy in itself.** `active_provider` ran before
   `strip`, so on the second launch — which happens on every start while
   compaction is on — `vibedesk/deepseek` resolved to the `vibedesk` provider and
   the mirror was mirrored. The alias is normalised before the lookup.
7. **The mirrored model was indistinguishable in the picker.** The agent lists
   models by `display_name`, and the copy was verbatim, so the user saw two
   identical rows and no way to pick the routed one. The mirrored model now
   carries ` · via VibeDesk`.
8. **The mirrored provider named no credential.** `api_key_env_var` was copied
   from the upstream entry, which is empty when the key lives in the OS keyring.
   The client therefore sent no `Authorization` header, and the proxy — which
   deliberately holds no key — forwarded none. Every request came back
   `Invalid API Key`. The CLI's own convention fills the gap:
   `<PROVIDER>_API_KEY`, and the backend is the provider's name.
9. **The mirrored model name went upstream.** The client asks for
   `vibedesk/<name>`; upstream has never heard of the prefix and answered
   `Invalid model`. It is stripped on the way out.
10. **A raw JSON body was written into an SSE stream.** When upstream failed, the
    proxy forwarded its body verbatim, so the client reported a malformed chunk
    and the real cause — the status and the body — was invisible. A streamed
    answer now stays a stream: the failure becomes an `error` event.
11. **Enabling compaction did not route the running session.** The agent reads
    `config.toml` once, at launch, so a session that started before the switch
    kept talking to the provider directly. The switch now sends `/reload` and
    re-points the session's model, which keeps the conversation; a session
    restart would have dropped it. The agent does not announce the new model
    list, so the picker is updated from the registration.

The panel was also lying by omission: it labelled the *config's* model `active`,
which reads as the session's. It now shows `config` and `session` separately, and
says `direct — … (reload to route)` when the two disagree.

### What the harness had to learn

Three checks were wrong in ways that hid the above:

- The rail-drag check started its drag from a point that was stable but covered
  by the drawer's transition overlay, so the pointer-down was swallowed and the
  rail did not move. Drags now start from a hit-tested point (`waitDragStart`).
- The palette check asserted every visible row contained the typed text. The
  filter also matches descriptions, so "high-**comp**lexity" and "**comp**eting
  constraints" are correct matches. It now asserts the list narrows and the
  command being typed survives.
- The tool-call check grepped the page for words like `make`, which matched the
  README's prose and the Makefile's name. It now counts `[data-vd-tool]` cards
  and requires one to have left `pending`.

Three more surfaced when the transcript started folding tool calls into one bar,
and none of them was a product defect:

- The bar check clicked the first bar in the document. The transcript virtualises
  off-screen rows, so that bar sat 4000px above the viewport and never became
  clickable. It now picks the bar nearest the middle of the window, scrolls it
  there, and addresses it by index so the click and the card count are about the
  same bar.
- The drag checks assumed the rail was open. The collapse is persisted to
  `layout.json`, so on a machine where the rail had been closed the handle
  measured 0×0 inside a `display: none` aside and read as "no resize handle".
  `ensureRailVisible` puts it back first.
- Two drawer checks clicked fields below the fold. The compaction panel gained
  rows in this phase, which pushed `compress at` past the bottom of the window;
  the click landed on nothing and the check reported that the setting did not
  round-trip. `scrollTo` brings the target into view first.

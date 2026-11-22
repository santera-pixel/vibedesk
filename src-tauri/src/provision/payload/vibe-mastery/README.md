# vibe-mastery

Skill bundles on demand for Mistral Vibe. A port of [dsh-mastery](https://github.com/santera-pixel/dsh-mastery) from the DeepSeek harness.

Four bundles ship by default: **3Dviz**, **Anti-Slop**, **UI/UX**, and **Assay**. Each is a directory of `SKILL.md` files — usually somebody else's clone. Nothing is preloaded. You ask for a skill, the body arrives as a tool result, and the plugin counts that it happened.

## Install

The plugin lives at `~/.vibe/plugins/vibe-mastery/`. Vibe discovers it on the next session or after `/reload`.

Clone the bundle sources once:

```
mcp_mastery.update {action: "pull"}
```

That clones into `~/.vibe/mastery/sources/` and fast-forwards anything already there. Nothing else needs installing: the server is plain CommonJS on Node built-ins, with no `package.json` and no build step.

## Tools

| Tool | What it does |
|---|---|
| `mcp_mastery.bundles` | Every bundle: on or off, skill count, source status, which skills have been loaded. `{verbose: true}` adds descriptions and lists all skills. |
| `mcp_mastery.find` | Search skill names and descriptions across bundles. `{query, bundle?}` |
| `mcp_mastery.load` | Return one skill's full body. `{bundle, skill}` |
| `mcp_mastery.toggle` | Switch a bundle for this conversation. `{bundle, on}` |
| `mcp_mastery.update` | `{action: "check"}` fetches and reports; `{action: "pull"}` clones or fast-forwards. `{bundle?}` limits to one. |

A `session_start` hook injects a short catalog — which bundles exist, which are on, which tool does what — so the model does not have to find five tools among forty and guess.

## What did not port, and why

The dsh plugin had two things Vibe cannot express. Both are stated here rather than quietly dropped.

**Composer chips.** dsh drew a row of toggle chips above the input box. Vibe is a terminal UI and plugins have no GUI surface at all — no widgets, no panels, no input decorations. There is nothing to draw into. The substitute is the session-start catalog plus `mcp_mastery.toggle`, which is the same information and the same control, reached by asking instead of clicking.

**Per-chat dynamic skill registration.** dsh could register a bundle's skills with the harness mid-conversation, so they appeared in the skill catalog the moment you enabled the bundle. Vibe's skill catalog is fixed at session start and only rebuilt on `/reload`; `enabled_skills` and `disabled_skills` are config-level, not runtime. A plugin cannot add a skill to a running session.

The substitute is `mcp_mastery.load`: it returns the `SKILL.md` body as a tool result. The model ends up with the same text in context, which is what a registered skill would have given it. What is lost is *discovery* — a registered skill announces itself in the catalog, whereas a bundle skill has to be found with `mcp_mastery.bundles` or `mcp_mastery.find`. The session-start hook exists to close that gap.

## State

Two files, deliberately in different places.

**`~/.vibe/mastery/config.json`** — durable. Which bundles exist, where their roots are, whether they are enabled, and the update policy. Shared by every conversation, because "which clones do I read" is a property of the user, not of a chat. Written on first run so there is something to edit rather than a shape to guess.

**`$PLUGIN_DATA/state.json`** — per conversation. Vibe gives every session its own `PLUGIN_DATA` directory, so this file is already scoped to one chat and nothing in it is keyed by session id. It holds only what you did in this conversation: which bundles you switched off, and how many times each skill body was actually loaded.

That split is why `toggle` is session-scoped and the config is not. Turning a bundle off in one conversation does not follow you into the next.

Environment overrides: `VIBE_MASTERY_HOME` moves the whole mastery directory, `VIBE_MASTERY_SOURCES` moves just the clones.

## Adding a bundle

Edit `~/.vibe/mastery/config.json`:

```json
{
  "id": "mine",
  "label": "Mine",
  "blurb": "What it is for",
  "repo": "https://github.com/you/your-skills",
  "root": "C:/Users/you/.vibe/mastery/sources/your-skills",
  "enabled": true
}
```

`root` is the only required field. `repo` enables `update`. `authored: true` means the skills ship inside this plugin and there is no upstream to be behind.

Defaults are appended for ids the file has never seen. A bundle you delete stays deleted — that is the difference between a default and an override.

## Git policy

`update` is fast-forward only, with `GIT_TERMINAL_PROMPT=0` so a missing credential fails instead of blocking a turn on a prompt nobody can see. These are somebody else's clones: a merge commit or a rebase would rewrite their history. A diverged branch is reported, not forced.

`behind` is `null` when there is no upstream to compare against, which is a different answer from `0`. "Up to date" and "I cannot tell" are not the same claim.

## Layout

```
plugin.json                  Agent Plugins 1.0 manifest
mcp.json                     stdio server registration
ai.mistral.vibe/hooks.toml   session_start catalog hook
hooks/session-start.js       builds the catalog text
server/index.js              JSON-RPC 2.0 over stdio
server/config.js             bundle definitions, config read/write
server/bundles.js            SKILL.md discovery and frontmatter parsing
server/git.js                status, clone, fetch, pull
server/state.js              per-session switches and load counts
skills/assay/SKILL.md        the one authored skill
```

`mcp.json` must set `"cwd": "./"` — Vibe rejects a stdio server whose cwd does not start with `./`, `${PLUGIN_ROOT}`, or `${PLUGIN_DATA}`, and the failure is a warning rather than an error, so the server silently never starts.

## Notes

- Loading a large skill costs context. `antislop` is about 54 KB. That is the skill's real size; the plugin does not slice it, because a partial skill is a skill you did not read.
- Bodies are capped at 256 KiB and files at 512 KiB, with discovery bounded to depth 6 and 8000 entries. A pathological clone cannot hang a turn.
- `assay` is both a bundle and a registered skill (`vibe-mastery:assay`), pointing at the same file. It is the one skill worth having in the catalog without being asked for.

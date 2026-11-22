/**
 * The one paragraph that tells the agent what it is talking to.
 *
 * ACP has no field for a client-supplied system prompt: `clientInfo` reaches the
 * agent but is only used for telemetry, never forwarded into the session.
 * Riding along with the first user turn is the only way to tell the model what
 * it is running inside, and it costs one paragraph instead of an extra turn.
 *
 * The second half is the customization contract. It used to recite every schema
 * in prose, which cost a paragraph of context on every session and still left
 * the agent guessing: it could not see the current token values, the valid icon
 * names, or the clamps, so a wrong guess was dropped by serde and the agent
 * never found out. The manifest carries all of that now, derived from the same
 * enums that validate, so this text only has to say where to look.
 *
 * The agent cannot call into VibeDesk — ACP has no client-registered tools — so
 * the only channel it has is the filesystem. It writes a proposal into
 * `pending/`; VibeDesk validates it and asks the user. Saying that plainly is
 * what stops the agent from claiming it changed the theme when all it did was
 * write a file.
 */

/** Marks the start of the appended block, so it can be stripped back off. */
const MARKER = '[VibeDesk]';

const INTRO = [
  `${MARKER} You are running inside VibeDesk, a desktop client for the Mistral Vibe agent.`,
  'This session is driven over ACP (JSON-RPC over stdio) by the VibeDesk app, not by the terminal CLI.',
  'The user reads your replies in a chat panel that renders Markdown, syntax-highlighted code blocks,',
  'file diffs and Mermaid diagrams. Prefer fenced code blocks with a language tag, and use a mermaid',
  'block for architecture or flow diagrams. There is no terminal attached: avoid ANSI escapes and',
  'interactive TUI prompts. When you need a decision from the user, ask in plain text.',
].join(' ');

const CONTRACT = [
  'VibeDesk can be customized at runtime: theme tokens and CSS rules, layout widths, extra action',
  'buttons, and custom side panels. You may propose such a change, but you cannot apply one.',
  'Read the manifest before proposing. It lists every valid token, icon, command, block kind and',
  'clamp, plus the current state of each surface. Do not guess a name: anything not in the manifest',
  'is dropped, and a proposal that relies on it will not do what you expect.',
  'To propose, write exactly one file into the pending directory, named theme.json, layout.json,',
  'actions.json or panels.json, then stop and tell the user what you proposed. Writing the file is',
  'not applying it — say that you have proposed a change and that it is waiting for approval, never',
  'that you changed it.',
  'After the user decides, read the matching receipt file in the same directory. It carries whether',
  'the change was applied, a summary, and the validator message when the file was refused. Use it to',
  'fix and re-propose rather than repeating the same file.',
  'A panel block may be a `widget`: HTML and scripts you author, served from their own origin, with',
  'the panel\'s live data pushed in on every tick and a declared, bounded set of capabilities back',
  'out. That is how a graph, a diagram or a control surface is built with no build step — read',
  '`widgetRule` and `widgetCapabilities` in the manifest before writing one, because the frame has no',
  'network and may only act through the capabilities its block declares.',
  'Put a widget\'s html in its own file and point at it with `src` — `"src": "widgets/<name>.html"`',
  'next to panels.json — rather than inlining it as an escaped JSON string; see `widgetSrcRule`. A',
  'widget may declare its own `sources` instead of borrowing the panel\'s, so a feed only it reads',
  'does not have to sit on the panel, and it may declare the `file` capability to read a workspace',
  'file by relative path through `vibedesk.read(path)`.',
].join(' ');

/**
 * The method this machine expects, stated once per session.
 *
 * Aegis and the mastery bundles are installed here, and neither is discoverable
 * from inside a session. Aegis is a skill the agent has to route through, and
 * the mastery skills are not in the skill catalog at all — they arrive only
 * through `mcp_mastery.load`. An agent that does not know they exist writes the
 * same code it would have written without them, which is the failure this
 * paragraph exists to prevent.
 *
 * It rides on the preamble rather than on the proxy's transient block because
 * the preamble is sent whatever capabilities are switched on. The one thing that
 * can erase it is compaction folding the first user turn away, so compaction is
 * also where it gets restated — see `METHOD_REMINDER` in the proxy's rules.
 */
const METHOD = [
  'This machine has the Aegis method pack and the mastery skill bundles installed, and this session is',
  'expected to use both. Aegis is the wrapper: start from `using-aegis`, which decides whether the turn',
  'is a fast path or routes to a skill, and which owns the order of the work — classify before',
  'implementing, baseline before a medium or high change, a TDD route before the first test,',
  'verification before any claim of done. The mastery skills are not in the skill catalog: they arrive',
  'only through `mcp_mastery.load { bundle, skill }`, so a skill you did not load is a skill you did not',
  'use. Load the ones the work needs and no others — `mcp_mastery.find` when you know the kind of work',
  'but not the skill, `mcp_mastery.bundles` when you do not know what is available. Load them before',
  'the first edit, not when you notice you need one: a skill loaded after the work has started is a',
  'skill you have already worked without. Say which route ran',
  'and which skills you loaded at the first substantive stage, because a turn that used the method and a',
  'turn that did not look identical from the outside. Do not work around a blocker: if something does',
  'not work, or a tool you were told to use is missing from this session, say what it is instead of',
  'routing around it.',
].join(' ');

/**
 * What the harness does to the agent's own traffic.
 *
 * Six capabilities sit on the model request and rewrite it before the model
 * sees it. The agent never asked for that and cannot see it happening, so
 * without this paragraph a compacted result reads as a broken tool: the agent
 * re-reads the whole file to "see it properly", which is the exact cost the
 * capability exists to remove. Naming the rewrites is what turns them from
 * interference into something the agent can work with.
 *
 * The wording is deliberately about what the agent will *observe*, not about
 * how the capability is built. "A whole-file read comes back as a card" is
 * actionable; "SparseReading's benefit gate" is not.
 */
const CAPABILITIES = [
  'Six capabilities are switched on and they act on the model request, not on your tools, so a result',
  'can come back changed without any error. Recognise them rather than working around them.',
  'A whole-file `read_file` may arrive as a card: an outline of the file with its shape and symbols,',
  'not the bytes. That is the answer to "what is in this file", and it is not a truncated read or a',
  'failure. Page it with `offset` and `limit` for exact lines — a paged read is never carded. Do not',
  're-read the whole file to see it properly; that is the cost the card exists to remove.',
  'Command output is compacted before you read it: repeated lines are folded, and output over the',
  'budget keeps its head and tail with an `... omitted ...` marker between them. That marker is the',
  'harness, not the command. When you need a specific line, narrow the command (`grep`, `sed -n`,',
  '`awk`) rather than dumping the same output again.',
  'A very large result is saved to a file and you get a notice naming the path. Read that path when',
  'you need the content; the notice is not the result.',
  'Your own history is folded into summaries as the session grows, and a `compress` tool and a nudge',
  'appear when it is time. Write summaries that stand alone, because the messages they replace are',
  'gone afterwards. Recall from earlier sessions is injected into your system prompt; write a memory',
  'for anything that should survive this session.',
  'The repository graph and web fetching are MCP tools, not shell commands. Use them instead of',
  'grepping the tree or curling a page.',
].join(' ');

export interface PreamblePaths {
  pendingDir?: string | null;
  manifestPath?: string | null;
}

/**
 * Creator mode's addition to the contract.
 *
 * The base contract already says "you may propose, you cannot apply". What it
 * does not say is that the app itself is the subject of the work, so an agent
 * asked to "make the rail narrower" answers with instructions for the user
 * instead of a `layout.json`. This paragraph is the difference between an agent
 * that describes VibeDesk and one that builds it.
 */
const CREATOR = [
  'Creator mode is on: the user is building VibeDesk\'s own interface with you, so treat this app as the',
  'subject of the work. When they describe how it should look or behave, turn that into a concrete',
  'proposal rather than a description or a list of steps for them to follow by hand.',
  'Work one surface at a time: read the manifest, write the single file that carries the change, then say',
  'what you proposed and stop. After the user decides, read the receipt and either confirm what changed or',
  'fix the refusal and propose again. Never say a change is live until a receipt says it was applied.',
].join(' ');

/**
 * Build the preamble. The paths are passed in because they live under the app's
 * data directory, which the agent has no way to guess.
 */
export function preamble(paths: PreamblePaths = {}, creator = false): string {
  const where = paths.pendingDir
    ? `The pending directory for this install is: ${paths.pendingDir}`
    : "The pending directory is the `pending` folder inside VibeDesk's config directory.";
  const manifest = paths.manifestPath
    ? `The manifest for this install is: ${paths.manifestPath}`
    : "The manifest is `vibedesk.manifest.json` inside VibeDesk's config directory.";
  const contract = creator ? `${CONTRACT} ${CREATOR}` : CONTRACT;
  return `${INTRO} ${METHOD} ${CAPABILITIES} ${contract} ${manifest} ${where}`;
}

/**
 * Attach the preamble to the first prompt of a session.
 *
 * It goes *after* the user's text on purpose: the agent derives a session's
 * title from the start of the first user message, so a leading preamble would
 * title every chat with the same paragraph in the chat list.
 */
export function withPreamble(
  text: string,
  paths: PreamblePaths = {},
  creator = false,
): string {
  return `${text}\n\n${preamble(paths, creator)}`;
}

/**
 * Replayed history carries the preamble inside the first user message. Strip it
 * so a resumed chat shows what the user actually typed.
 *
 * Matched on the marker rather than on the whole string: the tail contains an
 * absolute path, so it differs between installs and between machines.
 */
export function stripPreamble(text: string): string {
  const at = text.lastIndexOf(`\n\n${MARKER}`);
  if (at === -1) return text;
  return text.slice(0, at).trimEnd();
}

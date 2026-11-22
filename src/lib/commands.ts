/**
 * Slash-command catalogue.
 *
 * Two kinds of entries:
 *  - `agent`  — forwarded verbatim to `vibe-acp` as a prompt. The ACP command
 *               controller in Vibe 3.x implements exactly these.
 *  - `local`  — handled by VibeDesk itself (panels, session control, theme).
 *
 * Descriptions are taken from the Vibe CLI reference so the palette reads the
 * same as the terminal experience.
 */

export type CommandGroup = 'session' | 'context' | 'workspace' | 'config' | 'account' | 'help';

export interface SlashCommand {
  name: string;
  description: string;
  group: CommandGroup;
  transport: 'agent' | 'local';
  inputHint?: string;
  aliases?: string[];
  /** Local action id consumed by the composer. */
  action?: string;
  /** Hidden from the palette but still executable. */
  hidden?: boolean;
}

export const SLASH_COMMANDS: SlashCommand[] = [
  {
    name: 'config',
    description: 'Browse and edit every Vibe setting, with the layer that sets it',
    group: 'config',
    transport: 'local',
    action: 'open-config',
  },
  {
    name: 'plugins',
    description: 'Inspect installed plugins, their skills, MCP servers and hooks',
    group: 'config',
    transport: 'local',
    action: 'open-plugins',
  },
  {
    name: 'reload-plugins',
    description: 'Re-discover plugins from disk and re-pin the session snapshot',
    group: 'config',
    transport: 'local',
    action: 'reload-plugins',
  },
  {
    name: 'reload',
    description: 'Reload configuration, agent instructions and skills from disk',
    group: 'config',
    transport: 'agent',
  },
  {
    name: 'mcp',
    description: 'Show MCP server and connector status, or sign in to a server',
    group: 'config',
    transport: 'agent',
    inputHint: 'status | login <alias> | logout <alias>',
  },
  {
    name: 'proxy-setup',
    description: 'Configure proxy and SSL certificate settings',
    group: 'config',
    transport: 'agent',
    inputHint: 'KEY value to set, KEY to unset, or empty for help',
  },
  {
    name: 'model',
    description: 'Switch the active model for this session',
    group: 'session',
    transport: 'local',
    action: 'pick-model',
  },
  {
    name: 'thinking',
    description: 'Set the reasoning effort level',
    group: 'session',
    transport: 'local',
    action: 'pick-thinking',
  },
  {
    name: 'theme',
    description: 'Switch between the ink and paper palettes',
    group: 'session',
    transport: 'local',
    action: 'toggle-theme',
  },
  {
    name: 'clear',
    description: 'Start a new conversation, optionally seeded with a prompt',
    group: 'session',
    transport: 'local',
    action: 'new-session',
    aliases: ['new'],
  },
  {
    name: 'resume',
    description: 'Browse and resume a saved session for this folder',
    group: 'session',
    transport: 'local',
    action: 'open-sessions',
    aliases: ['continue'],
  },
  {
    name: 'branch',
    description: 'Fork this conversation into a new resumable session',
    group: 'session',
    transport: 'local',
    action: 'fork-session',
    aliases: ['fork'],
  },
  {
    name: 'rename',
    description: 'Set a manual session title',
    group: 'session',
    transport: 'local',
    action: 'rename-session',
    inputHint: 'New title',
  },
  {
    name: 'compact',
    description: 'Summarise the conversation to reclaim context',
    group: 'context',
    transport: 'agent',
    inputHint: 'Optional instructions to guide the summary',
  },
  {
    name: 'context',
    description: 'Show what fills the context window, by category',
    group: 'context',
    transport: 'local',
    action: 'open-context',
  },
  {
    name: 'status',
    description: 'Agent statistics for this session',
    group: 'context',
    transport: 'local',
    action: 'open-status',
  },
  {
    name: 'capabilities',
    description: 'Every boundary VibeDesk intercepts, and what each one saved',
    group: 'context',
    transport: 'local',
    action: 'open-capabilities',
    aliases: ['caps'],
  },
  {
    name: 'compaction',
    description: 'The compaction proxy, its conversations and its thresholds',
    group: 'context',
    transport: 'local',
    action: 'open-compaction',
  },
  {
    name: 'terminal',
    description: 'The rule families that shrink command output, and what fired',
    group: 'context',
    transport: 'local',
    action: 'open-terminal',
  },
  {
    name: 'reading',
    description: 'Probe the sparse-reading gate on a file and see the card',
    group: 'context',
    transport: 'local',
    action: 'open-reading',
  },
  {
    name: 'memory',
    description: 'What the agent remembers, and consolidate this conversation',
    group: 'context',
    transport: 'local',
    action: 'open-memory',
  },
  {
    name: 'code-graph',
    description: 'The repository knowledge graph server and its registration',
    group: 'context',
    transport: 'local',
    action: 'open-code-graph',
  },
  {
    name: 'web',
    description: 'The search and page-read server and its registration',
    group: 'context',
    transport: 'local',
    action: 'open-web',
  },
  {
    name: 'swarm',
    description: 'Run a swarm on a goal, or open the panel when there is no goal',
    group: 'context',
    transport: 'local',
    action: 'open-swarm',
    inputHint: 'The goal — a lead plans it, workers build it, verifiers try to break it',
    aliases: ['orchestrate'],
  },
  {
    name: 'billing',
    description: 'Plan, allowance and token usage for this chat',
    group: 'account',
    transport: 'local',
    action: 'open-billing',
    aliases: ['usage', 'cost'],
  },
  {
    name: 'retry',
    description: 'Retry a failed or interrupted response',
    group: 'context',
    transport: 'agent',
    inputHint: 'Optional additional instructions',
  },
  {
    name: 'log',
    description: 'Show the path to the current session log directory',
    group: 'context',
    transport: 'agent',
  },
  {
    name: 'skills',
    description: 'Browse installed skills and toggle them on or off',
    group: 'workspace',
    transport: 'local',
    action: 'open-skills',
  },
  {
    name: 'release-notes',
    description: 'Read the release notes of every Vibe version',
    group: 'help',
    transport: 'local',
    action: 'open-release-notes',
    aliases: ['whatsnew'],
  },
  {
    name: 'help',
    description: 'Show available commands and keyboard shortcuts',
    group: 'help',
    transport: 'local',
    action: 'open-help',
  },
  {
    name: 'whoami',
    description: 'Show the signed-in account, workspace and plan',
    group: 'account',
    transport: 'local',
    action: 'open-account',
  },
  {
    name: 'data-retention',
    description: 'Show data retention information',
    group: 'account',
    transport: 'agent',
  },
  {
    name: 'leanstall',
    description: 'Install the Lean 4 agent (leanstral)',
    group: 'account',
    transport: 'agent',
  },
  {
    name: 'unleanstall',
    description: 'Uninstall the Lean 4 agent',
    group: 'account',
    transport: 'agent',
  },
  {
    name: 'exit',
    description: 'Quit VibeDesk',
    group: 'help',
    transport: 'local',
    action: 'quit',
    hidden: true,
  },
];

export const GROUP_LABEL: Record<CommandGroup, string> = {
  session: 'Session',
  context: 'Context',
  workspace: 'Workspace',
  config: 'Configuration',
  account: 'Account',
  help: 'Help',
};

export function findCommand(name: string): SlashCommand | undefined {
  const needle = name.toLowerCase();
  return SLASH_COMMANDS.find(
    (c) => c.name === needle || (c.aliases ?? []).includes(needle),
  );
}

export function filterCommands(query: string, extra: SlashCommand[] = []): SlashCommand[] {
  // The agent reports its own command list, which overlaps the built-ins almost
  // entirely. Keep the curated entry (description, group, local action) and only
  // append commands the agent knows about and VibeDesk does not. Duplicates also
  // break React's reconciliation, because the palette keys rows by name.
  const known = new Set(SLASH_COMMANDS.map((c) => c.name));
  const all = [...SLASH_COMMANDS, ...extra.filter((c) => !known.has(c.name))];
  const needle = query.toLowerCase();
  if (!needle) return all.filter((c) => !c.hidden);
  const scored = all
    .map((cmd) => {
      const name = cmd.name.toLowerCase();
      let score = -1;
      if (name === needle) score = 0;
      else if (name.startsWith(needle)) score = 1;
      else if (name.includes(needle)) score = 2;
      else if (cmd.description.toLowerCase().includes(needle)) score = 3;
      return { cmd, score };
    })
    .filter((s) => s.score >= 0)
    .sort((a, b) => a.score - b.score || a.cmd.name.localeCompare(b.cmd.name));
  return scored.map((s) => s.cmd);
}

/**
 * What Enter does while the slash palette is open.
 *
 * `accept` completes the highlighted command into the draft. That is right for a
 * command that takes an argument, because the next thing the user types is that
 * argument. It is wrong for one that takes none: completing `/status` into
 * `/status` rewrites the draft to the text already there, closes the palette and
 * swallows the keypress, so Enter looked dead until a second press. A command
 * that is already complete runs.
 *
 * Tab is not routed through this. Tab always completes, never runs, because a
 * user reaching for Tab is asking to finish the word.
 */
export function slashEnterAction(
  results: SlashCommand[],
  activeIndex: number,
): { kind: 'run'; command: SlashCommand } | { kind: 'accept' } {
  const command = results[activeIndex];
  if (command && !command.inputHint) return { kind: 'run', command };
  return { kind: 'accept' };
}

/** Commands the ACP command controller accepts as prompt text. */
export const AGENT_COMMANDS = new Set(
  SLASH_COMMANDS.filter((c) => c.transport === 'agent').map((c) => c.name),
);

/**
 * Which skills a turn actually loaded.
 *
 * The skills panel lists what is installed. That is not the same as what was
 * used, and the difference is the whole point: a session that loaded `assay`
 * and a session that worked from memory look identical in a catalog. The only
 * evidence of use is the tool call the agent made to load one, so that is what
 * this reads.
 *
 * Four shapes exist on this machine and all are recognised:
 *
 * - `mcp_mastery.load { bundle, skill }` — the mastery bundles, which are not
 *   in the skill catalog at all and arrive only through this call.
 * - `skill { name }` — the Aegis pack and any project or user skill, loaded by
 *   the CLI's own skill tool.
 * - a read of the skill's own `SKILL.md` — how a skill is loaded when the tool
 *   is not reachable, which is every swarm member, because a member session
 *   opens with no MCP servers. The brief tells the worker to read the path.
 * - `Loading skill: <name>` — the title the app server renders for that same
 *   skill tool. It is the only evidence left when `rawInput` is not forwarded,
 *   and it is the app server's own wording for exactly one tool, so it is read
 *   rather than guessed at.
 *
 * A call that names none of these is not a skill load, and guessing from a
 * title would put an aura on a skill the agent never opened.
 */
import type { ToolCall } from '@/types/acp';

/** The tool names that load a skill, lowercased. */
const LOADERS = ['mcp_mastery.load', 'mcp_mastery_load', 'skill', 'skills.load'];

/** The app server's title for the skill tool: `Loading skill: <name>`. */
const TITLE_LOAD = /^loading skill:\s*(.+)$/i;

export interface SkillUse {
  /** The skill's name, as the catalog spells it. */
  name: string;
  /** The bundle or plugin it came from, when the call named one. */
  bundle: string | null;
}

function asRecord(value: unknown): Record<string, unknown> | null {
  return value && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : null;
}

function str(value: unknown): string | null {
  return typeof value === 'string' && value.trim() ? value.trim() : null;
}

/**
 * Split the qualified form the agent names a plugin skill by.
 *
 * The agent loads `vibe-mastery:assay`; the catalog lists that skill as `assay`
 * under the `Assay` bundle, because the catalog is built from directories and a
 * directory has no prefix. Keying the panel on the qualified name would leave
 * every mastery row unlit while the "used" strip filled up with names that
 * appear nowhere else in the panel.
 */
function qualify(name: string): SkillUse {
  const at = name.indexOf(':');
  if (at <= 0 || at === name.length - 1) return { name, bundle: null };
  return { name: name.slice(at + 1), bundle: name.slice(0, at) };
}

/**
 * The tool name a call reports.
 *
 * `title` is what the agent shows, and for an MCP tool it is usually the
 * qualified name. `rawInput` is the arguments. Neither is guaranteed, so both
 * are tried and a call that names nothing is ignored rather than guessed at.
 */
function toolName(call: ToolCall): string {
  const raw = asRecord(call.rawInput);
  const candidates = [
    str(raw?.tool),
    str(raw?.toolName),
    str(raw?.name) && LOADERS.includes((str(raw?.name) ?? '').toLowerCase()) ? str(raw?.name) : null,
    str(call.title),
  ];
  for (const candidate of candidates) {
    if (!candidate) continue;
    const lower = candidate.toLowerCase();
    if (LOADERS.some((loader) => lower === loader || lower.startsWith(`${loader} `))) {
      return lower;
    }
  }
  return '';
}

/**
 * The skill a path opens, if it opens one.
 *
 * Two shapes count: a path to a `SKILL.md`, whose parent directory is the skill,
 * and a markdown file inside a `skills/<name>/` directory, which is a skill's own
 * reference document. Both are the agent opening the skill. The name is
 * validated rather than taken on trust, so a sentence that happens to contain the
 * words "SKILL.md" cannot credit a skill called `the`.
 */
function skillFromPath(text: string): string | null {
  const normalised = text.replace(/\\/g, '/');

  const at = normalised.indexOf('SKILL.md');
  if (at >= 0) {
    const before = normalised.slice(0, at).replace(/\/+$/, '');
    const cut = before.lastIndexOf('/');
    const name = cut >= 0 ? skillName(before.slice(cut + 1)) : null;
    if (name) return name;
  }

  if (!normalised.endsWith('.md')) return null;
  const marker = normalised.lastIndexOf('/skills/');
  if (marker < 0) return null;
  const rest = normalised.slice(marker + '/skills/'.length);
  return skillName(rest.split('/')[0] ?? '');
}

/** A directory name that can be a skill name, or nothing. */
function skillName(raw: string): string | null {
  const name = raw.trim();
  if (name.length < 2 || name.length > 64) return null;
  if (!/^[A-Za-z0-9._-]+$/.test(name)) return null;
  return name;
}

/** Every string inside a JSON value, in document order. */
function stringsIn(value: unknown, out: string[]): void {
  if (typeof value === 'string') {
    out.push(value);
    return;
  }
  if (Array.isArray(value)) {
    for (const item of value) stringsIn(item, out);
    return;
  }
  const record = asRecord(value);
  if (record) for (const field of Object.values(record)) stringsIn(field, out);
}

/**
 * The skills one tool call loaded, or an empty list.
 *
 * A mastery load names `{ bundle, skill }`; the CLI's skill tool names
 * `{ name }`. Both are read from `rawInput`, because the title is prose and
 * prose is not a contract — but when `rawInput` is absent the title is the only
 * evidence there is, and `Loading skill: <name>` is unambiguous.
 */
export function skillLoads(call: ToolCall): SkillUse[] {
  const tool = toolName(call);
  const raw = asRecord(call.rawInput);

  if (tool && raw) {
    const bundle = str(raw.bundle);
    const skill = str(raw.skill) ?? str(raw.skillName);
    if (skill) return [{ name: skill, bundle }];

    // `skill { name }` — but only when the tool is the skill tool, because
    // `name` is a common argument and a `read` call with a `name` is not a load.
    if (tool === 'skill' || tool === 'skills.load') {
      const name = str(raw.name);
      if (name) return [qualify(name)];
    }
  }

  const title = str(call.title);
  const named = title ? str(TITLE_LOAD.exec(title)?.[1]) : null;
  if (named) return [qualify(named)];

  // A read of the skill's own file. Every string the call carries is searched,
  // because the path arrives under `rawInput.path` for a read, under
  // `locations[].path` when the agent reports where it went, and inside a shell
  // command when the agent opens the file with `cat`.
  const candidates: string[] = [];
  stringsIn(call.rawInput, candidates);
  for (const location of call.locations ?? []) {
    if (location.path) candidates.push(location.path);
  }
  if (title) candidates.push(title);
  for (const candidate of candidates) {
    const name = skillFromPath(candidate);
    if (name) return [qualify(name)];
  }

  return [];
}

/** Every skill a run of calls loaded, in order, without duplicates. */
export function skillsUsed(calls: readonly ToolCall[]): SkillUse[] {
  const seen = new Set<string>();
  const used: SkillUse[] = [];
  for (const call of calls) {
    for (const use of skillLoads(call)) {
      if (seen.has(use.name)) continue;
      seen.add(use.name);
      used.push(use);
    }
  }
  return used;
}

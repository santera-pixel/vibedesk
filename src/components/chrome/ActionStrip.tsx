import { useState } from 'react';
import type { LucideIcon } from 'lucide-react';
import {
  BookOpen,
  Bug,
  Check,
  Code,
  File,
  Folder,
  GitBranch,
  Globe,
  Hammer,
  Info,
  List,
  Play,
  RefreshCw,
  Search,
  Settings,
  Sparkles,
  Terminal,
  TestTube,
  Wrench,
  Zap,
} from 'lucide-react';
import type { ActionIcon, BuiltinCommand, CustomAction } from '@/lib/customize';
import type { JsonValue } from '@/types/acp';
import { applyLayout, defaultLayout } from '@/lib/customize';
import { customize, interaction, session as sessionApi, workspace } from '@/lib/bridge';
import {
  checkForUpdates,
  errorMessage,
  newChat,
  reloadPlugins,
  reloadSession,
  sendPrompt,
} from '@/lib/actions';
import { revealTheme } from '@/lib/reveal';
import { useApp, type PanelId } from '@/store/app';

/** The closed icon vocabulary from `customize.ts`, resolved to lucide components. */
export const ACTION_ICONS: Record<ActionIcon, LucideIcon> = {
  bolt: Zap,
  book: BookOpen,
  bug: Bug,
  check: Check,
  code: Code,
  file: File,
  folder: Folder,
  'git-branch': GitBranch,
  globe: Globe,
  hammer: Hammer,
  info: Info,
  list: List,
  play: Play,
  refresh: RefreshCw,
  search: Search,
  settings: Settings,
  sparkles: Sparkles,
  terminal: Terminal,
  'test-tube': TestTube,
  wrench: Wrench,
};

/** Commands that only open one of the built-in drawers. */
const PANEL_COMMANDS: Partial<Record<BuiltinCommand, Exclude<PanelId, null>>> = {
  'open-plugins': 'plugins',
  'open-config': 'config',
  'open-sessions': 'sessions',
  'open-context': 'context',
  'open-skills': 'skills',
  'open-customize': 'customize',
  'open-help': 'help',
  'open-swarm': 'swarm',
};

async function runCommand(command: BuiltinCommand): Promise<void> {
  const state = useApp.getState();

  const panel = PANEL_COMMANDS[command];
  if (panel) {
    state.setPanel(panel);
    return;
  }

  switch (command) {
    case 'reload-session':
      await reloadSession();
      return;
    case 'reload-plugins':
      await reloadPlugins();
      return;
    case 'new-chat':
      await newChat();
      return;
    case 'toggle-theme':
      // A command can arrive from a widget with no pointer behind it, so the
      // reveal spreads from the centre of the window.
      revealTheme(state.theme === 'ink' ? 'paper' : 'ink');
      return;
    case 'toggle-sidebar': {
      const current = state.layoutConfig ?? defaultLayout();
      const saved = await customize.setLayout({
        ...current,
        sidebarCollapsed: !current.sidebarCollapsed,
      });
      state.setLayoutConfig(saved);
      applyLayout(saved);
      return;
    }
    case 'focus-composer':
      document.querySelector<HTMLTextAreaElement>('.vd-input')?.focus();
      return;
    case 'check-updates':
      await checkForUpdates(true);
      return;
    default:
      return;
  }
}

export async function runAction(action: CustomAction): Promise<void> {
  const run = action.run;
  try {
    switch (run.kind) {
      case 'prompt':
        await sendPrompt(run.text);
        return;
      case 'command':
        await runCommand(run.command);
        return;
      case 'open-url': {
        const { openUrl } = await import('@tauri-apps/plugin-opener');
        await openUrl(run.url);
        return;
      }
      case 'reveal':
        await workspace.reveal(run.path);
        return;
      case 'mode': {
        const state = useApp.getState();
        if (!state.sessionId) {
          state.notify('Start a session before switching modes.', 'neutral');
          return;
        }
        await sessionApi.setMode(state.sessionId, run.mode);
        // The agent owns the mode list, but it does not push a mode change back
        // as an update, so the store would keep showing the old one until the
        // next session load. Mirror it here.
        if (state.modes) {
          state.setSession({
            sessionId: state.sessionId,
            modes: { ...state.modes, currentModeId: run.mode },
          });
        }
        return;
      }
      case 'option': {
        const state = useApp.getState();
        if (!state.sessionId) {
          state.notify('Start a session before changing options.', 'neutral');
          return;
        }
        await sessionApi.setConfigOption(state.sessionId, run.option, run.value);
        state.setConfigOptions(
          state.configOptions.map((option) =>
            option.id === run.option ? { ...option, currentValue: run.value } : option,
          ),
        );
        return;
      }
      case 'cancel': {
        const state = useApp.getState();
        if (!state.sessionId) return;
        await sessionApi.cancel(state.sessionId);
        return;
      }
      case 'approve':
      case 'reject': {
        // The one control that unblocks a stopped agent. `prompt` sends text to
        // a running agent; this answers the question that is holding it.
        const state = useApp.getState();
        const request = state.permission;
        if (!request) {
          state.notify('Nothing is waiting for approval.', 'neutral');
          return;
        }
        // Chosen by kind, not by id: the ids belong to the agent and differ
        // between agents, but `allow_once` is the one thing every ACP agent
        // agrees on. `always` prefers the always variant and falls back to the
        // one-shot one, because an agent that offers only `allow_once` should
        // still be answerable.
        const wanted = run.kind === 'approve' ? 'allow' : 'reject';
        const preferred = `${wanted}_${run.scope}`;
        const fallback = `${wanted}_once`;
        const option =
          request.options.find((one) => one.kind === preferred) ??
          request.options.find((one) => one.kind === fallback) ??
          request.options.find((one) => one.kind.startsWith(wanted)) ??
          // An agent that offers no option of the requested kind still has to
          // be answered: a request left hanging is a session that never moves.
          request.options.find((one) => one.kind.startsWith('reject')) ??
          null;
        await interaction.respondPermission(request.requestId, option?.optionId ?? null);
        state.setPermission(null);
        return;
      }
      case 'answer': {
        const state = useApp.getState();
        const request = state.elicitation;
        if (!request) {
          state.notify('Nothing is waiting for an answer.', 'neutral');
          return;
        }
        await interaction.respondElicitation(
          request.requestId,
          run.action,
          run.action === 'accept' ? ((run.content ?? {}) as JsonValue) : undefined,
        );
        state.setElicitation(null);
        return;
      }
    }
  } catch (error) {
    useApp.getState().addNotice(errorMessage(error), 'error');
  }
}

export function ActionButton({
  action,
  compact,
}: {
  action: CustomAction;
  compact?: boolean;
}) {
  const [busy, setBusy] = useState(false);
  const Icon = action.icon ? ACTION_ICONS[action.icon] : null;

  async function fire() {
    if (busy) return;
    setBusy(true);
    try {
      await runAction(action);
    } finally {
      setBusy(false);
    }
  }

  return (
    <button
      type="button"
      onClick={() => void fire()}
      disabled={busy}
      title={action.hint ?? action.label}
      className={
        compact
          ? 'vd-btn vd-btn-ghost !h-[24px] !px-2 !text-[11px]'
          : 'vd-btn vd-btn-ghost !h-[26px] !px-2.5 !text-[11.5px]'
      }
    >
      {Icon && <Icon size={11} strokeWidth={2} />}
      {action.label}
    </button>
  );
}

export function ActionStrip() {
  const actionsConfig = useApp((s) => s.actionsConfig);
  const actions = (actionsConfig?.actions ?? []).filter((action) => action.slot === 'composer');

  if (actions.length === 0) return null;

  return (
    <div className="flex items-center gap-1">
      {actions.map((action) => (
        <ActionButton key={action.id} action={action} compact />
      ))}
    </div>
  );
}

/**
 * Imperative actions shared by the composer, the command palette, the
 * toolbar and keyboard shortcuts. Everything reads the store through
 * `getState()` so it can be called from event handlers without hooks.
 */
import { confirm, open } from '@tauri-apps/plugin-dialog';
import type { PromptImage, PromptOutcome, SessionSummary } from '@/types/acp';
import {
  agent,
  chats as chatsApi,
  customize as customizeApi,
  dataPlane as dataPlaneApi,
  plugins as pluginsApi,
  prefs as prefsApi,
  session as sessionApi,
  swarm as swarmApi,
  updates,
  workstations as workstationsApi,
} from '@/lib/bridge';
import { findCommand, type SlashCommand } from '@/lib/commands';
import { withPreamble } from '@/lib/preamble';
import { revealTheme } from '@/lib/reveal';
import { transitionTo } from '@/lib/motion';
import { useApp } from '@/store/app';
import { usePanelData } from '@/store/panelData';

function store() {
  return useApp.getState();
}

export async function ensureSession(): Promise<string | null> {
  const state = store();
  if (state.sessionId) return state.sessionId;

  const cwd = state.cwd ?? state.environment?.vibeHome ?? null;
  if (!cwd) {
    state.addNotice('No working folder is open. Pick one from the title bar.', 'warn');
    return null;
  }

  try {
    const snapshot = await sessionApi.create(cwd);
    store().setSession({
      sessionId: snapshot.sessionId,
      modes: snapshot.modes,
      configOptions: snapshot.configOptions,
      trust: snapshot.trust,
    });
    return snapshot.sessionId;
  } catch (error) {
    store().addNotice(errorMessage(error), 'error');
    return null;
  }
}

/**
 * Split a data URL into the block ACP wants.
 *
 * The webview holds an attachment as a data URL because that is what an
 * `<img>` and a `FileReader` both speak. ACP wants the bare base64 and the
 * mime type separately, so the prefix is parsed off here rather than carried
 * around in two shapes.
 */
export function toPromptImage(dataUrl: string): PromptImage | null {
  const match = /^data:([^;,]+);base64,(.*)$/s.exec(dataUrl);
  if (!match?.[1] || !match[2]) return null;
  return { mimeType: match[1], data: match[2] };
}

export async function sendPrompt(rawText: string, images?: string[]): Promise<void> {
  const text = rawText.trim();
  const attached = images?.length ? images : undefined;
  if (!text && !attached) return;

  const state = store();

  // A local command takes no images, so a message that carries one is a
  // message, not a command — sending it as a prompt is the only reading that
  // does not silently throw the attachment away.
  if (text.startsWith('/') && !attached) {
    const [name, ...rest] = text.slice(1).split(/\s+/);
    const command = name ? findCommand(name) : undefined;
    if (command && command.transport === 'local') {
      await runLocalCommand(command, rest.join(' '));
      // A local command is finished once it returns, so the composer must not
      // keep its text — otherwise Enter again re-runs it instead of sending.
      //
      // Only the text that was sent is cleared. `/clear` and `/resume` are round
      // trips to the app server, and a user who starts typing the next message
      // while the new session is being created must not have it deleted under
      // them when the command finally returns.
      if (store().draft === text) state.setDraft('');
      return;
    }
  }

  // A turn is already running. ACP has no steer method — the app server
  // rejects a second turn with `A turn is already running` — so the message is
  // held and dispatched the moment the turn ends. Refusing it would lose what
  // the user typed; sending it would corrupt the turn.
  if (state.busy) {
    state.enqueuePrompt(text, attached);
    state.setDraft('');
    return;
  }

  const sessionId = await ensureSession();
  if (!sessionId) return;

  const firstTurn = !state.preambleSent;
  if (firstTurn) {
    state.markPreambleSent(true);
    state.addContextChip('VibeDesk context attached to this session');
  }

  const rowId = state.addUserMessage(text, attached);
  state.setDraft('');
  state.setBusy(true);
  state.setStopReason(null);

  // A stalled turn is still running in the agent, so the UI must not tear down
  // its busy state — the Stop control has to stay reachable, and the backend
  // clears `busy` when the agent finally answers.
  let stalled = false;

  try {
    const outcome = await promptWithRecovery(
      sessionId,
      firstTurn
        ? withPreamble(
            text,
            {
              pendingDir: state.customizePaths?.pendingDir,
              manifestPath: state.customizePaths?.manifestPath,
            },
            state.creatorMode,
          )
        : text,
      attached?.map(toPromptImage).filter((image): image is PromptImage => image !== null),
    );
    store().setStopReason(outcome.stopReason);
    // The context meter is fed by `usage_update` notifications. Only overwrite
    // it when the prompt response actually carries that shape.
    const usage = outcome.usage;
    if (usage && typeof usage.used === 'number' && typeof usage.size === 'number') {
      // The response carries no `_meta`, so keep the ledger the notifications
      // already delivered rather than blanking the cache figures.
      store().setUsage({
        used: usage.used,
        size: usage.size,
        cost: usage.cost ?? null,
        meta: store().usage?.meta ?? null,
      });
    }
    if (outcome.stopReason === 'cancelled') {
      store().addNotice('Turn cancelled.', 'warn');
    } else if (outcome.stopReason === 'max_tokens') {
      store().addNotice('The response hit the output limit.', 'warn');
    } else if (outcome.stopReason === 'max_turn_requests') {
      store().addNotice('The turn hit the tool-call limit.', 'warn');
    } else if (outcome.stopReason === 'refusal') {
      store().addNotice('The model declined this request.', 'warn');
    }
  } catch (error) {
    const message = errorMessage(error);

    // The backend still owns a turn the UI thought had ended — the reaper has
    // not cleared it yet. Put the message back in the queue rather than losing
    // it, and drop the row that never reached the agent.
    if (/stop it before sending another prompt/i.test(message)) {
      store().dropUserMessage(rowId);
      store().enqueuePrompt(text, attached);
      return;
    }

    stalled = /agent went silent/i.test(message);
    // A stalled turn did reach the agent, so the preamble is already in its
    // context; only a turn that never ran needs it re-sent.
    if (firstTurn && !stalled) store().markPreambleSent(false);
    store().addNotice(message, stalled ? 'warn' : 'error');
  } finally {
    if (!stalled) {
      store().setBusy(false);
      store().finishStreaming();
    }
  }
}

/**
 * Send the next held prompt, if the agent is free.
 *
 * Called whenever `busy` falls, which covers both a normal turn end and a
 * stalled turn the backend finally reaped. The guard stops a re-entrant drain
 * from sending two prompts at once.
 */
let draining = false;

export async function drainQueue(): Promise<void> {
  if (draining) return;
  const state = store();
  if (state.busy) return;

  const next = state.takeQueued(state.sessionId);
  if (!next) return;

  draining = true;
  try {
    await sendPrompt(next.text, next.images);
  } finally {
    draining = false;
  }
}

/**
 * Send a prompt, recovering from a stale turn.
 *
 * `-32603 A turn is already running` from the agent means a turn it owns never
 * reached its end — an unanswered elicitation used to do exactly that.
 * Cancelling the orphaned turn and retrying once is what makes the session
 * usable again without forcing a full reload.
 *
 * A `TurnInProgress` error is different: the bridge already knows a turn is in
 * flight, so retrying would only fail again. That one is reported as-is.
 */
async function promptWithRecovery(
  sessionId: string,
  text: string,
  images?: PromptImage[],
): Promise<PromptOutcome> {
  try {
    return await sessionApi.prompt(sessionId, text, images);
  } catch (error) {
    const message = errorMessage(error);
    if (/stop it before sending another prompt/i.test(message)) throw error;
    if (!/turn is already running/i.test(message)) throw error;
    await sessionApi.cancel(sessionId).catch(() => undefined);
    await new Promise((resolve) => setTimeout(resolve, 300));
    return sessionApi.prompt(sessionId, text, images);
  }
}

export async function cancelTurn(): Promise<void> {
  const state = store();
  if (!state.sessionId) return;
  try {
    await sessionApi.cancel(state.sessionId);
  } catch (error) {
    state.addNotice(errorMessage(error), 'error');
  }
}

export async function runLocalCommand(command: SlashCommand, argument: string): Promise<void> {
  const state = store();

  switch (command.action) {
    case 'open-config':
      state.setPanel('config');
      return;
    case 'open-plugins':
      state.setPanel('plugins');
      return;
    case 'open-sessions':
      state.setPanel('sessions');
      return;
    case 'open-context':
      state.setPanel('context');
      return;
    case 'open-status':
      state.setPanel('status');
      return;
    case 'open-skills':
      state.setPanel('skills');
      return;
    case 'open-release-notes':
      state.setPanel('release-notes');
      return;
    case 'open-account':
      state.setPanel('account');
      return;
    case 'open-billing':
      state.setPanel('billing');
      return;
    case 'open-help':
      state.setPanel('help');
      return;
    case 'open-capabilities':
      state.setPanel('capabilities');
      return;
    case 'open-compaction':
      state.setPanel('compaction');
      return;
    case 'open-terminal':
      state.setPanel('terminal');
      return;
    case 'open-reading':
      state.setPanel('reading');
      return;
    case 'open-memory':
      state.setPanel('memory');
      return;
    case 'open-code-graph':
      state.setPanel('code-graph');
      return;
    case 'open-web':
      state.setPanel('web');
      return;
    // `/swarm` opens the panel; `/swarm <goal>` starts a run and opens it.
    //
    // The panel opens either way, including when a precondition is blocked: a
    // command that does nothing on a workstation that cannot run a swarm is a
    // command that looks broken, and the panel is where the reason is written
    // down. Starting goes through the same `swarm_start` the panel's button
    // uses, so a goal typed into the conversation cannot start a run the panel
    // would refuse.
    case 'open-swarm': {
      const goal = argument.trim();
      state.setPanel('swarm');
      if (!goal) return;
      if (!state.cwd) {
        state.addNotice('Open a folder first — a swarm runs in a workspace.', 'warn');
        return;
      }
      try {
        const run = await swarmApi.start(goal);
        state.addNotice(
          `Swarm ${run.id} started on “${run.goal}”. The lead is planning; the panel is open.`,
          'success',
        );
      } catch (error) {
        state.addNotice(errorMessage(error), 'error');
      }
      return;
    }
    case 'toggle-theme':
      // Through `revealTheme`, not `setTheme`: this is the path a slash command
      // and an `actions.json` entry take, and a theme change with no reveal is
      // the same bug in a different doorway. There is no pointer behind a
      // command, so the reveal spreads from the centre of the window.
      revealTheme(state.theme === 'ink' ? 'paper' : 'ink');
      return;
    case 'new-session':
      await newSession(argument);
      return;
    case 'fork-session':
      await forkSession();
      return;
    case 'reload-plugins':
      await reloadPlugins();
      return;
    case 'rename-session': {
      if (!state.sessionId) {
        state.notify('No active session to rename.', 'neutral');
        return;
      }
      const title = argument.trim();
      if (!title) {
        state.notify('Usage: /rename <title>', 'neutral');
        return;
      }
      try {
        await sessionApi.setTitle(state.sessionId, title);
        state.setTitle(title);
        state.notify('Session renamed.', 'success');
      } catch (error) {
        state.addNotice(errorMessage(error), 'error');
      }
      return;
    }
    case 'pick-model':
    case 'pick-thinking':
      state.setPanel('config');
      return;
    case 'quit':
      await agent.stop().catch(() => undefined);
      window.close();
      return;
    default:
      state.notify(`/${command.name} is not wired to a local action yet.`, 'neutral');
  }
}

export async function newSession(seed?: string): Promise<void> {
  const state = store();
  const cwd = state.cwd;
  if (!cwd) {
    state.notify('Open a folder first.', 'neutral');
    return;
  }
  state.clearSession();
  try {
    const snapshot = await sessionApi.create(cwd);
    store().setSession({
      sessionId: snapshot.sessionId,
      modes: snapshot.modes,
      configOptions: snapshot.configOptions,
      trust: snapshot.trust,
    });
    store().notify('New session started.', 'success');
    if (seed?.trim()) await sendPrompt(seed);
  } catch (error) {
    store().addNotice(errorMessage(error), 'error');
  }
}

export async function forkSession(): Promise<void> {
  const state = store();
  if (!state.sessionId) {
    state.notify('No active session to fork.', 'neutral');
    return;
  }
  try {
    const snapshot = await sessionApi.fork(state.sessionId);
    store().setSession({
      sessionId: snapshot.sessionId,
      modes: snapshot.modes,
      configOptions: snapshot.configOptions,
      trust: snapshot.trust,
    });
    store().notify('Conversation forked into a new session.', 'success');
  } catch (error) {
    store().addNotice(errorMessage(error), 'error');
  }
}

/**
 * "Reload Session" — restarts the `vibe-acp` child process and opens a fresh
 * session in the same folder. Transient context is dropped; the app stays up.
 */
export async function reloadSession(): Promise<void> {
  const state = store();
  const cwd = state.cwd;
  state.setBusy(false);
  state.setPermission(null);
  state.setElicitation(null);
  try {
    const status = await agent.restart(cwd ?? undefined);
    store().setStatus(status);
    store().clearSession();
    if (cwd) {
      const snapshot = await sessionApi.create(cwd);
      store().setSession({
        sessionId: snapshot.sessionId,
        modes: snapshot.modes,
        configOptions: snapshot.configOptions,
        trust: snapshot.trust,
      });
    }
    store().notify('Session reloaded — agent process restarted.', 'success');
  } catch (error) {
    store().addNotice(errorMessage(error), 'error');
  }
}

/**
 * Creator mode.
 *
 * One boolean, but three things have to move together: the store (so the chip
 * and the banner repaint), `settings.json` (so it survives a relaunch), and the
 * agent's own instructions. The last one is the awkward part — the contract
 * rides on the first user turn, so a session that already sent one is running
 * on the old text. Clearing `preambleSent` makes the *next* prompt carry the
 * new contract, which is cheaper and quieter than spending a turn on a message
 * whose only content is "you are now in creator mode".
 */
export async function toggleCreatorMode(): Promise<void> {
  const state = store();
  const next = !state.creatorMode;
  state.setCreatorMode(next);
  if (next) state.setPanel('customize');
  if (state.preambleSent) state.markPreambleSent(false);

  try {
    // The whole object, because `prefs_set` writes the settings file whole. A
    // partial body would read the missing field as its default and silently
    // switch the other preference off.
    await prefsApi.set({ creatorMode: next, swarmSkills: state.swarmSkills });
  } catch (error) {
    // The mode is live for this run either way; only the write failed.
    state.addNotice(
      `Creator mode is ${next ? 'on' : 'off'} for this run, but could not be saved: ${errorMessage(error)}`,
      'warn',
    );
    return;
  }

  state.notify(
    next ? 'Creator mode on — the agent proposes, you decide.' : 'Creator mode off.',
    'success',
  );
}

/**
 * The skill switch: whether a swarm run is handed the installed skills.
 *
 * On by default. The switch exists because a user comparing two runs needs to be
 * able to take the method out of the equation, and because a skill that is wrong
 * about a repository should be removable without uninstalling it.
 *
 * The write is optimistic and the failure is reported rather than reverted: the
 * switch is live for this session either way, and a toggle that snaps back under
 * the user's finger is worse than one that says it could not be saved.
 */
export async function toggleSwarmSkills(): Promise<void> {
  const state = store();
  const next = !state.swarmSkills;
  state.setSwarmSkills(next);

  try {
    await prefsApi.set({ creatorMode: state.creatorMode, swarmSkills: next });
  } catch (error) {
    state.addNotice(
      `Swarm skills are ${next ? 'on' : 'off'} for this session, but could not be saved: ${errorMessage(error)}`,
      'warn',
    );
    return;
  }

  state.notify(
    next
      ? 'Swarm skills on — every run is handed the method this machine has.'
      : 'Swarm skills off — runs will work from the brief alone.',
    next ? 'success' : 'neutral',
  );
}

/**
 * "Reload Plugins" — re-discovers plugin roots from disk, diffs them against
 * the previous snapshot, and re-pins the session so new components load.
 */
/**
 * Re-read the panel list.
 *
 * A plugin contributes panels, so anything that changes which plugins are
 * enabled changes the panel list too. Without this the rail keeps showing a
 * panel whose plugin was just disabled, and a plugin installed while the app is
 * running contributes nothing until the next launch.
 */
export async function refreshPanels(): Promise<void> {
  try {
    store().setPanelsConfig(await customizeApi.getPanels());
  } catch (error) {
    store().addNotice(errorMessage(error), 'error');
  }
}

/**
 * Answer the consent question a panel's source is asking.
 *
 * The backend re-reads the panel file and reconciles, so an allowed origin
 * starts fetching without a restart. `null` puts the origin back to asking,
 * which is how a user undoes a decision they do not remember making.
 */
export async function decideConsent(
  origin: string,
  decision: 'allow' | 'deny' | null,
): Promise<void> {
  try {
    const status = await dataPlaneApi.consentSet(origin, decision);
    usePanelData.getState().setStatus(status);
    if (decision === 'allow') store().notify(`Allowed ${origin}`, 'success');
    else if (decision === 'deny') store().notify(`Refused ${origin}`, 'success');
  } catch (error) {
    store().addNotice(errorMessage(error), 'error');
  }
}

export async function reloadPlugins(): Promise<void> {
  const state = store();
  try {
    const report = await pluginsApi.reload(true);
    store().setPlugins(report.plugins);
    // The panel list is derived from the enabled set, so it is stale the moment
    // the plugin list changes.
    await refreshPanels();
    const parts: string[] = [];
    if (report.added.length) parts.push(`+${report.added.length} added`);
    if (report.removed.length) parts.push(`−${report.removed.length} removed`);
    if (report.updated.length) parts.push(`~${report.updated.length} updated`);
    const summary = parts.length ? parts.join('  ') : 'no changes';
    store().notify(`Plugins reloaded · ${summary}`, 'success');
    if (report.repinned) {
      store().addNotice(
        `Plugin snapshot re-pinned (${summary}). New skills, MCP servers and hooks are live.`,
        'success',
      );
    }
  } catch (error) {
    state.addNotice(errorMessage(error), 'error');
  }
}

export async function checkForUpdates(force = false): Promise<void> {
  try {
    const status = await updates.check(force);
    store().setUpdate(status);
    if (status.updateAvailable) {
      store().notify(`Vibe ${status.latestVersion} is available.`, 'neutral');
    } else if (force) {
      store().notify('Vibe is up to date.', 'success');
    }
  } catch (error) {
    store().setUpdate({
      currentVersion: null,
      latestVersion: null,
      updateAvailable: false,
      channel: 'stable',
      checkedAt: new Date().toISOString(),
      installMethod: 'unknown',
      upgradeCommand: null,
      releaseUrl: null,
      notes: null,
      error: errorMessage(error),
      applying: false,
    });
  }
}

/* ------------------------------------------------------------------ *
 * Workstations and chats
 * ------------------------------------------------------------------ */

/** Windows paths differ in case and separators; compare them loosely. */
function samePath(a: string | null | undefined, b: string | null | undefined): boolean {
  if (!a || !b) return false;
  // `\\?\` is the Windows verbatim prefix. The agent stores session history
  // under the plain form, so a verbatim cwd would never match its own chats.
  const norm = (value: string) =>
    value
      .replace(/^\\\\\?\\UNC\\/i, '//')
      .replace(/^\\\\\?\\/, '')
      .replace(/\\/g, '/')
      .replace(/\/+$/, '')
      .toLowerCase();
  return norm(a) === norm(b);
}

/**
 * Read the pinned folders and mark the one matching the open folder. Called at
 * boot, after the agent has already opened its last workspace.
 */
export async function loadWorkstations(): Promise<void> {
  const state = store();
  try {
    const list = await workstationsApi.list();
    state.setWorkstations(list);
    const match = list.find((entry) => samePath(entry.path, state.cwd));
    state.setActiveWorkstation(match?.path ?? null);
    // Even an unpinned folder has chats worth showing; the rail just marks it
    // as not pinned.
    await refreshChats();
  } catch (error) {
    state.addNotice(errorMessage(error), 'error');
  }
}

export async function addWorkstation(): Promise<void> {
  const state = store();
  try {
    const picked = await open({
      directory: true,
      multiple: false,
      title: 'Add a workstation',
    });
    if (typeof picked !== 'string') return;
    await pinWorkstation(picked);
  } catch (error) {
    state.addNotice(errorMessage(error), 'error');
  }
}

/** Pin a folder and switch to it. Used by the rail and the title bar. */
export async function pinWorkstation(path: string): Promise<void> {
  const state = store();
  try {
    const list = await workstationsApi.add(path);
    store().setWorkstations(list);
    await selectWorkstation(path);
  } catch (error) {
    state.addNotice(errorMessage(error), 'error');
  }
}

export async function removeWorkstation(path: string): Promise<void> {
  const state = store();
  try {
    const list = await workstationsApi.remove(path);
    store().setWorkstations(list);
    if (samePath(state.activeWorkstation, path)) {
      store().setActiveWorkstation(null);
      store().setChats([]);
    }
  } catch (error) {
    state.addNotice(errorMessage(error), 'error');
  }
}

/**
 * Switch projects. The agent process keeps running — ACP sessions carry their
 * own cwd — so this only moves the bridge's view and reopens the folder's most
 * recent chat.
 *
 * `open` is false when the caller is about to create a chat itself, so switching
 * does not leave an empty session behind.
 */
export async function selectWorkstation(path: string, open = true): Promise<void> {
  const state = store();
  if (state.busy) {
    state.notify('The agent is still working. Stop it before switching projects.', 'neutral');
    return;
  }

  store().setActiveWorkstation(path);
  useApp.setState({ cwd: path });

  try {
    await agent.setWorkspace(path);
  } catch (error) {
    store().addNotice(errorMessage(error), 'error');
  }

  const chats = await refreshChats();
  if (!open) return;
  if (chats.length > 0) {
    await openChat(chats[0]!.sessionId);
  } else {
    await newChat();
  }
}

/** Sessions saved for the active folder, newest first. */
export async function refreshChats(): Promise<SessionSummary[]> {
  const state = store();
  const cwd = state.cwd;
  if (!cwd) {
    state.setChats([]);
    return [];
  }

  state.setChatsLoading(true);
  try {
    const list = await sessionApi.list(cwd);
    const mine = list
      .filter((entry) => !entry.cwd || samePath(entry.cwd, cwd))
      .sort((a, b) => (b.updatedAt ?? '').localeCompare(a.updatedAt ?? ''));
    store().setChats(mine);
    return mine;
  } catch (error) {
    store().addNotice(errorMessage(error), 'error');
    return [];
  } finally {
    store().setChatsLoading(false);
  }
}

/**
 * Read the archive and delete lists.
 *
 * Called at boot and after every change. The rail filters on these rather than
 * on a pre-filtered list from the backend, so toggling "show archived" is a
 * render, not a round trip.
 */
export async function loadChatState(): Promise<void> {
  try {
    const state = await chatsApi.state();
    store().setChatState(state.archived, state.deleted);
  } catch {
    // A missing or unreadable file means nothing is archived. The rail still
    // works; it just shows everything.
  }
}

/** Put a chat away, or bring it back. Nothing on disk moves. */
export async function archiveChat(sessionId: string, archived: boolean): Promise<void> {
  try {
    const state = await chatsApi.archive(sessionId, archived);
    store().setChatState(state.archived, state.deleted);
    store().notify(archived ? 'Chat archived' : 'Chat restored', 'success');
  } catch (error) {
    store().addNotice(errorMessage(error), 'error');
  }
}

/**
 * Delete a chat.
 *
 * The session directory moves into `<data>/trash/sessions/`, so this is
 * recoverable by hand but gone from both VibeDesk and `vibe --resume`. The
 * confirmation says so rather than asking a vague "are you sure".
 */
export async function deleteChat(sessionId: string, title: string): Promise<void> {
  const state = store();
  if (sessionId === state.sessionId) {
    state.addNotice('That chat is open. Switch to another chat before deleting it.', 'warn');
    return;
  }

  let ok = false;
  try {
    ok = await confirm(
      `"${title}" will be removed from VibeDesk and from the Vibe CLI's session list. ` +
        'The session files move to VibeDesk\\trash\\sessions, so this can be undone by hand.',
      { title: 'Delete this chat?', kind: 'warning', okLabel: 'Delete', cancelLabel: 'Keep' },
    );
  } catch {
    // No dialog available: fall through and delete, since the user already
    // chose the item. The trash keeps it recoverable either way.
    ok = true;
  }
  if (!ok) return;

  try {
    const next = await chatsApi.remove(sessionId);
    store().setChatState(next.archived, next.deleted);
    store().notify('Chat deleted', 'success');
    await refreshChats();
  } catch (error) {
    store().addNotice(errorMessage(error), 'error');
  }
}

/**
 * Archive or restore many chats in one write.
 *
 * Archiving is a list edit, so there is nothing to report per chat: either the
 * write landed or it did not.
 */
export async function archiveChats(sessionIds: string[], archived: boolean): Promise<void> {
  if (sessionIds.length === 0) return;
  try {
    const result = await chatsApi.archiveMany(sessionIds, archived);
    store().setChatState(result.state.archived, result.state.deleted);
    const count = result.done.length;
    store().notify(
      archived
        ? `${count} ${count === 1 ? 'chat' : 'chats'} archived`
        : `${count} ${count === 1 ? 'chat' : 'chats'} restored`,
      'success',
    );
  } catch (error) {
    store().addNotice(errorMessage(error), 'error');
  }
}

/**
 * Delete many chats at once.
 *
 * The open chat is filtered out here as well as in the backend. The backend
 * skips it so a bulk call cannot abort; this keeps the confirmation from
 * promising something it will not do.
 */
export async function deleteChats(sessionIds: string[]): Promise<void> {
  const state = store();
  const ids = sessionIds.filter((id) => id !== state.sessionId);
  if (ids.length === 0) {
    state.addNotice('That chat is open. Switch to another chat before deleting it.', 'warn');
    return;
  }

  let ok = false;
  try {
    ok = await confirm(
      `${ids.length} ${ids.length === 1 ? 'chat' : 'chats'} will be removed from VibeDesk and from the Vibe CLI's session list. ` +
        'The session files move to VibeDesk\\trash\\sessions, so this can be undone by hand.',
      {
        title: `Delete ${ids.length} ${ids.length === 1 ? 'chat' : 'chats'}?`,
        kind: 'warning',
        okLabel: 'Delete',
        cancelLabel: 'Keep',
      },
    );
  } catch {
    // No dialog available: fall through and delete, since the user already
    // chose the action. The trash keeps it recoverable either way.
    ok = true;
  }
  if (!ok) return;

  try {
    const result = await chatsApi.removeMany(ids);
    store().setChatState(result.state.archived, result.state.deleted);
    await refreshChats();

    const deleted = result.done.length;
    if (result.failed.length > 0) {
      store().addNotice(
        `${deleted} deleted, ${result.failed.length} could not be: ${result.failed[0]!.error}`,
        'error',
      );
    } else if (result.skipped.length > 0) {
      store().addNotice(
        `${deleted} deleted, ${result.skipped.length} skipped because open`,
        'warn',
      );
    } else {
      store().notify(`${deleted} ${deleted === 1 ? 'chat' : 'chats'} deleted`, 'success');
    }
  } catch (error) {
    store().addNotice(errorMessage(error), 'error');
  }
}

export async function newChat(): Promise<void> {
  const state = store();
  const cwd = state.cwd;
  if (!cwd) {
    state.notify('Pick a workstation first.', 'neutral');
    return;
  }
  if (state.busy) {
    state.notify('The agent is still working. Stop it before starting a chat.', 'neutral');
    return;
  }

  state.clearSession();
  try {
    const snapshot = await sessionApi.create(cwd);
    store().setSession({
      sessionId: snapshot.sessionId,
      modes: snapshot.modes,
      configOptions: snapshot.configOptions,
      trust: snapshot.trust,
    });
    await refreshChats();
  } catch (error) {
    store().addNotice(errorMessage(error), 'error');
  }
}

/**
 * Resume a saved chat. `session/load` replays the transcript as
 * `session/update` notifications, so the store is cleared first and refilled by
 * the replay. The replay is buffered and reduced per frame, and the transcript
 * keeps only its newest rows — a long chat opens at the bottom, not by
 * re-rendering every message it ever had.
 */
export async function openChat(sessionId: string): Promise<void> {
  const state = store();
  const cwd = state.cwd;
  if (!cwd || state.sessionId === sessionId) return;
  if (state.busy) {
    state.notify('The agent is still working. Stop it before switching chats.', 'neutral');
    return;
  }

  // The transcript is what changes, so the transcript is what cross-fades.
  // Wrapped here rather than around the whole load: the replay streams in over
  // the following seconds and a transition cannot span an await.
  transitionTo('chat', () => {
    state.clearSession();
    store().setChatLoading(true);
  });
  try {
    const snapshot = await sessionApi.load(cwd, sessionId);
    store().setSession({
      sessionId: snapshot.sessionId,
      modes: snapshot.modes,
      configOptions: snapshot.configOptions,
      trust: snapshot.trust,
    });
    // The replayed history already carries the preamble, so don't inject it again.
    store().markPreambleSent(true);
    await refreshChats();
  } catch (error) {
    store().addNotice(errorMessage(error), 'error');
  } finally {
    store().setChatLoading(false);
  }
}

export function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message;
  if (typeof error === 'string') return error;
  if (error && typeof error === 'object' && 'message' in error) {
    return String((error as { message: unknown }).message);
  }
  return 'Unexpected bridge error';
}

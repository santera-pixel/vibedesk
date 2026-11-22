import { useCallback, useEffect, useRef } from 'react';
import { AlertTriangle, Loader2 } from 'lucide-react';
import type { ElicitationRequest, PermissionRequest, SessionUpdate } from '@/types/acp';
import {
  agent,
  customize as customizeApi,
  dataPlane,
  debug as debugApi,
  onConfigChanged,
  onLog,
  onNotification,
  onPanelData,
  onPendingChanged,
  onRequest,
  onStatus,
  onTurnEnded,
  plugins as pluginsApi,
  prefs as prefsApi,
  provision as provisionApi,
  session as sessionApi,
  setup as setupApi,
  sources as sourcesApi,
  vibe,
} from '@/lib/bridge';
import { applyLayout, applyTheme } from '@/lib/customize';
import { installSprings } from '@/lib/motion';
import { checkForUpdates, drainQueue, errorMessage, loadWorkstations } from '@/lib/actions';
import { useApp } from '@/store/app';
import { usePanelData } from '@/store/panelData';
import { TitleBar } from '@/components/chrome/TitleBar';
import { StatusBar } from '@/components/chrome/StatusBar';
import { Rail } from '@/components/chrome/Rail';
import { WorkstationRail } from '@/components/chrome/WorkstationRail';
import { Transcript } from '@/components/chat/Transcript';
import { Composer } from '@/components/composer/Composer';
import { ApprovalBar } from '@/components/chat/ApprovalBar';
import { ProposalBar } from '@/components/chat/ProposalBar';
import { CreatorBanner } from '@/components/chat/CreatorBanner';
import { ElicitationBar } from '@/components/chat/ElicitationBar';
import { SwarmStrip } from '@/components/chat/SwarmStrip';
import { ToastHost } from '@/components/ui/Toast';
import { SetupScreen, needsProvisioning } from '@/components/setup/SetupScreen';
import { PluginsPanel } from '@/components/panels/PluginsPanel';
import { ConfigPanel } from '@/components/panels/ConfigPanel';
import { SessionsPanel } from '@/components/panels/SessionsPanel';
import { CustomizePanel } from '@/components/panels/CustomizePanel';
import { CustomPanels } from '@/components/panels/CustomPanels';
import { BillingPanel } from '@/components/panels/BillingPanel';
import { SettingsPanel } from '@/components/panels/SettingsPanel';
import { ActionStrip } from '@/components/chrome/ActionStrip';
import {
  AccountPanel,
  ContextPanel,
  HelpPanel,
  ReleaseNotesPanel,
  SkillsPanel,
  StatusPanel,
} from '@/components/panels/InfoPanels';
import {
  CapabilitiesPanel,
  ChannelerPanel,
  CodeGraphPanel,
  CompactionPanel,
  MemoryPanel,
  ReadingPanel,
  TerminalPanel,
  WebPanel,
} from '@/components/panels/CapabilityPanels';
import { SwarmPanel } from '@/components/panels/SwarmPanel';

export default function App() {
  const theme = useApp((s) => s.theme);
  const panel = useApp((s) => s.panel);
  const bootError = useApp((s) => s.bootError);
  const setup = useApp((s) => s.setup);
  const provision = useApp((s) => s.provision);
  const status = useApp((s) => s.status);
  const busy = useApp((s) => s.busy);
  const booted = useRef(false);

  /* theme */
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  /* The two spring curves are generated, not written: `springToLinear` samples
     a damped oscillator and the result goes into the token namespace, so the
     stylesheet and the physics cannot disagree. Once, before the first paint
     that could use them. */
  useEffect(() => {
    installSprings();
  }, []);

  /**
   * A prompt typed mid-turn is held, not refused. Dispatch it the moment the
   * agent is free again — which covers both a normal turn end and a stalled
   * turn the backend finally reaped.
   */
  useEffect(() => {
    if (!busy) void drainQueue();
  }, [busy]);

  /* boot — re-runnable, so the setup screen can retry after an install */
  const boot = useCallback(async () => {
    const state = useApp.getState();
    state.setBootError(null);

    // First run: is the CLI even on this machine? VibeDesk wraps the installed
    // agent rather than bundling one, so this is a normal state, not a crash.
    const setupStatus = await setupApi.status().catch(() => null);
    // Provisioning answers a different question from `setup`: not "is the agent
    // installed" but "will a swarm run work". A machine can pass the first and
    // fail the second, and that is the state where the lead implements the goal
    // itself instead of planning it.
    const provisionStatus = await provisionApi.status().catch(() => null);
    if (provisionStatus) state.setProvision(provisionStatus);
    if (setupStatus && !setupStatus.binaryFound) {
      state.setSetup(setupStatus);
      try {
        state.setEnvironment(await vibe.environment());
      } catch {
        /* environment is best-effort */
      }
      return;
    }
    state.setSetup(setupStatus);
    if (needsProvisioning(provisionStatus)) return;

    try {
      const next = await agent.start();
      state.setStatus(next);
      if (next.cwd) {
        useApp.setState({ cwd: next.cwd });
        const snapshot = await sessionApi.create(next.cwd);
        state.setSession({
          sessionId: snapshot.sessionId,
          modes: snapshot.modes,
          configOptions: snapshot.configOptions,
          trust: snapshot.trust,
        });
      }
    } catch (error) {
      state.setBootError(errorMessage(error));
    }

    try {
      const environment = await vibe.environment();
      state.setEnvironment(environment);
      if (!useApp.getState().cwd) {
        useApp.setState({ cwd: environment.vibeHome });
      }
    } catch {
      /* environment is best-effort */
    }

    try {
      state.setPlugins(await pluginsApi.list());
    } catch {
      /* plugins are best-effort */
    }

    // Pinned folders and their chats. Best-effort: the chat panel works
    // without them, it just has no history to show.
    void loadWorkstations();

    // Runtime theme, layout, actions and panels. Applied before the first paint
    // of the transcript so a custom accent does not flash the default one first.
    try {
      const [theme, layout, actions, panels, paths] = await Promise.all([
        customizeApi.getTheme(),
        customizeApi.getLayout(),
        customizeApi.getActions(),
        customizeApi.getPanels(),
        customizeApi.paths(),
      ]);
      state.setThemeConfig(theme);
      state.setLayoutConfig(layout);
      state.setActionsConfig(actions);
      state.setPanelsConfig(panels);
      state.setCustomizePaths(paths);
      applyTheme(theme);
      applyLayout(layout);
    } catch {
      /* a broken theme must never block boot */
    }

    // Creator mode is a persisted preference, not a per-session toggle: it is
    // read before the first turn so the very first prompt already carries the
    // builder contract.
    try {
      state.setCreatorMode((await prefsApi.get()).creatorMode);
    } catch {
      /* a missing prefs file means the default, which is off */
    }

    // Anything the agent proposed while the app was closed, and anything a
    // plugin ships. Both are listings; neither is applied here.
    try {
      const [pending, pluginSources] = await Promise.all([
        sourcesApi.pending(),
        sourcesApi.plugins(),
      ]);
      state.setPending(pending);
      state.setPluginSources(pluginSources);
    } catch {
      /* sources are best-effort */
    }

    void checkForUpdates(false);

    // The kill switch is persisted in Rust, so its state has to be read before
    // the first panel renders or a paused plane would look like a broken one.
    void dataPlane
      .status()
      .then((status) => usePanelData.getState().setStatus(status))
      .catch(() => {
        /* a plane that cannot report is treated as empty, not as paused */
      });

    // A debugging port lets any local process drive this window. It is
    // debug-only and opt-in, but a capability the user cannot see is one they
    // cannot reason about, so it is announced rather than left to stderr.
    void debugApi
      .endpoint()
      .then((port) => {
        if (port === null) return;
        state.addNotice(
          `Remote debugging is on at 127.0.0.1:${port} — any local process can drive this window.`,
          'warn',
        );
      })
      .catch(() => {
        /* an endpoint that cannot report is not worth a second notice */
      });
  }, []);

  useEffect(() => {
    if (booted.current) return;
    booted.current = true;
    void boot();
  }, [boot]);

  /* bridge events */
  useEffect(() => {
    const unlisteners: Array<() => void> = [];
    let disposed = false;

    (async () => {
      const subs = await Promise.all([
        onNotification(({ method, params }) => {
          const state = useApp.getState();
          if (method === 'session/update') {
            const payload = params as { update?: SessionUpdate } | null;
            if (payload?.update) state.enqueueUpdate(payload.update);
            return;
          }
          if (method === '_session/retrying') {
            const detail = params as { detail?: string } | null;
            state.addNotice(`Retrying: ${detail?.detail ?? 'transient backend error'}`, 'warn');
            return;
          }
          if (method === '_config/changed') {
            state.addNotice('Configuration changed on disk. Run /reload to apply it.', 'info');
            return;
          }
          state.pushLog({
            level: 'debug',
            message: `notification ${method}`,
            at: new Date().toISOString(),
          });
        }),
        onRequest(({ requestId, method, params }) => {
          const state = useApp.getState();
          if (method === 'session/request_permission') {
            const payload = params as {
              sessionId?: string;
              toolCall?: PermissionRequest['toolCall'];
              options?: PermissionRequest['options'];
            } | null;
            state.setPermission({
              requestId,
              sessionId: payload?.sessionId ?? state.sessionId ?? '',
              toolCall: payload?.toolCall ?? { toolCallId: 'unknown' },
              options: payload?.options ?? [],
            });
            return;
          }
          if (method === 'elicitation/create') {
            // ACP shape: { sessionId, toolCallId?, message, mode: "form"|"url",
            // requestedSchema }. `mode` is a string, not an object.
            const payload = params as {
              sessionId?: string;
              toolCallId?: string;
              message?: string;
              mode?: string;
              requestedSchema?: ElicitationRequest['schema'];
              url?: string;
            } | null;
            state.setElicitation({
              requestId,
              sessionId: payload?.sessionId ?? state.sessionId ?? '',
              toolCallId: payload?.toolCallId ?? null,
              message: payload?.message ?? 'The agent needs input',
              mode: payload?.mode ?? 'form',
              schema: payload?.requestedSchema ?? { type: 'object', properties: {} },
              url: payload?.url ?? null,
            });
            return;
          }
          state.pushLog({
            level: 'warn',
            message: `unhandled agent request ${method}`,
            at: new Date().toISOString(),
          });
        }),
        onStatus((next) => {
          useApp.getState().setStatus(next);
        }),
        onTurnEnded(() => {
          // A stalled turn the webview had stopped waiting for finally closed.
          // This is the only signal that can clear `busy` for it.
          const state = useApp.getState();
          state.setBusy(false);
          state.finishStreaming();
        }),
        onLog((line) => {
          useApp.getState().pushLog(line);
        }),
        onConfigChanged((payload) => {
          const state = useApp.getState();

          // Each half is applied independently: a broken theme.json must not
          // stop a valid layout.json from taking effect, and vice versa. A null
          // half means "keep what you have", so a typo never wipes the UI.
          if (payload.theme) {
            state.setThemeError(null);
            state.setThemeConfig(payload.theme);
            applyTheme(payload.theme);
          } else if (payload.error) {
            state.setThemeError(payload.error);
          }

          if (payload.layout) {
            state.setLayoutError(null);
            state.setLayoutConfig(payload.layout);
            applyLayout(payload.layout);
          } else if (payload.error) {
            state.setLayoutError(payload.error);
          }

          if (payload.actions) {
            state.setActionsError(null);
            state.setActionsConfig(payload.actions);
          } else if (payload.error) {
            state.setActionsError(payload.error);
          }

          if (payload.panels) {
            state.setPanelsError(null);
            state.setPanelsConfig(payload.panels);
          } else if (payload.error) {
            state.setPanelsError(payload.error);
          }

          if (payload.error) {
            state.addNotice(`Config not applied — ${payload.error}`, 'warn');
          }

          // Rust reconciles the data plane before it emits, so the running set
          // has already changed by the time this arrives. Re-reading it here is
          // what keeps a panel's "live" badge honest after an edit that stopped
          // its sources.
          void dataPlane
            .status()
            .then((status) => usePanelData.getState().setStatus(status))
            .catch(() => {
              /* a plane that cannot report keeps its last known state */
            });
        }),
        onPendingChanged((next) => {
          useApp.getState().setPending(next);
        }),
        onPanelData((event) => {
          usePanelData.getState().setPoint(event);
        }),
      ]);

      if (disposed) {
        subs.forEach((fn) => fn());
        return;
      }
      unlisteners.push(...subs);
    })();

    return () => {
      disposed = true;
      unlisteners.forEach((fn) => fn());
    };
  }, []);

  /* shortcuts */
  useEffect(() => {
    function onKeyDown(event: KeyboardEvent) {
      const mod = event.ctrlKey || event.metaKey;
      if (!mod) return;

      if (event.key.toLowerCase() === 'r' && !event.shiftKey) {
        event.preventDefault();
        void import('@/lib/actions').then((m) => m.reloadSession());
        return;
      }
      if (event.key.toLowerCase() === 'p' && event.shiftKey) {
        event.preventDefault();
        void import('@/lib/actions').then((m) => m.reloadPlugins());
        return;
      }
      if (event.key.toLowerCase() === 'k') {
        event.preventDefault();
        document.querySelector<HTMLTextAreaElement>('.vd-input')?.focus();
      }
    }
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, []);

  return (
    <div className="flex h-full flex-col bg-[var(--vd-canvas)]">
      <TitleBar />

      <div className="vd-shell flex min-h-0 flex-1">
        <Rail />

        <main className="flex min-w-0 flex-1 flex-col">
          {setup && (!setup.binaryFound || needsProvisioning(provision)) ? (
            <SetupScreen onReady={() => void boot()} />
          ) : bootError ? (
            <BootError message={bootError} binaryFound={setup?.binaryFound ?? false} />
          ) : (
            <>
              <Transcript />
              <CreatorBanner />
              <ProposalBar />
              <ApprovalBar />
              <ElicitationBar />
              {/* The swarm sits with the conversation, not only in its drawer: a
                  run started from `/swarm <goal>` has to stay visible while the
                  user keeps talking. It renders nothing when no run is live. */}
              <SwarmStrip />
              <div className="mx-auto w-full max-w-[var(--vd-composer-max)] px-6">
                <ActionStrip />
              </div>
              <Composer />
            </>
          )}
        </main>

        {!(setup && !setup.binaryFound) && !bootError && <WorkstationRail />}

        {panel === 'settings' && <SettingsPanel />}
        {panel === 'plugins' && <PluginsPanel />}
        {panel === 'config' && <ConfigPanel />}
        {panel === 'sessions' && <SessionsPanel />}
        {panel === 'context' && <ContextPanel />}
        {panel === 'status' && <StatusPanel />}
        {panel === 'skills' && <SkillsPanel />}
        {panel === 'release-notes' && <ReleaseNotesPanel />}
        {panel === 'account' && <AccountPanel />}
        {panel === 'billing' && <BillingPanel />}
        {panel === 'customize' && <CustomizePanel />}
        {panel === 'help' && <HelpPanel />}
        {panel === 'capabilities' && <CapabilitiesPanel />}
        {panel === 'compaction' && <CompactionPanel />}
        {panel === 'terminal' && <TerminalPanel />}
        {panel === 'reading' && <ReadingPanel />}
        {panel === 'memory' && <MemoryPanel />}
        {panel === 'code-graph' && <CodeGraphPanel />}
        {panel === 'web' && <WebPanel />}
        {panel === 'channeler' && <ChannelerPanel />}
        {panel === 'swarm' && <SwarmPanel />}
        <CustomPanels />
      </div>

      <StatusBar />
      <ToastHost />

      {/* The gooey filter's definition. It is referenced by `.vd-gooey`, which
          is only ever on an element that exists while something is happening,
          so nothing in the chrome resolves a filter at rest. */}
      <svg aria-hidden className="pointer-events-none absolute size-0" focusable="false">
        <filter id="vd-gooey" colorInterpolationFilters="sRGB">
          <feGaussianBlur in="SourceGraphic" stdDeviation="3" result="blur" />
          <feColorMatrix
            in="blur"
            type="matrix"
            values="1 0 0 0 0  0 1 0 0 0  0 0 1 0 0  0 0 0 22 -9"
          />
        </filter>
      </svg>

      {status?.state === 'starting' && (
        <div className="pointer-events-none fixed inset-0 grid place-items-center bg-[color-mix(in_oklab,var(--vd-canvas)_72%,transparent)]">
          <div className="vd-gloss flex items-center gap-2 rounded-[var(--radius-md)] border border-[var(--vd-line)] px-3 py-2">
            <Loader2 size={13} className="vd-spin text-[var(--vd-accent)]" />
            <span className="text-[12px] text-[var(--vd-text-secondary)]">
              Starting the Vibe agent…
            </span>
          </div>
        </div>
      )}
    </div>
  );
}

function BootError({ message, binaryFound }: { message: string; binaryFound: boolean }) {
  return (
    <div className="flex flex-1 items-center justify-center p-8">
      <div className="max-w-[46ch] rounded-[var(--radius-lg)] border border-[color-mix(in_oklab,var(--vd-danger)_34%,transparent)] bg-[var(--vd-danger-wash)] p-4">
        <div className="flex items-center gap-2">
          <AlertTriangle size={14} strokeWidth={2} className="text-[var(--vd-danger)]" />
          <h2 className="m-0 text-[13px] font-semibold text-[var(--vd-text)]">
            The agent could not start
          </h2>
        </div>
        <p className="m-0 mt-2 text-[12px] leading-[1.55] text-[var(--vd-text-secondary)]">
          {message}
        </p>
        {!binaryFound && (
          <p className="m-0 mt-2 text-[11px] leading-[1.5] text-[var(--vd-text-tertiary)]">
            Install the CLI with <span className="font-mono">uv tool install mistral-vibe</span>, or
            point <span className="font-mono">VIBEDESK_ACP_BIN</span> at a{' '}
            <span className="font-mono">vibe-acp</span> binary.
          </p>
        )}
      </div>
    </div>
  );
}

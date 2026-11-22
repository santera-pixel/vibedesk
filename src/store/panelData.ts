/**
 * Panel data, held in one place.
 *
 * Two inputs, one model:
 *
 * * **External** — `panel://data` events from Rust, one per subscriber per tick.
 *   A failed fetch carries `value: null` and an `error`; the previous value is
 *   kept, because a panel that blanks on one dropped poll is worse than a panel
 *   showing a stale number with a timestamp.
 * * **Built-in** — read from the app store on demand. No event, no polling: the
 *   state is already here, and a copy would be a second truth to keep in sync.
 *
 * `usePanelModel` merges them into the flat `sourceId -> value` map a binding
 * reads. It is a hook rather than a selector because the built-in half depends
 * on app state that changes for reasons unrelated to panels.
 */

import { useEffect, useMemo, useState } from 'react';
import { create } from 'zustand';
import type { CustomPanel } from '@/lib/customize';
import { panelSources } from '@/lib/customize';
import {
  resolveBuiltin,
  type BuiltinInputs,
  type DataEvent,
  type DataModel,
  type DataPlaneStatus,
  type DataPoint,
  type PanelData,
} from '@/lib/data';
import { useApp } from '@/store/app';

interface PanelDataState {
  /** `panelId -> sourceId -> point`. */
  points: PanelData;
  paused: boolean;
  safeMode: boolean;
  status: DataPlaneStatus | null;
  setPoint: (event: DataEvent) => void;
  setStatus: (status: DataPlaneStatus) => void;
  setPaused: (paused: boolean) => void;
  clear: () => void;
}

export const usePanelData = create<PanelDataState>((set) => ({
  points: {},
  paused: false,
  safeMode: false,
  status: null,

  setPoint: (event) =>
    set((state) => {
      const panel = state.points[event.panelId] ?? {};
      const previous = panel[event.sourceId];

      // A failed fetch keeps the last good value. The error and the timestamp
      // are what tell the user it is stale; blanking the panel would not.
      const next: DataPoint =
        event.value === null
          ? {
              value: previous?.value ?? null,
              at: event.at,
              error: event.error,
            }
          : { value: event.value, at: event.at, error: null };

      return {
        points: {
          ...state.points,
          [event.panelId]: { ...panel, [event.sourceId]: next },
        },
      };
    }),

  setStatus: (status) =>
    set({ status, paused: status.paused, safeMode: status.safeMode }),

  setPaused: (paused) => set({ paused }),

  clear: () => set({ points: {} }),
}));

/** The built-in inputs, read from the app store. */
function builtinInputs(): BuiltinInputs {
  const state = useApp.getState();
  const status = state.status;
  const usage = state.usage;
  const meta = usage?.meta ?? null;

  const promptTokens = meta?.promptTokens ?? null;
  const cachedTokens = meta?.cachedTokens ?? null;

  return {
    status: status
      ? {
          state: status.state,
          pid: status.pid ?? null,
          version: status.version ?? null,
          cwd: status.cwd ?? null,
          sessionId: status.sessionId ?? null,
          model: status.model ?? null,
          agent: status.agent ?? null,
          restarts: status.restarts ?? 0,
          startedAt: status.startedAt ?? null,
          error: status.error ?? null,
        }
      : null,
    usage: usage
      ? {
          used: usage.used,
          size: usage.size,
          cost: usage.cost?.amount ?? null,
          promptTokens,
          completionTokens: meta?.completionTokens ?? null,
          cachedTokens,
          // The hit rate is cached over prompt, which is what the provider
          // actually served from cache. Null rather than zero when there is no
          // prompt yet, so a panel shows a dash instead of a misleading 0%.
          hitRate:
            promptTokens !== null && promptTokens > 0 && cachedTokens !== null
              ? cachedTokens / promptTokens
              : null,
        }
      : null,
    session: status
      ? {
          id: status.sessionId ?? null,
          title: state.title,
          cwd: status.cwd ?? null,
          busy: state.busy,
          mode: state.modes?.currentModeId ?? null,
        }
      : null,
    chats: {
      total: state.chats.length,
      visible: state.chats.length,
      archived: state.archivedChats?.length ?? 0,
    },
    plugins: {
      total: state.plugins.length,
      enabled: state.plugins.filter((plugin) => plugin.enabled).length,
      names: state.plugins.map((plugin) => plugin.name),
    },
  };
}

/**
 * A one-second tick, but only while something actually needs it.
 *
 * `clock` is the only built-in that changes without an app-state change, so a
 * panel that does not bind it should not re-render every second.
 *
 * The tick is returned rather than swallowed: the model is memoised, so a
 * re-render alone does not rebuild it. Without the value in the dependency list
 * the interval fires, the component re-renders, and the memo hands back the
 * model it built a second ago — a clock that never moves.
 */
function useClockTick(enabled: boolean): number {
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!enabled) return;
    const timer = window.setInterval(() => setTick((value) => value + 1), 1000);
    return () => window.clearInterval(timer);
  }, [enabled]);
  return tick;
}

/**
 * The data model for one panel: every declared source, resolved.
 *
 * A source that has not answered yet is absent from the map, so `@exists` is
 * false and a `data` block shows a dash rather than a zero. That distinction is
 * the whole reason `MISSING` exists.
 */
export function usePanelModel(panel: CustomPanel | null): DataModel {
  const points = usePanelData((state) => (panel ? state.points[panel.id] : undefined));
  // Includes the sources a widget declared for itself. Rust runs those under the
  // panel's id, so they arrive in the same map; reading only `panel.sources`
  // here would leave a widget-declared feed running and never delivered.
  const sources = useMemo(() => (panel ? panelSources(panel) : []), [panel]);

  const needsClock = sources.some(
    (source) => source.type === 'builtin' && (source.name ?? source.id) === 'clock',
  );
  const tick = useClockTick(needsClock);

  // Built-ins are read from the app store, so this has to re-run when that
  // store changes. Subscribing to the whole store is deliberate: the model is
  // small and the alternative is enumerating every field a built-in can read.
  const appState = useApp();

  return useMemo(() => {
    const model: DataModel = {};
    const inputs = builtinInputs();

    // A plugin source id is namespaced `plugin:<plugin>/<id>`, and a path is
    // read one segment at a time, so the panel's own bindings address it by the
    // id it declared. Only this panel's sources are in the model, so the short
    // form cannot reach another plugin's data.
    const put = (id: string, value: unknown) => {
      model[id] = value;
      const tail = id.slice(id.lastIndexOf('/') + 1);
      if (tail !== id) model[tail] = value;
    };

    for (const source of sources) {
      if (source.type === 'builtin') {
        put(source.id, resolveBuiltin(source.name ?? source.id, inputs));
        continue;
      }
      const point = points?.[source.id];
      if (point && point.value !== null) {
        put(source.id, point.value);
      }
    }
    return model;
    // `appState` is in the dependency list so a status or usage change rebuilds
    // the model; `points` covers the external half.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [points, sources, appState, tick]);
}

/** The point for one source, for showing staleness and errors in the UI. */
export function useDataPoint(panelId: string, sourceId: string): DataPoint | null {
  return usePanelData((state) => state.points[panelId]?.[sourceId] ?? null);
}

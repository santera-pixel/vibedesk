/**
 * How a run of tool calls reads in the transcript.
 *
 * The bar folds consecutive calls into one line, and that line has to say two
 * things without being opened: whether the agent is still working, and whether
 * anything failed. Both are easy to get subtly wrong. The first version tested
 * for `pending` alone, and a call that is actually running reports
 * `in_progress`, so the bar sat there claiming to be finished while the agent
 * worked. That is why this is a function with tests rather than two expressions
 * inside the component.
 */

/** The statuses a call can report. `pending` and `in_progress` are both unsettled. */
export type CallStatus = 'pending' | 'in_progress' | 'completed' | 'failed';

export type ToolRunState = {
  /** At least one call has not settled. */
  live: boolean;
  /** How many calls the run holds. */
  count: number;
  /** How many of them failed. */
  failed: number;
};

/**
 * Read a run of calls.
 *
 * A missing status means the call has only just been announced, so it counts as
 * unsettled. `failed` is settled: a run whose only call failed is not still
 * working, it is done and broken, and the bar says so in the danger colour.
 */
export function toolRunState(calls: readonly { status?: string }[]): ToolRunState {
  let live = false;
  let failed = 0;

  for (const call of calls) {
    const status = call.status ?? 'pending';
    if (status === 'failed') failed += 1;
    else if (status !== 'completed') live = true;
  }

  return { live, count: calls.length, failed };
}

/**
 * What each kind of call is called when there is no title to show.
 *
 * Lives here rather than in the card because the collapsed bar and the
 * expanded card have to agree: a run that reads "shell" when folded and
 * "execute" when opened is two names for one thing.
 */
export const KIND_LABEL: Record<string, string> = {
  read: 'read',
  edit: 'edit',
  delete: 'delete',
  move: 'move',
  search: 'search',
  execute: 'shell',
  think: 'think',
  fetch: 'fetch',
  switch_mode: 'mode',
  other: 'tool',
};

export interface RunLabel {
  text: string;
  failed: boolean;
}

/**
 * The calls of a run, as one line of labels.
 *
 * The bar used to say only "on it · 6 calls", which told the user that work was
 * happening but not what it was — so the only way to find out was to open the
 * run, and opening it turned one thought back into six stacked cards. Naming
 * the calls on the bar is what lets the run stay folded.
 *
 * `limit` is a width budget, not a data limit: past it the tail is counted
 * rather than listed, because a run of twenty calls would push the bar into a
 * second line and the whole point is that it is one.
 */
export function toolRunLabels(
  calls: readonly { title?: string; kind?: string; status?: string }[],
  limit = 4,
  maxLength = 34,
): { labels: RunLabel[]; hidden: number } {
  const labels: RunLabel[] = [];
  for (const call of calls.slice(0, Math.max(0, limit))) {
    const raw = (call.title ?? '').trim() || KIND_LABEL[call.kind ?? 'other'] || 'tool';
    // A title is often a whole sentence ("Read the file at src/main.rs"); the
    // bar only has room for the shape of it.
    const text = raw.length > maxLength ? `${raw.slice(0, maxLength - 1).trimEnd()}…` : raw;
    labels.push({ text, failed: call.status === 'failed' });
  }
  return { labels, hidden: Math.max(0, calls.length - labels.length) };
}

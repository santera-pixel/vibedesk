/**
 * Time, as the transcript and the status bar show it.
 *
 * Three different questions, three different formats, and they are not
 * interchangeable: a message needs a wall clock ("14:32:07"), a turn needs a
 * duration ("2m 14s"), and a session needs a span ("open 41m"). Rendering any
 * of them with the wrong one is how a timestamp ends up meaning nothing.
 *
 * Everything here is pure and takes `now` explicitly, so a test can pin the
 * clock instead of racing it.
 */

const MONTHS = [
  'Jan',
  'Feb',
  'Mar',
  'Apr',
  'May',
  'Jun',
  'Jul',
  'Aug',
  'Sep',
  'Oct',
  'Nov',
  'Dec',
] as const;

function pad(value: number): string {
  return value < 10 ? `0${value}` : String(value);
}

function sameDay(a: Date, b: Date): boolean {
  return (
    a.getFullYear() === b.getFullYear() &&
    a.getMonth() === b.getMonth() &&
    a.getDate() === b.getDate()
  );
}

/**
 * When a message was sent.
 *
 * Seconds are shown because a turn can be short enough that two messages in
 * the same minute are the normal case, and "14:32" twice in a row tells the
 * user nothing about which came first. Anything not from today carries its
 * date, because a bare clock on yesterday's message is a lie about when.
 */
export function formatClock(at: number, now: number = Date.now()): string {
  const date = new Date(at);
  if (Number.isNaN(date.getTime())) return '';
  const clock = `${pad(date.getHours())}:${pad(date.getMinutes())}:${pad(date.getSeconds())}`;
  if (sameDay(date, new Date(now))) return clock;
  return `${MONTHS[date.getMonth()]} ${date.getDate()}, ${clock}`;
}

/**
 * How long something took.
 *
 * Sub-second is kept to one decimal because a fast tool call is still worth
 * distinguishing from an instant one, and `0s` reads as "did not happen".
 */
export function formatDuration(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return '—';
  if (ms < 1000) return `${(ms / 1000).toFixed(1)}s`;
  const total = Math.round(ms / 1000);
  if (total < 60) return `${total}s`;
  const minutes = Math.floor(total / 60);
  const seconds = total % 60;
  if (minutes < 60) return seconds === 0 ? `${minutes}m` : `${minutes}m ${seconds}s`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return rest === 0 ? `${hours}h` : `${hours}h ${pad(rest)}m`;
}

/**
 * How long a chat has been open.
 *
 * Coarser than `formatDuration` on purpose: this is a fact about the session,
 * not about a turn, and it is read at a glance rather than compared.
 */
export function formatSpan(ms: number): string {
  if (!Number.isFinite(ms) || ms < 0) return '—';
  const total = Math.floor(ms / 1000);
  if (total < 60) return `${total}s`;
  const minutes = Math.floor(total / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return minutes % 60 === 0 ? `${hours}h` : `${hours}h ${pad(minutes % 60)}m`;
  const days = Math.floor(hours / 24);
  return hours % 24 === 0 ? `${days}d` : `${days}d ${hours % 24}h`;
}

/**
 * A turn's elapsed time, live or settled.
 *
 * `endedAt` is null while the turn runs, so the same call serves the ticking
 * counter and the final figure — one code path, so the number the user watched
 * climb is the number that stays.
 */
export function turnElapsed(
  turn: { startedAt: number; endedAt: number | null } | null,
  now: number = Date.now(),
): number | null {
  if (!turn) return null;
  return Math.max(0, (turn.endedAt ?? now) - turn.startedAt);
}

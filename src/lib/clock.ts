/**
 * A once-a-minute clock for anything that shows a span.
 *
 * A span is read at a glance, so a per-second tick would repaint the whole
 * surface sixty times a minute to change a number that moves once a minute.
 * Thirty seconds rather than sixty so the displayed minute is never more than
 * half a minute stale.
 *
 * It lives here rather than in `time.ts` because that module is pure and takes
 * `now` explicitly so a test can pin the clock; this one owns an interval and
 * cannot be tested that way. Two surfaces show a span — the status bar and the
 * composer — and they share this so they cannot tick apart.
 */

import { useEffect, useState } from 'react';

export function useMinute(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const handle = window.setInterval(() => setNow(Date.now()), 30_000);
    return () => window.clearInterval(handle);
  }, []);
  return now;
}

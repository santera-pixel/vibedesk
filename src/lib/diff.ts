/**
 * Minimal unified-diff reader for tool-call `diff` content and for
 * `rawOutput` patches. Produces renderable rows without pulling in a
 * diff library.
 */

export type DiffRowKind = 'context' | 'add' | 'del' | 'hunk' | 'meta';

export interface DiffRow {
  kind: DiffRowKind;
  text: string;
  oldLine: number | null;
  newLine: number | null;
}

export interface ParsedDiff {
  rows: DiffRow[];
  additions: number;
  deletions: number;
  isUnified: boolean;
}

const HUNK_RE = /^@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@/;

export function parseDiff(input: string): ParsedDiff {
  const lines = input.split('\n');
  const rows: DiffRow[] = [];
  let additions = 0;
  let deletions = 0;
  let oldLine = 1;
  let newLine = 1;
  let sawHunk = false;

  for (const line of lines) {
    const hunk = HUNK_RE.exec(line);
    if (hunk) {
      sawHunk = true;
      oldLine = Number(hunk[1]);
      newLine = Number(hunk[2]);
      rows.push({ kind: 'hunk', text: line, oldLine: null, newLine: null });
      continue;
    }
    if (line.startsWith('+++') || line.startsWith('---') || line.startsWith('diff ')) {
      rows.push({ kind: 'meta', text: line, oldLine: null, newLine: null });
      continue;
    }
    if (line.startsWith('+')) {
      additions += 1;
      rows.push({ kind: 'add', text: line.slice(1), oldLine: null, newLine: newLine++ });
      continue;
    }
    if (line.startsWith('-')) {
      deletions += 1;
      rows.push({ kind: 'del', text: line.slice(1), oldLine: oldLine++, newLine: null });
      continue;
    }
    if (line.startsWith(' ')) {
      rows.push({ kind: 'context', text: line.slice(1), oldLine: oldLine++, newLine: newLine++ });
      continue;
    }
    if (line === '') continue;
    rows.push({ kind: 'context', text: line, oldLine: oldLine++, newLine: newLine++ });
  }

  return { rows, additions, deletions, isUnified: sawHunk };
}

/**
 * Build a diff from two full texts by trimming the common prefix and suffix.
 * Used when a tool reports `oldText`/`newText` rather than a patch.
 */
export function diffFromTexts(oldText: string, newText: string): ParsedDiff {
  const a = oldText.split('\n');
  const b = newText.split('\n');

  let start = 0;
  while (start < a.length && start < b.length && a[start] === b[start]) start += 1;

  let endA = a.length - 1;
  let endB = b.length - 1;
  while (endA >= start && endB >= start && a[endA] === b[endB]) {
    endA -= 1;
    endB -= 1;
  }

  const rows: DiffRow[] = [];
  let additions = 0;
  let deletions = 0;

  const contextStart = Math.max(0, start - 3);
  for (let i = contextStart; i < start; i += 1) {
    rows.push({ kind: 'context', text: a[i] ?? '', oldLine: i + 1, newLine: i + 1 });
  }
  for (let i = start; i <= endA; i += 1) {
    deletions += 1;
    rows.push({ kind: 'del', text: a[i] ?? '', oldLine: i + 1, newLine: null });
  }
  for (let i = start; i <= endB; i += 1) {
    additions += 1;
    rows.push({ kind: 'add', text: b[i] ?? '', oldLine: null, newLine: i + 1 });
  }
  const contextEnd = Math.min(a.length, endA + 4);
  for (let i = endA + 1; i < contextEnd; i += 1) {
    rows.push({ kind: 'context', text: a[i] ?? '', oldLine: i + 1, newLine: i + 1 });
  }

  return { rows, additions, deletions, isUnified: false };
}

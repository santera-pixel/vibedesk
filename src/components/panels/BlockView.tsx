import { useState, type KeyboardEvent as ReactKeyboardEvent, type MouseEvent } from 'react';
import {
  ChevronDown,
  ClipboardCopy,
  ExternalLink,
  File as FileIcon,
  FileText,
  FolderOpen,
} from 'lucide-react';
import type { Block, CustomAction, Expr } from '@/lib/customize';
import { workspace } from '@/lib/bridge';
import { errorMessage } from '@/lib/actions';
import { basename } from '@/lib/utils';
import { display, evaluate, numeric, rows, type DataModel } from '@/lib/data';
import { useTween } from '@/lib/motion';
import { useApp } from '@/store/app';
import { ActionButton } from '@/components/chrome/ActionStrip';
import { ChartBlock } from '@/components/panels/ChartBlock';
import { DiagramBlock } from '@/components/panels/DiagramBlock';
import { SandboxBlock } from '@/components/panels/SandboxBlock';
import { WidgetBlock } from '@/components/panels/WidgetBlock';
import {
  ContextMenu,
  MenuItem,
  anchorAt,
  keyboardAnchor,
  type MenuAnchor,
} from '@/components/ui/ContextMenu';

/**
 * One block, drawn.
 *
 * This lives apart from `CustomPanels` because `LayoutView` renders the leaves
 * of a layout tree and must draw them exactly as the flat path does. Keeping
 * `BlockView` in `CustomPanels` made the two modules import each other, which
 * worked only because a hoisted function declaration is defined before either
 * module body runs — one `const` arrow would have turned that into a temporal
 * dead zone at first render.
 */
export function BlockView({ block, model }: { block: Block; model: DataModel }) {
  switch (block.kind) {
    case 'text':
      return <p className="vd-prose m-0 text-[12px] leading-[1.6]">{block.text}</p>;

    case 'list':
      return (
        <div>
          {block.label && <h3 className="vd-label m-0 mb-1.5">{block.label}</h3>}
          <ul className="m-0 flex list-none flex-col gap-1 p-0">
            {block.items.map((item, index) => (
              <li
                key={index}
                className="flex items-start gap-1.5 text-[11.5px] leading-[1.5] text-[var(--vd-text-secondary)]"
              >
                <span className="mt-[6px] h-[3px] w-[3px] shrink-0 rounded-[1px] bg-[var(--vd-line-strong)]" />
                <span className="min-w-0 break-words">{item}</span>
              </li>
            ))}
          </ul>
        </div>
      );

    case 'link':
      return <LinkBlock label={block.label} url={block.url} />;

    case 'actions':
      return <ActionsBlock ids={block.ids} />;

    case 'file':
      return <FileBlock path={block.path} label={block.label ?? null} />;

    case 'sandbox':
      return <SandboxBlock html={block.html} title={block.title ?? null} height={block.height} />;

    case 'widget':
      return (
        <WidgetBlock
          html={block.html}
          title={block.title ?? null}
          height={block.height}
          capabilities={block.capabilities}
          model={model}
        />
      );

    case 'data':
      return (
        <DataBlock
          label={block.label}
          value={block.value}
          hint={block.hint ?? null}
          model={model}
        />
      );

    case 'chart':
      return (
        <ChartBlock
          label={block.label}
          style={block.style}
          values={block.values}
          series={block.series}
          x={block.x}
          yMin={block.yMin}
          yMax={block.yMax}
          height={block.height}
          unit={block.unit}
          time={block.time}
          stack={block.stack}
          scale={block.scale}
          reference={block.reference}
          model={model}
        />
      );

    case 'diagram':
      return (
        <DiagramBlock
          label={block.label}
          layout={block.layout}
          nodes={block.nodes}
          edges={block.edges}
          routing={block.routing}
          height={block.height}
          model={model}
        />
      );

    case 'table':
      return (
        <TableBlock
          label={block.label ?? null}
          columns={block.columns}
          rows={block.rows}
          model={model}
        />
      );

    case 'progress':
      return (
        <ProgressBlock
          label={block.label}
          value={block.value}
          max={block.max ?? null}
          model={model}
        />
      );

    // A kind this build does not know about. That happens when the Rust side is
    // newer than the frontend, and it must not take the window down: the switch
    // is exhaustive over the kinds this build has, so anything reaching here is
    // from a version that added one.
    default:
      return (
        <p className="m-0 text-[11px] leading-[1.5] text-[var(--vd-text-faint)]">
          Unknown block kind
          {(block as { kind?: string }).kind ? ` \`${(block as { kind?: string }).kind}\`` : ''} —
          this build cannot render it.
        </p>
      );
  }
}

/* ---------------------------------------------------------------- *
 * Bound blocks
 * ---------------------------------------------------------------- */

function DataBlock({
  label,
  value,
  hint,
  model,
}: {
  label: string;
  value: Expr;
  hint: string | null;
  model: DataModel;
}) {
  const resolved = evaluate(value, model);
  const text = display(resolved);
  const absent = text === '—';

  // A live number that is replaced in one frame is unreadable: the eye cannot
  // tell 292 from 295 when the digits swap. Tweening makes the change legible
  // and shows the direction of travel. Only a change tweens — a panel that
  // opens with a tween from zero would lie about its data for 300 ms.
  const live = typeof resolved === 'number' && Number.isFinite(resolved) ? resolved : null;
  const tweened = useTween(live);
  const shown = live !== null && tweened !== null ? formatTweened(tweened, live) : text;

  return (
    <div className="flex flex-col gap-0.5">
      <h3 className="vd-label m-0">{label}</h3>
      <span
        className="vd-tabular text-[19px] font-semibold leading-[1.15] tracking-[-0.01em]"
        style={{ color: absent ? 'var(--vd-text-faint)' : 'var(--vd-text)' }}
      >
        {shown}
      </span>
      {hint && (
        <span className="text-[10.5px] leading-[1.4] text-[var(--vd-text-tertiary)]">{hint}</span>
      )}
    </div>
  );
}

/**
 * A tweened number, formatted to the precision of the value it is heading for.
 *
 * An integer stays an integer for the whole tween — a counter that reads
 * `292.47` on its way to `295` is worse than one that jumps, because the
 * decimals are noise the reader has to filter. A value that arrives with
 * decimals keeps them.
 */
function formatTweened(shown: number, target: number): string {
  const decimals = Number.isInteger(target) ? 0 : 2;
  return shown.toLocaleString(undefined, {
    minimumFractionDigits: decimals,
    maximumFractionDigits: decimals,
  });
}

function ProgressBlock({
  label,
  value,
  max,
  model,
}: {
  label: string;
  value: Expr;
  max: Expr | null;
  model: DataModel;
}) {
  const current = numeric(evaluate(value, model));
  const ceiling = max === null ? 100 : numeric(evaluate(max, model));

  if (current === null) {
    return (
      <div className="flex flex-col gap-1">
        <h3 className="vd-label m-0">{label}</h3>
        <span className="text-[11px] text-[var(--vd-text-faint)]">—</span>
      </div>
    );
  }

  const top = ceiling !== null && ceiling > 0 ? ceiling : 100;
  const ratio = Math.max(0, Math.min(1, current / top));

  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex items-baseline justify-between gap-2">
        <h3 className="vd-label m-0">{label}</h3>
        <span className="vd-tabular text-[10.5px] text-[var(--vd-text-tertiary)]">
          {display(current)} / {display(top)}
        </span>
      </div>
      <div
        className="h-[3px] w-full overflow-hidden rounded-full bg-[var(--vd-line)]"
        role="progressbar"
        aria-valuenow={Math.round(ratio * 100)}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-label={label}
      >
        {/* Scale, not width: a width transition is a layout pass on every
            frame, and a scale is a compositor transform of the same bar. */}
        <div
          className="h-full origin-left rounded-full bg-[var(--vd-accent)] transition-transform"
          style={{ transform: `scaleX(${ratio})` }}
        />
      </div>
    </div>
  );
}

function TableBlock({
  label,
  columns,
  rows: source,
  model,
}: {
  label: string | null;
  columns: string[];
  rows: Expr;
  model: DataModel;
}) {
  const cells = rows(evaluate(source, model), columns);

  return (
    <div className="flex flex-col gap-1.5">
      {label && <h3 className="vd-label m-0">{label}</h3>}
      {cells.length === 0 ? (
        <span className="text-[11px] text-[var(--vd-text-faint)]">—</span>
      ) : (
        <div className="vd-code-scroll max-h-[220px] overflow-auto rounded-[var(--radius-sm)] border border-[var(--vd-line-soft)]">
          <table className="w-full border-collapse text-[10.5px]">
            <thead>
              <tr>
                {columns.map((column) => (
                  <th
                    key={column}
                    className="sticky top-0 border-b border-[var(--vd-line)] bg-[var(--vd-surface-sunken)] px-1.5 py-1 text-left font-medium text-[var(--vd-text-tertiary)]"
                  >
                    {column}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {cells.map((row, index) => (
                <tr key={index}>
                  {row.map((cell, cellIndex) => (
                    <td
                      key={cellIndex}
                      className="vd-tabular max-w-[160px] truncate border-b border-[var(--vd-line-soft)] px-1.5 py-1 text-[var(--vd-text-secondary)] last:border-r-0"
                      title={cell}
                    >
                      {cell}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

/* ---------------------------------------------------------------- *
 * Literal blocks
 * ---------------------------------------------------------------- */

function LinkBlock({ label, url }: { label: string; url: string }) {
  const addNotice = useApp((s) => s.addNotice);

  async function open() {
    try {
      const { openUrl } = await import('@tauri-apps/plugin-opener');
      await openUrl(url);
    } catch (error) {
      addNotice(errorMessage(error), 'error');
    }
  }

  return (
    <button
      type="button"
      onClick={() => void open()}
      title={url}
      className="flex items-center gap-1 text-[11.5px] text-[var(--vd-accent)] hover:underline"
    >
      <ExternalLink size={10} strokeWidth={2} className="shrink-0" />
      <span className="min-w-0 truncate">{label}</span>
    </button>
  );
}

function ActionsBlock({ ids }: { ids: string[] }) {
  const actionsConfig = useApp((s) => s.actionsConfig);

  const actions = ids
    .map((id) => actionsConfig?.actions.find((action) => action.id === id))
    .filter((action): action is CustomAction => Boolean(action));

  if (actions.length === 0) return null;

  return (
    <div className="flex flex-wrap items-center gap-1">
      {actions.map((action) => (
        <ActionButton key={action.id} action={action} compact />
      ))}
    </div>
  );
}

/**
 * A file block is a stub until the user asks for its contents: the path is
 * rendered, nothing is read, and the context menu is the only way in.
 */
function FileBlock({ path, label }: { path: string; label: string | null }) {
  const addNotice = useApp((s) => s.addNotice);
  const [menu, setMenu] = useState<MenuAnchor | null>(null);
  const [content, setContent] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function load() {
    setMenu(null);
    setLoading(true);
    setError(null);
    try {
      setContent(await workspace.read(path));
    } catch (failure) {
      setError(errorMessage(failure));
    } finally {
      setLoading(false);
    }
  }

  async function reveal() {
    setMenu(null);
    try {
      await workspace.reveal(path);
    } catch (failure) {
      addNotice(errorMessage(failure), 'error');
    }
  }

  async function copyPath() {
    setMenu(null);
    try {
      await navigator.clipboard.writeText(path);
    } catch (failure) {
      addNotice(errorMessage(failure), 'error');
    }
  }

  function openMenu(event: MouseEvent) {
    event.preventDefault();
    setMenu(anchorAt(event, 3));
  }

  // The file card is a div, so it needs a tab stop and a key before the menu
  // behind it is reachable without a mouse.
  function menuKey(event: ReactKeyboardEvent<HTMLElement>) {
    const anchor = keyboardAnchor(event, 3);
    if (anchor) setMenu(anchor);
  }

  return (
    <div>
      <div
        role="group"
        tabIndex={0}
        aria-label={`File ${label ?? basename(path)}`}
        onContextMenu={openMenu}
        onKeyDown={menuKey}
        className="flex items-start gap-1.5 rounded-[var(--radius-sm)] focus-visible:outline focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-[var(--vd-accent)]"
      >
        <FileIcon
          size={12}
          strokeWidth={2}
          className="mt-[2px] shrink-0 text-[var(--vd-text-tertiary)]"
        />
        <div className="min-w-0 flex-1">
          <span className="block truncate text-[11.5px] text-[var(--vd-text)]">
            {label ?? basename(path)}
          </span>
          <span className="block truncate font-mono text-[10px] text-[var(--vd-text-tertiary)]">
            {path}
          </span>
          <span className="block text-[10px] text-[var(--vd-text-tertiary)]">
            right-click to load
          </span>
        </div>
      </div>

      {loading && (
        <p className="m-0 mt-1.5 text-[10.5px] text-[var(--vd-text-tertiary)]">Reading file...</p>
      )}

      {error && <p className="m-0 mt-1.5 text-[10.5px] text-[var(--vd-danger)]">{error}</p>}

      {content !== null && (
        <div className="mt-1.5 overflow-hidden rounded-[var(--radius-sm)] border border-[var(--vd-line)] bg-[var(--vd-surface-sunken)]">
          <button
            type="button"
            onClick={() => setContent(null)}
            className="flex w-full items-center gap-1.5 border-b border-[var(--vd-line-soft)] px-2 py-1 text-left"
            title="Collapse"
          >
            <ChevronDown
              size={10}
              strokeWidth={2.2}
              className="shrink-0 text-[var(--vd-text-tertiary)]"
            />
            <span className="vd-label !text-[9px]">Loaded</span>
            <span className="ml-auto text-[10px] text-[var(--vd-text-tertiary)]">collapse</span>
          </button>
          <div className="px-2 py-1.5">
            <pre className="vd-code-scroll m-0 max-h-[320px] overflow-auto whitespace-pre-wrap break-words font-mono text-[11px] leading-[1.55]">
              {content}
            </pre>
          </div>
        </div>
      )}

      {menu && (
        <ContextMenu anchor={menu} onClose={() => setMenu(null)}>
          <MenuItem icon={FileText} label="Load file" onSelect={() => void load()} />
          <MenuItem
            icon={FolderOpen}
            label="Reveal in file manager"
            onSelect={() => void reveal()}
          />
          <MenuItem icon={ClipboardCopy} label="Copy path" onSelect={() => void copyPath()} />
        </ContextMenu>
      )}
    </div>
  );
}

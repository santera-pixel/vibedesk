import { useCallback, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import type { LayoutNode, PanelNode } from '@/lib/customize';
import { visible, type DataModel } from '@/lib/data';
import { cn } from '@/lib/utils';
import { BlockView } from '@/components/panels/BlockView';

/**
 * Renders a panel's layout tree.
 *
 * Structure and data arrive on separate channels: a data tick replaces part of
 * the model and leaves the tree alone, so this component re-renders without
 * remounting anything. Every key is derived from the node's position in the
 * tree for the same reason — a key that moved with a value would throw away tab
 * selection, scroll position and any state a block holds.
 */
export function LayoutView({ layout, model }: { layout: PanelNode; model: DataModel }) {
  // Tab selection is keyed by tree position rather than held per tabs node, so
  // it survives a node being hidden by its `when` and shown again.
  const [selection, setSelection] = useState<Record<string, number>>({});

  const select = useCallback((key: string, index: number) => {
    setSelection((current) => (current[key] === index ? current : { ...current, [key]: index }));
  }, []);

  return <>{renderNode(layout, [], model, selection, select)}</>;
}

/** Where a node sits in the tree: child indices from the root. */
type Path = number[];

interface NodeProps<T extends LayoutNode> {
  node: T;
  path: Path;
  model: DataModel;
  selection: Record<string, number>;
  select: (key: string, index: number) => void;
}

/** A node's position as a stable string, used for keys and for ARIA ids. */
function keyOf(path: Path): string {
  return path.length === 0 ? 'root' : path.join('.');
}

/** Prefixed so a tab id cannot collide with an id the rest of the app owns. */
function idOf(path: Path, suffix: string): string {
  return `vd-layout-${keyOf(path)}-${suffix}`;
}

function isLayout(node: PanelNode): node is LayoutNode {
  return 'node' in node;
}

function renderNode(
  node: PanelNode,
  path: Path,
  model: DataModel,
  selection: Record<string, number>,
  select: (key: string, index: number) => void,
): ReactNode {
  // A guarded node hides its whole subtree. `visible` is the same helper the
  // flat block path uses, so a node and a block agree on what "true" means.
  if (!visible(node.when, model)) return null;

  if (!isLayout(node)) {
    return (
      <div
        key={keyOf(path)}
        className="border-b border-[var(--vd-line-soft)] px-3 py-2 last:border-b-0"
      >
        <BlockView block={node} model={model} />
      </div>
    );
  }

  switch (node.node) {
    case 'stack':
      return (
        <div key={keyOf(path)} className="flex min-w-0 flex-col">
          {node.children.map((child, index) =>
            renderNode(child, [...path, index], model, selection, select),
          )}
        </div>
      );

    case 'split':
      return (
        <SplitView
          key={keyOf(path)}
          node={node}
          path={path}
          model={model}
          selection={selection}
          select={select}
        />
      );

    case 'tabs':
      return (
        <TabsView
          key={keyOf(path)}
          node={node}
          path={path}
          model={model}
          selection={selection}
          select={select}
        />
      );

    // A node kind this build does not know about. Same treatment as an unknown
    // block kind: name it and carry on, because a renderer that throws on
    // unexpected data takes the whole window down.
    default:
      return (
        <p
          key={keyOf(path)}
          className="m-0 px-3 py-2 text-[11px] leading-[1.5] text-[var(--vd-text-faint)]"
        >
          Unknown layout node `{(node as { node?: string }).node}` — this build cannot render it.
        </p>
      );
  }
}

/**
 * Side by side. `weights` are relative widths; absent means equal.
 *
 * A hidden pane is dropped before the widths are worked out, so a guarded child
 * does not leave an empty column behind. There are no drag handles: resizing a
 * split is deferred, and a handle that did nothing would be worse than none.
 */
function SplitView({
  node,
  path,
  model,
  selection,
  select,
}: NodeProps<Extract<LayoutNode, { node: 'split' }>>) {
  const shown = node.children
    .map((child, index) => ({ child, index, weight: node.weights?.[index] ?? 1 }))
    .filter((entry) => visible(entry.child.when, model));

  if (shown.length === 0) return null;

  const total = shown.reduce((sum, entry) => sum + entry.weight, 0);

  return (
    <div className="flex min-w-0 flex-row items-stretch">
      {shown.map((entry, position) => (
        <div
          key={entry.index}
          className={cn('min-w-0', position > 0 && 'border-l border-[var(--vd-line-soft)]')}
          style={{ flex: `0 1 ${(entry.weight / total) * 100}%` }}
        >
          {renderNode(entry.child, [...path, entry.index], model, selection, select)}
        </div>
      ))}
    </div>
  );
}

/**
 * One tab visible at a time.
 *
 * The tablist is a single tab stop and the arrow keys move within it, which is
 * what roving tabindex means: only the selected tab is tabbable, so Tab leaves
 * the tablist instead of walking every label.
 */
function TabsView({
  node,
  path,
  model,
  selection,
  select,
}: NodeProps<Extract<LayoutNode, { node: 'tabs' }>>) {
  const key = keyOf(path);
  const tabs = node.tabs;
  const buttons = useRef<Array<HTMLButtonElement | null>>([]);

  // Clamped because a config reload can shrink a tabs node under a selection
  // made against the previous one. Rust refuses a tabs node with no tabs, so
  // the empty case only fires on a file that never reached the validator, and
  // an empty tablist is not worth a crash.
  const selected = Math.max(0, Math.min(selection[key] ?? 0, tabs.length - 1));
  const active = tabs[selected];
  if (!active) return null;

  function onKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    const last = tabs.length - 1;
    let next: number | null = null;
    if (event.key === 'ArrowRight') next = selected === last ? 0 : selected + 1;
    else if (event.key === 'ArrowLeft') next = selected === 0 ? last : selected - 1;
    else if (event.key === 'Home') next = 0;
    else if (event.key === 'End') next = last;
    if (next === null) return;
    event.preventDefault();
    select(key, next);
    buttons.current[next]?.focus();
  }

  return (
    <div className="flex min-w-0 flex-col">
      <div
        role="tablist"
        aria-label="Panel sections"
        onKeyDown={onKeyDown}
        className="flex shrink-0 items-center gap-0.5 border-b border-[var(--vd-line-soft)] px-2 py-1"
      >
        {tabs.map((tab, index) => (
          <button
            key={index}
            ref={(element) => {
              buttons.current[index] = element;
            }}
            type="button"
            role="tab"
            id={idOf(path, `tab-${index}`)}
            aria-selected={index === selected}
            aria-controls={idOf(path, 'panel')}
            tabIndex={index === selected ? 0 : -1}
            onClick={() => select(key, index)}
            className={cn(
              'rounded-[var(--radius-xs)] px-1.5 py-1 text-[10.5px] font-medium duration-[var(--vd-fast)]',
              index === selected
                ? 'bg-[var(--vd-surface-sunken)] text-[var(--vd-text)]'
                : 'text-[var(--vd-text-tertiary)] hover:text-[var(--vd-text-secondary)]',
            )}
          >
            {tab.label}
          </button>
        ))}
      </div>
      <div
        role="tabpanel"
        id={idOf(path, 'panel')}
        aria-labelledby={idOf(path, `tab-${selected}`)}
        className="flex min-w-0 flex-col"
      >
        {active.children.map((child, index) =>
          renderNode(child, [...path, selected, index], model, selection, select),
        )}
      </div>
    </div>
  );
}

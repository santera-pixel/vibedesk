import { ArrowUpRight, FolderGit2, Plug, Sparkles, Terminal } from 'lucide-react';
import { useApp } from '@/store/app';
import { basename } from '@/lib/utils';

const SUGGESTIONS = [
  {
    title: 'Map this codebase',
    prompt:
      'Give me a tour of this repository: entry points, module boundaries, and the three riskiest files.',
    icon: FolderGit2,
  },
  {
    title: 'Draw the architecture',
    prompt:
      'Render a Mermaid diagram of how the main components talk to each other, then explain the data flow.',
    icon: Sparkles,
  },
  {
    title: 'Audit the plugins',
    prompt: 'List the plugins this session is running and what each one contributes.',
    icon: Plug,
  },
  {
    title: 'Run the test suite',
    prompt: 'Find the test command for this project, run it, and summarise the failures.',
    icon: Terminal,
  },
];

export function EmptyState() {
  const environment = useApp((s) => s.environment);
  const cwd = useApp((s) => s.cwd);
  const pluginCount = useApp((s) => s.plugins.length);
  const setDraft = useApp((s) => s.setDraft);

  const folder = cwd ? basename(cwd) : 'no folder';

  return (
    <div className="vd-enter relative isolate flex min-h-[62vh] flex-col justify-center py-8">
      {/* The one organic surface in the app. Three shape primitives, one job:
          a mesh gradient in `oklch` so the interpolation between the three
          stops is even rather than muddy, clipped to a blob so it reads as
          drawn rather than as a rectangle, with grain over it so a large flat
          area does not band. `isolate` keeps the negative z-index inside this
          element instead of dropping it behind the canvas. */}
      <div
        aria-hidden
        className="vd-mesh vd-blob vd-grain pointer-events-none absolute -left-24 -top-16 -z-10 h-[360px] w-[560px] opacity-60"
      />

      <div className="mb-1 flex items-center gap-2">
        <span className="vd-label">session ready</span>
        <span className="h-px flex-1 bg-[var(--vd-line-soft)]" />
        <span className="vd-meta font-mono !text-[11px]">{folder}</span>
      </div>

      <h1 className="vd-display mt-5 text-[2.6rem] text-[var(--vd-text)]">What are we building?</h1>
      <p className="mt-2.5 max-w-[46ch] text-[var(--text-body)] leading-[1.6] text-[var(--vd-text-secondary)]">
        VibeDesk drives the Mistral Vibe agent over ACP. Ask for a change, paste a stack trace, or
        type <span className="font-mono text-[0.9em] text-[var(--vd-text)]">/</span> for commands
        and <span className="font-mono text-[0.9em] text-[var(--vd-text)]">@</span> to reference
        files.
      </p>

      <div className="mt-8 grid grid-cols-1 gap-2 sm:grid-cols-2">
        {SUGGESTIONS.map(({ title, prompt, icon: Icon }) => (
          <button
            key={title}
            type="button"
            onClick={() => setDraft(prompt)}
            className="vd-card vd-card-interactive group flex items-start gap-2.5 px-3 py-2.5 text-left"
          >
            <span className="mt-0.5 grid size-6 shrink-0 place-items-center rounded-[var(--radius-xs)] border border-[var(--vd-line)] bg-[var(--vd-surface-sunken)] text-[var(--vd-text-tertiary)] group-hover:text-[var(--vd-accent)]">
              <Icon size={12} strokeWidth={1.9} />
            </span>
            <span className="min-w-0">
              <span className="flex items-center gap-1 text-[12.5px] font-medium text-[var(--vd-text)]">
                {title}
                <ArrowUpRight
                  size={11}
                  strokeWidth={2}
                  className="opacity-0 transition-opacity group-hover:opacity-60"
                />
              </span>
              <span className="mt-0.5 line-clamp-2 block text-[11.5px] leading-[1.45] text-[var(--vd-text-tertiary)]">
                {prompt}
              </span>
            </span>
          </button>
        ))}
      </div>

      {environment && (
        <dl className="mt-8 grid grid-cols-2 gap-x-6 gap-y-2 border-t border-[var(--vd-line-soft)] pt-4 sm:grid-cols-4">
          <Fact label="model" value={environment.activeModel ?? 'server default'} />
          <Fact label="agent" value={environment.defaultAgent ?? 'accept-edits'} />
          <Fact label="plugins" value={String(pluginCount)} />
          <Fact label="vibe" value={environment.cliVersion ?? 'unknown'} />
        </dl>
      )}
    </div>
  );
}

function Fact({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0">
      <dt className="vd-label !text-[9.5px]">{label}</dt>
      <dd className="m-0 mt-0.5 truncate font-mono text-[11.5px] text-[var(--vd-text-secondary)]">
        {value}
      </dd>
    </div>
  );
}

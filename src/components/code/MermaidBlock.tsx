import { useEffect, useId, useRef, useState } from 'react';
import { AlertTriangle, Code2, Maximize2, Minimize2 } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useApp } from '@/store/app';

type MermaidApi = {
  initialize: (config: Record<string, unknown>) => void;
  render: (id: string, text: string) => Promise<{ svg: string }>;
  parse: (text: string) => Promise<unknown>;
};

let mermaidPromise: Promise<MermaidApi> | null = null;

async function loadMermaid(theme: 'ink' | 'paper'): Promise<MermaidApi> {
  if (!mermaidPromise) {
    mermaidPromise = import('mermaid').then((mod) => mod.default as unknown as MermaidApi);
  }
  const api = await mermaidPromise;
  api.initialize({
    startOnLoad: false,
    securityLevel: 'strict',
    theme: 'base',
    fontFamily: "'Instrument Sans', system-ui, sans-serif",
    themeVariables:
      theme === 'ink'
        ? {
            background: '#141715',
            primaryColor: '#1b201c',
            primaryTextColor: '#e8e6e0',
            primaryBorderColor: '#333a33',
            lineColor: '#6f766e',
            secondaryColor: '#191d1a',
            tertiaryColor: '#0e100f',
            mainBkg: '#1b201c',
            nodeBorder: '#333a33',
            clusterBkg: '#101211',
            clusterBorder: '#232823',
            titleColor: '#e8e6e0',
            edgeLabelBackground: '#141715',
            textColor: '#a4a9a1',
            fontSize: '12px',
          }
        : {
            background: '#ffffff',
            primaryColor: '#f7f5f0',
            primaryTextColor: '#1a1c19',
            primaryBorderColor: '#cbc5b6',
            lineColor: '#8b9088',
            secondaryColor: '#fbfaf6',
            tertiaryColor: '#efece4',
            mainBkg: '#f7f5f0',
            nodeBorder: '#cbc5b6',
            clusterBkg: '#fbfaf6',
            clusterBorder: '#e0dcd1',
            titleColor: '#1a1c19',
            edgeLabelBackground: '#ffffff',
            textColor: '#5b615a',
            fontSize: '12px',
          },
  });
  return api;
}

interface MermaidBlockProps {
  source: string;
  className?: string;
}

export function MermaidBlock({ source, className }: MermaidBlockProps) {
  const theme = useApp((s) => s.theme);
  const [svg, setSvg] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [showSource, setShowSource] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const reactId = useId().replace(/[:]/g, '');
  const renderSeq = useRef(0);

  useEffect(() => {
    let cancelled = false;
    const seq = ++renderSeq.current;
    setError(null);

    (async () => {
      try {
        const api = await loadMermaid(theme);
        const { svg: rendered } = await api.render(`vd-mermaid-${reactId}-${seq}`, source);
        if (!cancelled && seq === renderSeq.current) setSvg(rendered);
      } catch (err) {
        if (!cancelled && seq === renderSeq.current) {
          setSvg(null);
          setError(err instanceof Error ? err.message : 'Diagram could not be rendered');
        }
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [source, theme, reactId]);

  return (
    <figure className={cn('vd-card my-3 overflow-hidden', className)}>
      <figcaption className="flex h-9 items-center gap-2 border-b border-[var(--vd-line-soft)] px-3">
        <span className="vd-label !text-[10px] !tracking-[0.11em]">diagram</span>
        <span className="vd-meta !text-[11px]">mermaid</span>
        <span className="ml-auto flex items-center gap-0.5">
          <button
            type="button"
            onClick={() => setShowSource((v) => !v)}
            className={cn('vd-icon-btn !h-6 !w-6', showSource && 'text-[var(--vd-accent)]')}
            title="Toggle source"
          >
            <Code2 size={13} strokeWidth={1.75} />
          </button>
          <button
            type="button"
            onClick={() => setExpanded((v) => !v)}
            className="vd-icon-btn !h-6 !w-6"
            title={expanded ? 'Collapse' : 'Expand'}
          >
            {expanded ? (
              <Minimize2 size={13} strokeWidth={1.75} />
            ) : (
              <Maximize2 size={13} strokeWidth={1.75} />
            )}
          </button>
        </span>
      </figcaption>

      {error ? (
        <div className="flex items-start gap-2 px-3 py-3 text-[12px] text-[var(--vd-warn)]">
          <AlertTriangle size={14} strokeWidth={1.9} className="mt-0.5 shrink-0" />
          <div className="min-w-0">
            <p className="m-0 font-medium">Diagram syntax error</p>
            <p className="m-0 mt-0.5 break-words text-[var(--vd-text-tertiary)]">{error}</p>
          </div>
        </div>
      ) : (
        <div
          className={cn(
            'vd-diagram flex justify-center overflow-auto bg-[var(--vd-surface)] px-4 py-5',
            expanded ? 'max-h-none' : 'max-h-[420px]',
          )}
          dangerouslySetInnerHTML={svg ? { __html: svg } : undefined}
        >
          {!svg ? <div className="vd-skeleton h-24 w-full" /> : null}
        </div>
      )}

      {showSource && (
        <pre className="vd-code m-0 max-h-64 overflow-auto border-t border-[var(--vd-line-soft)] bg-[var(--vd-surface-sunken)] px-3 py-2 text-[11.5px] text-[var(--vd-text-secondary)]">
          {source}
        </pre>
      )}
    </figure>
  );
}

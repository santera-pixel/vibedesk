import { memo, type ReactNode } from 'react';
import ReactMarkdown, { type Components } from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { openUrl } from '@tauri-apps/plugin-opener';
import { CodeBlock } from '@/components/code/CodeBlock';
import { MermaidBlock } from '@/components/code/MermaidBlock';
import { DiffView } from '@/components/code/DiffView';
import { cn } from '@/lib/utils';
import { sanitizeSvg } from '@/lib/sanitize-svg';

interface MarkdownProps {
  children: string;
  className?: string;
  /** Appended caret while the message is still streaming. */
  streaming?: boolean;
}

const MERMAID_LANGS = new Set(['mermaid', 'mmd']);
const DIFF_LANGS = new Set(['diff', 'patch']);
const SVG_LANGS = new Set(['svg', 'xml-svg']);

function extractText(node: ReactNode): string {
  if (node === null || node === undefined || typeof node === 'boolean') return '';
  if (typeof node === 'string' || typeof node === 'number') return String(node);
  if (Array.isArray(node)) return node.map(extractText).join('');
  if (typeof node === 'object' && 'props' in (node as { props?: unknown })) {
    const props = (node as { props?: { children?: ReactNode } }).props;
    return extractText(props?.children);
  }
  return '';
}

const components: Components = {
  pre({ children }) {
    // CodeBlock owns its own <pre>; unwrap react-markdown's wrapper.
    return <>{children}</>;
  },

  code({ className, children, ...rest }) {
    const raw = extractText(children);
    const match = /language-([\w-]+)/.exec(className ?? '');
    const language = match?.[1]?.toLowerCase();
    const isBlock = Boolean(match) || raw.includes('\n');

    if (!isBlock) {
      return (
        <code className={className} {...rest}>
          {children}
        </code>
      );
    }

    if (language && MERMAID_LANGS.has(language)) {
      return <MermaidBlock source={raw.replace(/\n$/, '')} />;
    }

    if (language && DIFF_LANGS.has(language)) {
      return <DiffView patch={raw.replace(/\n$/, '')} />;
    }

    if (language && SVG_LANGS.has(language) && raw.trimStart().startsWith('<svg')) {
      return <SvgBlock source={raw} />;
    }

    return (
      <CodeBlock
        code={raw.replace(/\n$/, '')}
        language={language ?? 'text'}
        showLineNumbers={raw.split('\n').length > 6}
      />
    );
  },

  a({ href, children }) {
    return (
      <a
        href={href}
        onClick={(event) => {
          event.preventDefault();
          if (!href) return;
          if (href.startsWith('#')) return;
          void openUrl(href).catch(() => {
            window.open(href, '_blank', 'noopener');
          });
        }}
      >
        {children}
      </a>
    );
  },

  table({ children }) {
    return (
      <div className="my-3 overflow-x-auto rounded-[var(--radius-sm)] border border-[var(--vd-line)]">
        <table>{children}</table>
      </div>
    );
  },
};

function SvgBlock({ source }: { source: string }) {
  // The drawing is markup the model wrote, so it is rebuilt from an allowlist
  // before it is injected. See `src/lib/sanitize-svg.ts` for what survives and
  // why a link inside a drawing is the thing that matters.
  const safe = sanitizeSvg(source);
  return (
    <figure className="vd-card my-3 overflow-hidden">
      <figcaption className="flex h-9 items-center border-b border-[var(--vd-line-soft)] px-3">
        <span className="vd-label !text-[10px] !tracking-[0.11em]">vector</span>
        <span className="vd-meta ml-2 !text-[11px]">inline svg</span>
      </figcaption>
      {safe === null ? (
        <pre className="m-0 overflow-auto bg-[var(--vd-surface)] px-4 py-5 font-mono text-[11.5px] leading-[1.6] text-[var(--vd-text-secondary)]">
          {source}
        </pre>
      ) : (
        <div
          className="flex justify-center overflow-auto bg-[var(--vd-surface)] px-4 py-5 [&_svg]:max-w-full"
          dangerouslySetInnerHTML={{ __html: safe }}
        />
      )}
    </figure>
  );
}

export const Markdown = memo(function Markdown({ children, className, streaming }: MarkdownProps) {
  return (
    <div className={cn('vd-prose', streaming && 'vd-caret', className)}>
      <ReactMarkdown remarkPlugins={[remarkGfm]} components={components}>
        {children}
      </ReactMarkdown>
    </div>
  );
});

import { useEffect } from 'react';
import { Check, Info, X } from 'lucide-react';
import { cn } from '@/lib/utils';
import { useApp } from '@/store/app';

export function ToastHost() {
  const toast = useApp((s) => s.toast);
  const dismiss = useApp((s) => s.dismissToast);

  useEffect(() => {
    if (!toast) return;
    const handle = window.setTimeout(dismiss, 4200);
    return () => window.clearTimeout(handle);
  }, [toast, dismiss]);

  if (!toast) return null;

  const Icon = toast.tone === 'success' ? Check : toast.tone === 'danger' ? X : Info;

  return (
    <div className="pointer-events-none fixed bottom-12 left-1/2 z-50 -translate-x-1/2">
      <div
        className={cn(
          'vd-pop pointer-events-auto flex items-center gap-2 rounded-[var(--radius-md)] border bg-[var(--vd-overlay)] px-3 py-2 shadow-[var(--vd-shadow-pop)]',
          toast.tone === 'success'
            ? 'border-[color-mix(in_oklab,var(--vd-accent)_36%,transparent)]'
            : toast.tone === 'danger'
              ? 'border-[color-mix(in_oklab,var(--vd-danger)_36%,transparent)]'
              : 'border-[var(--vd-line-strong)]',
        )}
        role="status"
      >
        <Icon
          size={13}
          strokeWidth={2.2}
          className={cn(
            toast.tone === 'success'
              ? 'text-[var(--vd-accent)]'
              : toast.tone === 'danger'
                ? 'text-[var(--vd-danger)]'
                : 'text-[var(--vd-info)]',
          )}
        />
        <span className="text-[12px] text-[var(--vd-text)]">{toast.message}</span>
        <button type="button" onClick={dismiss} className="vd-icon-btn !h-5 !w-5" title="Dismiss">
          <X size={11} strokeWidth={2.2} />
        </button>
      </div>
    </div>
  );
}

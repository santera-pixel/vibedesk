/**
 * The last line of defence between a bad value and a black window.
 *
 * React unmounts the whole tree when a render throws, and this app had no
 * boundary: one malformed field from the backend — a null where a number was
 * expected, a date that does not parse — turned the entire cockpit into a black
 * rectangle with no message and no way back. The user's only recourse was to
 * kill the process.
 *
 * A boundary does not make the bug go away. It makes it survivable and legible:
 * the rest of the app keeps working, the message names what broke, and the
 * reload button is one click instead of one process kill.
 */
import { Component, type ErrorInfo, type ReactNode } from 'react';
import { CircleAlert, RotateCw } from 'lucide-react';

type Props = { children: ReactNode };
type State = { error: Error | null; where: string };

export class ErrorBoundary extends Component<Props, State> {
  state: State = { error: null, where: '' };

  static getDerivedStateFromError(error: Error): Partial<State> {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    // The console is the only place a release build can put this, and it is
    // where a user reporting a blank window will be asked to look.
    console.error('vibedesk: a view threw', error, info.componentStack);
    this.setState({ where: info.componentStack?.split('\n')[1]?.trim() ?? '' });
  }

  render(): ReactNode {
    const { error, where } = this.state;
    if (!error) return this.props.children;

    return (
      <div className="flex h-full w-full items-center justify-center bg-[var(--vd-bg)] p-8">
        <div className="max-w-[520px]">
          <div className="flex items-center gap-2">
            <CircleAlert size={16} strokeWidth={2} className="text-[var(--vd-danger)]" />
            <h1 className="m-0 text-[15px] font-semibold text-[var(--vd-text)]">
              This view stopped rendering
            </h1>
          </div>
          <p className="m-0 mt-2 text-[12.5px] leading-[1.6] text-[var(--vd-text-secondary)]">
            The rest of VibeDesk is still running, and so is any agent or swarm it is
            driving. Reloading rebuilds the view from the engine's state — nothing is
            lost.
          </p>
          <pre className="mt-3 max-h-[180px] overflow-auto rounded-[var(--radius-sm)] border border-[var(--vd-line)] bg-[var(--vd-surface)] p-2.5 text-[11px] leading-[1.5] text-[var(--vd-text-tertiary)]">
            {error.message}
            {where ? `\n\n${where}` : ''}
          </pre>
          <button
            type="button"
            onClick={() => window.location.reload()}
            className="mt-3 inline-flex items-center gap-1.5 rounded-[var(--radius-sm)] border border-[var(--vd-line)] bg-[var(--vd-surface)] px-2.5 py-1.5 text-[12px] text-[var(--vd-text)] transition-colors duration-[var(--vd-dur-1)] hover:bg-[var(--vd-surface-hover)]"
          >
            <RotateCw size={12} strokeWidth={2} />
            Reload the view
          </button>
        </div>
      </div>
    );
  }
}

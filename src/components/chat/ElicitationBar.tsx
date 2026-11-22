import { useEffect, useMemo, useRef, useState } from 'react';
import { Check, ChevronDown, ExternalLink, MessageCircleQuestion, X } from 'lucide-react';
import type { ElicitationProperty, JsonValue } from '@/types/acp';
import { cn } from '@/lib/utils';
import { interaction } from '@/lib/bridge';
import { useApp } from '@/store/app';
import { errorMessage } from '@/lib/actions';
import { ContextMenu, MenuItem, anchorUnder, type MenuAnchor } from '@/components/ui/ContextMenu';

type Values = Record<string, JsonValue>;

/**
 * The agent's `elicitation/create` request, rendered as a form.
 *
 * Without this the request sits unanswered until the 900s interaction timeout,
 * the turn never ends, and every later prompt fails with "A turn is already
 * running". Answering it is what keeps the session usable.
 */
export function ElicitationBar() {
  const request = useApp((s) => s.elicitation);
  const setElicitation = useApp((s) => s.setElicitation);

  const [values, setValues] = useState<Values>({});
  const [pending, setPending] = useState<'accept' | 'decline' | 'cancel' | null>(null);
  const [error, setError] = useState<string | null>(null);

  // A new request must not inherit the previous form's answers.
  const requestId = request?.requestId ?? null;
  useEffect(() => {
    setValues({});
    setError(null);
  }, [requestId]);

  const fields = useMemo(() => {
    const properties = request?.schema?.properties ?? {};
    return Object.entries(properties).map(([key, property]) => ({
      key,
      property,
      required: (request?.schema?.required ?? []).includes(key),
    }));
  }, [request]);

  if (!request) return null;

  async function respond(action: 'accept' | 'decline' | 'cancel') {
    setPending(action);
    setError(null);
    try {
      const content = action === 'accept' ? values : null;
      await interaction.respondElicitation(request!.requestId, action, content);
      setElicitation(null);
      setValues({});
    } catch (cause) {
      setError(errorMessage(cause));
    } finally {
      setPending(null);
    }
  }

  const missing = fields
    .filter(({ key, required }) => required && isEmpty(values[key]))
    .map(({ key }) => key);

  return (
    <div
      data-vd-protected="elicitation"
      className="vd-enter mx-auto mb-2 w-full max-w-[var(--vd-composer-max)] px-6"
    >
      <div className="overflow-hidden rounded-[var(--radius-md)] border border-[color-mix(in_oklab,var(--vd-info)_38%,transparent)] bg-[var(--vd-info-wash)]">
        <div className="flex items-start gap-2.5 px-3 py-2.5">
          <span className="mt-0.5 grid size-6 shrink-0 place-items-center rounded-[var(--radius-xs)] border border-[color-mix(in_oklab,var(--vd-info)_34%,transparent)] text-[var(--vd-info)]">
            <MessageCircleQuestion size={13} strokeWidth={2} />
          </span>
          <div className="min-w-0 flex-1">
            <p className="m-0 text-[12.5px] font-medium text-[var(--vd-text)]">
              The agent is asking
            </p>
            <p className="m-0 mt-0.5 text-[12px] leading-[1.5] text-[var(--vd-text-secondary)]">
              {request.message}
            </p>
          </div>
        </div>

        {request.mode === 'url' ? (
          <div className="border-t border-[color-mix(in_oklab,var(--vd-info)_24%,transparent)] px-3 py-2.5">
            <p className="m-0 text-[11.5px] text-[var(--vd-text-secondary)]">
              This one needs a browser.
            </p>
            {request.url && (
              <button
                type="button"
                onClick={() => {
                  // The opener can refuse a URL, and a bare `void` would turn
                  // that into an unhandled rejection with nothing on screen.
                  void openUrl(request.url!).catch((cause) => setError(errorMessage(cause)));
                }}
                className="vd-btn mt-2 !h-[26px]"
              >
                <ExternalLink size={11} strokeWidth={2} />
                Open the page
              </button>
            )}
          </div>
        ) : fields.length > 0 ? (
          <div className="space-y-2.5 border-t border-[color-mix(in_oklab,var(--vd-info)_24%,transparent)] px-3 py-2.5">
            {fields.map(({ key, property, required }) => (
              <Field
                key={key}
                name={key}
                property={property}
                required={required}
                value={values[key]}
                onChange={(value) => setValues((prev) => ({ ...prev, [key]: value }))}
              />
            ))}
          </div>
        ) : null}

        {error && (
          <p className="m-0 border-t border-[color-mix(in_oklab,var(--vd-danger)_30%,transparent)] px-3 py-1.5 text-[11px] text-[var(--vd-danger)]">
            {error}
          </p>
        )}

        <div className="flex flex-wrap items-center gap-1.5 border-t border-[color-mix(in_oklab,var(--vd-info)_24%,transparent)] px-3 py-2">
          <button
            type="button"
            onClick={() => void respond('accept')}
            disabled={pending !== null || missing.length > 0}
            className="vd-btn vd-btn-primary !h-[26px]"
            title={missing.length > 0 ? `Fill in: ${missing.join(', ')}` : undefined}
          >
            <Check size={11} strokeWidth={2.6} />
            {pending === 'accept' ? 'Sending…' : 'Send'}
          </button>
          <button
            type="button"
            onClick={() => void respond('decline')}
            disabled={pending !== null}
            className="vd-btn !h-[26px]"
          >
            Decline
          </button>
          <button
            type="button"
            onClick={() => void respond('cancel')}
            disabled={pending !== null}
            className="vd-btn vd-btn-ghost !h-[26px]"
          >
            <X size={11} strokeWidth={2.4} />
            Cancel turn
          </button>
          <span className="ml-auto text-[10.5px] text-[var(--vd-text-tertiary)]">
            {missing.length > 0
              ? `required: ${missing.join(', ')}`
              : 'the agent is paused until you answer'}
          </span>
        </div>
      </div>
    </div>
  );
}

function isEmpty(value: JsonValue | undefined): boolean {
  if (value === undefined || value === null) return true;
  if (typeof value === 'string') return value.trim() === '';
  if (Array.isArray(value)) return value.length === 0;
  return false;
}

function label(property: ElicitationProperty, name: string): string {
  return property.title?.trim() || name;
}

/**
 * One choice out of a closed list.
 *
 * This was a `<select>`, and the control was the only themed part of it: the
 * list that opens is drawn by the platform, so on Windows it arrived as a white
 * rectangle with the system font, hanging out of the card and over the composer.
 * A form that asks the user a question cannot have one element that ignores the
 * theme it sits in.
 *
 * So the closed control is a button and the open list is the app's own menu,
 * which is portalled into `body` — it cannot be clipped by the card — and
 * already carries the keyboard behaviour a picker needs: arrows, Home, End,
 * Enter, Escape, and focus going back to the trigger. The menu is a menu: it is
 * not a tab stop, so `aria-haspopup` and `aria-expanded` describe it rather than
 * pretending it is a native select's popup.
 */
function Choice({
  value,
  options,
  onChange,
}: {
  value: string;
  options: { value: string; title: string }[];
  onChange: (value: string) => void;
}) {
  const trigger = useRef<HTMLButtonElement>(null);
  const [anchor, setAnchor] = useState<MenuAnchor | null>(null);
  const current = options.find((option) => option.value === value);
  const chosen = value !== '';

  const open = () => {
    if (trigger.current) setAnchor(anchorUnder(trigger.current, options.length));
  };

  return (
    <>
      <button
        ref={trigger}
        type="button"
        aria-haspopup="menu"
        aria-expanded={anchor !== null}
        onClick={open}
        onKeyDown={(event) => {
          if (event.key === 'Enter' || event.key === ' ' || event.key === 'ArrowDown') {
            event.preventDefault();
            open();
          }
        }}
        className={cn(
          'vd-input flex w-full items-center gap-1.5 !h-[28px] !text-[11.5px] text-left',
          !chosen && 'text-[var(--vd-text-tertiary)]',
        )}
      >
        <span className="min-w-0 flex-1 truncate">{current?.title ?? 'Choose…'}</span>
        <ChevronDown size={11} strokeWidth={2} className="shrink-0 opacity-60" />
      </button>

      {anchor && (
        <ContextMenu anchor={anchor} onClose={() => setAnchor(null)}>
          {options.map((option) => (
            <MenuItem
              key={option.value}
              label={option.title}
              icon={option.value === value ? Check : undefined}
              onSelect={() => {
                onChange(option.value);
                setAnchor(null);
              }}
            />
          ))}
        </ContextMenu>
      )}
    </>
  );
}

function Field({
  name,
  property,
  required,
  value,
  onChange,
}: {
  name: string;
  property: ElicitationProperty;
  required: boolean;
  value: JsonValue | undefined;
  onChange: (value: JsonValue) => void;
}) {
  const text = typeof value === 'string' ? value : '';
  const options = enumOptions(property);

  return (
    <div>
      <label className="flex items-baseline gap-1.5">
        <span className="text-[11.5px] font-medium text-[var(--vd-text)]">
          {label(property, name)}
        </span>
        {required && <span className="text-[10px] text-[var(--vd-danger)]">required</span>}
      </label>
      {property.description && (
        <p className="m-0 mt-0.5 text-[10.5px] leading-[1.45] text-[var(--vd-text-tertiary)]">
          {property.description}
        </p>
      )}

      <div className="mt-1">
        {options ? (
          <Choice
            value={text}
            options={[{ value: '', title: 'Choose…' }, ...options]}
            onChange={onChange}
          />
        ) : property.type === 'boolean' ? (
          <button
            type="button"
            onClick={() => onChange(!(value === true))}
            className={cn(
              'vd-btn !h-[26px]',
              value === true && 'vd-btn-primary',
            )}
          >
            {value === true ? 'Yes' : 'No'}
          </button>
        ) : property.type === 'array' ? (
          <MultiSelect
            property={property}
            value={Array.isArray(value) ? (value as JsonValue[]) : []}
            onChange={onChange}
          />
        ) : property.type === 'number' || property.type === 'integer' ? (
          <input
            type="number"
            value={typeof value === 'number' ? value : ''}
            min={property.minimum ?? undefined}
            max={property.maximum ?? undefined}
            step={property.type === 'integer' ? 1 : 'any'}
            onChange={(event) =>
              onChange(event.target.value === '' ? null : Number(event.target.value))
            }
            className="vd-input !h-[28px] w-full !text-[11.5px]"
          />
        ) : (
          <input
            type="text"
            value={text}
            minLength={property.minLength ?? undefined}
            maxLength={property.maxLength ?? undefined}
            onChange={(event) => onChange(event.target.value)}
            className="vd-input !h-[28px] w-full !text-[11.5px]"
          />
        )}
      </div>
    </div>
  );
}

function MultiSelect({
  property,
  value,
  onChange,
}: {
  property: ElicitationProperty;
  value: JsonValue[];
  onChange: (value: JsonValue) => void;
}) {
  const options = enumOptions(property.items ?? { type: 'string' });
  const selected = value.filter((item): item is string => typeof item === 'string');

  if (!options) {
    return (
      <input
        type="text"
        value={selected.join(', ')}
        onChange={(event) =>
          onChange(
            event.target.value
              .split(',')
              .map((part) => part.trim())
              .filter(Boolean),
          )
        }
        placeholder="comma separated"
        className="vd-input !h-[28px] w-full !text-[11.5px]"
      />
    );
  }

  return (
    <div className="flex flex-wrap gap-1">
      {options.map((option) => {
        const active = selected.includes(option.value);
        return (
          <button
            key={option.value}
            type="button"
            onClick={() =>
              onChange(
                active
                  ? selected.filter((item) => item !== option.value)
                  : [...selected, option.value],
              )
            }
            className={cn('vd-chip !h-[22px]', active && 'vd-chip-accent')}
          >
            {option.title}
          </button>
        );
      })}
    </div>
  );
}

function enumOptions(property: {
  enum?: string[] | null;
  oneOf?: { const?: string; title?: string }[] | null;
  anyOf?: { const?: string; title?: string }[] | null;
}): { value: string; title: string }[] | null {
  if (property.enum && property.enum.length > 0) {
    return property.enum.map((value) => ({ value, title: value }));
  }
  const titled = property.oneOf ?? property.anyOf;
  if (titled && titled.length > 0) {
    return titled
      .filter((option): option is { const: string; title?: string } => typeof option.const === 'string')
      .map((option) => ({ value: option.const, title: option.title ?? option.const }));
  }
  return null;
}

function openUrl(url: string) {
  return import('@tauri-apps/plugin-opener').then((m) => m.openUrl(url));
}

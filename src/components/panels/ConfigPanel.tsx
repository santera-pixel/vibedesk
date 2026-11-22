import { useState } from 'react';
import { AlertTriangle, Check, ExternalLink, RefreshCw, Server, Webhook } from 'lucide-react';
import type { ConfigOption } from '@/types/acp';
import { cn } from '@/lib/utils';
import { session as sessionApi, workspace } from '@/lib/bridge';
import { errorMessage, sendPrompt } from '@/lib/actions';
import { useApp } from '@/store/app';
import { Drawer, Empty, KeyValue, Section } from '@/components/chrome/Drawer';

export function ConfigPanel() {
  const environment = useApp((s) => s.environment);
  const configOptions = useApp((s) => s.configOptions);
  const sessionId = useApp((s) => s.sessionId);
  const setConfigOptions = useApp((s) => s.setConfigOptions);
  const notify = useApp((s) => s.notify);
  const addNotice = useApp((s) => s.addNotice);
  const [busy, setBusy] = useState(false);

  async function updateOption(option: ConfigOption, value: string | boolean) {
    if (!sessionId) {
      notify('Start a session before changing options.', 'neutral');
      return;
    }
    try {
      await sessionApi.setConfigOption(sessionId, option.id, value);
      setConfigOptions(
        configOptions.map((o) => (o.id === option.id ? { ...o, currentValue: value } : o)),
      );
    } catch (error) {
      addNotice(errorMessage(error), 'error');
    }
  }

  async function reloadConfig() {
    setBusy(true);
    try {
      await sendPrompt('/reload');
      notify('Configuration reloaded from disk.', 'success');
    } catch (error) {
      // `sendPrompt` owns its own errors, but a throw here would otherwise be an
      // unhandled rejection: this is an async click handler, and React does not
      // catch one.
      addNotice(errorMessage(error), 'error');
    } finally {
      setBusy(false);
    }
  }

  return (
    <Drawer
      title="Configuration"
      subtitle={environment?.configPath ?? '~/.vibe/config.toml'}
      actions={
        <>
          <button
            type="button"
            onClick={reloadConfig}
            disabled={busy}
            className="vd-btn vd-btn-ghost !h-[24px] !px-1.5"
            title="Reload configuration, agent instructions and skills"
          >
            <RefreshCw size={12} strokeWidth={2} className={cn(busy && 'vd-spin')} />
          </button>
          <button
            type="button"
            onClick={() => {
              if (environment?.configPath) {
                void workspace.reveal(environment.configPath).catch((error) => {
                  addNotice(errorMessage(error), 'error');
                });
              }
            }}
            className="vd-btn vd-btn-ghost !h-[24px] !px-1.5"
            title="Reveal config.toml"
          >
            <ExternalLink size={12} strokeWidth={2} />
          </button>
        </>
      }
    >
      <Section label="Session options">
        {configOptions.length === 0 ? (
          <Empty>Session options appear once the agent reports its configuration.</Empty>
        ) : (
          <div className="flex flex-col gap-2">
            {configOptions.map((option) => (
              <div key={option.id}>
                <div className="flex items-center gap-2">
                  <span className="text-[11.5px] font-medium text-[var(--vd-text)]">
                    {option.name}
                  </span>
                  {option.type === 'boolean' ? (
                    <button
                      type="button"
                      className="vd-switch ml-auto"
                      data-on={option.currentValue === true}
                      onClick={() => void updateOption(option, option.currentValue !== true)}
                      aria-pressed={option.currentValue === true}
                    />
                  ) : null}
                </div>
                {option.description && (
                  <p className="m-0 mt-0.5 text-[10.5px] leading-[1.4] text-[var(--vd-text-tertiary)]">
                    {option.description}
                  </p>
                )}
                {option.type !== 'boolean' && option.options && (
                  <div className="mt-1 flex flex-wrap gap-1">
                    {option.options.map((value) => {
                      const active = option.currentValue === value.value;
                      return (
                        <button
                          key={value.value}
                          type="button"
                          onClick={() => void updateOption(option, value.value)}
                          className={cn('vd-chip ', active && 'vd-chip-accent')}
                          title={value.description ?? value.name}
                        >
                          {active && <Check size={9} strokeWidth={3} />}
                          {value.name}
                        </button>
                      );
                    })}
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </Section>

      <Section label="Providers">
        {environment?.providers.length ? (
          <div className="flex flex-col gap-1.5">
            {environment.providers.map((provider) => (
              <div key={provider.name} className="vd-card px-2.5 py-1.5">
                <div className="flex items-center gap-2">
                  <span className="text-[11.5px] font-medium text-[var(--vd-text)]">
                    {provider.name}
                  </span>
                  <span className="vd-chip !h-[17px] !text-[9px]">{provider.backend}</span>
                  <span
                    className={cn(
                      'vd-chip ml-auto !h-[17px] !text-[9px]',
                      provider.hasKey ? 'vd-chip-accent' : 'vd-chip-warn',
                    )}
                  >
                    {provider.hasKey ? 'key set' : 'no key'}
                  </span>
                </div>
                <p className="m-0 mt-0.5 truncate font-mono text-[10px] text-[var(--vd-text-tertiary)]">
                  {provider.apiBase}
                </p>
              </div>
            ))}
          </div>
        ) : (
          <Empty>No providers configured — Vibe falls back to the Mistral default.</Empty>
        )}
      </Section>

      <Section label="Models">
        {environment?.models.length ? (
          <div className="flex flex-col gap-1">
            {environment.models.map((model) => (
              <div key={`${model.provider}:${model.name}`} className="flex items-baseline gap-2">
                <span className="font-mono text-[11px] text-[var(--vd-text)]">{model.alias}</span>
                <span className="truncate text-[10px] text-[var(--vd-text-tertiary)]">
                  {model.name}
                </span>
                {model.thinking && (
                  <span className="vd-chip ml-auto !h-[16px] !text-[8.5px]">{model.thinking}</span>
                )}
              </div>
            ))}
          </div>
        ) : (
          <Empty>No models declared in config.toml.</Empty>
        )}
      </Section>

      <Section label="MCP servers">
        {environment?.mcpServers.length ? (
          <div className="flex flex-col gap-1.5">
            {environment.mcpServers.map((server) => (
              <div key={server.name} className="flex items-center gap-2">
                <Server
                  size={11}
                  strokeWidth={2}
                  className="shrink-0 text-[var(--vd-text-tertiary)]"
                />
                <span className="text-[11.5px] text-[var(--vd-text)]">{server.name}</span>
                <span className="vd-chip !h-[16px] !text-[8.5px]">{server.transport}</span>
                <span className="ml-auto truncate font-mono text-[10px] text-[var(--vd-text-tertiary)]">
                  {server.auth}
                </span>
              </div>
            ))}
          </div>
        ) : (
          <Empty>No MCP servers configured.</Empty>
        )}
      </Section>

      <Section label="Hooks">
        {environment?.hooks.length ? (
          <div className="flex flex-col gap-1.5">
            {environment.hooks.map((hook) => (
              <div key={`${hook.source}:${hook.name}`} className="vd-card px-2.5 py-1.5">
                <div className="flex items-center gap-2">
                  <Webhook
                    size={11}
                    strokeWidth={2}
                    className="shrink-0 text-[var(--vd-text-tertiary)]"
                  />
                  <span className="text-[11.5px] text-[var(--vd-text)]">{hook.name}</span>
                  <span className="vd-chip !h-[16px] !text-[8.5px]">{hook.event}</span>
                  <span className="vd-chip ml-auto !h-[16px] !text-[8.5px]">{hook.scope}</span>
                </div>
                <p className="m-0 mt-0.5 truncate font-mono text-[10px] text-[var(--vd-text-tertiary)]">
                  {hook.command}
                </p>
              </div>
            ))}
          </div>
        ) : (
          <Empty>No hooks declared in hooks.toml.</Empty>
        )}
      </Section>

      <Section label="Environment">
        <KeyValue label="vibe home" value={environment?.vibeHome ?? '—'} />
        <KeyValue label="config" value={environment?.configPath ?? '—'} />
        <KeyValue label="hooks" value={environment?.hooksPath ?? '—'} />
        <KeyValue label="acp binary" value={environment?.acpBinary ?? 'not found'} />
        <KeyValue label="resolved via" value={environment?.acpBinarySource ?? '—'} />
        <KeyValue label="api key" value={environment?.apiKeyPresent ? 'present' : 'missing'} />
      </Section>

      {environment?.configIssues.length ? (
        <Section label="Config issues">
          <ul className="m-0 list-none space-y-1 p-0">
            {environment.configIssues.map((issue) => (
              <li
                key={issue}
                className="flex items-start gap-1.5 text-[10.5px] leading-[1.4] text-[var(--vd-warn)]"
              >
                <AlertTriangle size={10} strokeWidth={2} className="mt-0.5 shrink-0" />
                {issue}
              </li>
            ))}
          </ul>
        </Section>
      ) : null}
    </Drawer>
  );
}

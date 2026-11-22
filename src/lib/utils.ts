import { clsx, type ClassValue } from 'clsx';
import { twMerge } from 'tailwind-merge';

export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}

export function formatTokens(n: number): string {
  if (!Number.isFinite(n) || n <= 0) return '0';
  if (n < 1000) return String(n);
  if (n < 1_000_000) return `${(n / 1000).toFixed(n < 10_000 ? 1 : 0)}k`;
  return `${(n / 1_000_000).toFixed(1)}M`;
}

export function formatCost(amount: number, currency = 'USD'): string {
  const symbol = currency === 'USD' ? '$' : `${currency} `;
  if (amount === 0) return `${symbol}0.00`;
  if (amount < 0.01) return `${symbol}${amount.toFixed(4)}`;
  return `${symbol}${amount.toFixed(2)}`;
}

export function relativeTime(iso: string | null | undefined): string {
  if (!iso) return 'never';
  const then = new Date(iso).getTime();
  if (Number.isNaN(then)) return 'unknown';
  const delta = Date.now() - then;
  const mins = Math.round(delta / 60_000);
  if (mins < 1) return 'just now';
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.round(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  if (days < 30) return `${days}d ago`;
  return new Date(iso).toLocaleDateString();
}

export function basename(path: string): string {
  const parts = path.split(/[\\/]/).filter(Boolean);
  return parts[parts.length - 1] ?? path;
}

export function truncateMiddle(value: string, max = 48): string {
  if (value.length <= max) return value;
  const head = Math.ceil((max - 1) / 2);
  const tail = Math.floor((max - 1) / 2);
  return `${value.slice(0, head)}…${value.slice(value.length - tail)}`;
}

/**
 * The agent titles a session from its first user message, and the first message
 * carries the VibeDesk preamble. Without this the rail shows a chat called
 * "Reply with exactly: PONG [VibeDesk] You are running inside VibeDesk, a
 * desktop client for the Mistral Vibe agent…". The stored title is left alone;
 * only what the rail prints is trimmed.
 */
export function chatTitle(title: string | null | undefined): string {
  const raw = (title ?? '').trim();
  if (!raw) return 'Untitled chat';
  const cut = raw.indexOf('[VibeDesk]');
  if (cut === 0) return 'Untitled chat';
  const head = (cut > 0 ? raw.slice(0, cut) : raw).trim();
  return head || 'Untitled chat';
}

export function uid(prefix = 'id'): string {
  return `${prefix}-${Math.random().toString(36).slice(2, 10)}`;
}

export function clamp(value: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, value));
}

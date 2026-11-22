import hljs from 'highlight.js/lib/core';

import bash from 'highlight.js/lib/languages/bash';
import c from 'highlight.js/lib/languages/c';
import cpp from 'highlight.js/lib/languages/cpp';
import csharp from 'highlight.js/lib/languages/csharp';
import css from 'highlight.js/lib/languages/css';
import diff from 'highlight.js/lib/languages/diff';
import dockerfile from 'highlight.js/lib/languages/dockerfile';
import go from 'highlight.js/lib/languages/go';
import graphql from 'highlight.js/lib/languages/graphql';
import ini from 'highlight.js/lib/languages/ini';
import java from 'highlight.js/lib/languages/java';
import javascript from 'highlight.js/lib/languages/javascript';
import json from 'highlight.js/lib/languages/json';
import kotlin from 'highlight.js/lib/languages/kotlin';
import less from 'highlight.js/lib/languages/less';
import lua from 'highlight.js/lib/languages/lua';
import markdown from 'highlight.js/lib/languages/markdown';
import php from 'highlight.js/lib/languages/php';
import plaintext from 'highlight.js/lib/languages/plaintext';
import powershell from 'highlight.js/lib/languages/powershell';
import python from 'highlight.js/lib/languages/python';
import ruby from 'highlight.js/lib/languages/ruby';
import rust from 'highlight.js/lib/languages/rust';
import scss from 'highlight.js/lib/languages/scss';
import shell from 'highlight.js/lib/languages/shell';
import sql from 'highlight.js/lib/languages/sql';
import swift from 'highlight.js/lib/languages/swift';
import typescript from 'highlight.js/lib/languages/typescript';
import xml from 'highlight.js/lib/languages/xml';
import yaml from 'highlight.js/lib/languages/yaml';

const LANGUAGES: Record<string, Parameters<typeof hljs.registerLanguage>[1]> = {
  bash,
  c,
  cpp,
  csharp,
  css,
  diff,
  dockerfile,
  go,
  graphql,
  ini,
  java,
  javascript,
  json,
  kotlin,
  less,
  lua,
  markdown,
  php,
  plaintext,
  powershell,
  python,
  ruby,
  rust,
  scss,
  shell,
  sql,
  swift,
  typescript,
  xml,
  yaml,
};

let registered = false;

function ensureRegistered(): void {
  if (registered) return;
  for (const [name, def] of Object.entries(LANGUAGES)) {
    hljs.registerLanguage(name, def);
  }
  hljs.registerAliases(['ts', 'tsx'], { languageName: 'typescript' });
  hljs.registerAliases(['js', 'jsx', 'mjs', 'cjs'], { languageName: 'javascript' });
  hljs.registerAliases(['py'], { languageName: 'python' });
  hljs.registerAliases(['rs'], { languageName: 'rust' });
  hljs.registerAliases(['sh', 'zsh', 'console', 'shellsession'], { languageName: 'bash' });
  hljs.registerAliases(['yml'], { languageName: 'yaml' });
  hljs.registerAliases(['toml'], { languageName: 'ini' });
  hljs.registerAliases(['html', 'svg', 'vue'], { languageName: 'xml' });
  hljs.registerAliases(['md'], { languageName: 'markdown' });
  hljs.registerAliases(['text', 'txt', 'output'], { languageName: 'plaintext' });
  registered = true;
}

export function highlight(code: string, language?: string): string {
  ensureRegistered();
  const lang = (language ?? '').toLowerCase().trim();
  if (lang && hljs.getLanguage(lang)) {
    try {
      return hljs.highlight(code, { language: lang, ignoreIllegals: true }).value;
    } catch {
      /* fall through to plain text */
    }
  }
  return escapeHtml(code);
}

export function escapeHtml(value: string): string {
  return value
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;')
    .replace(/>/g, '&gt;')
    .replace(/"/g, '&quot;');
}

const EXTENSION_MAP: Record<string, string> = {
  ts: 'typescript',
  tsx: 'tsx',
  js: 'javascript',
  jsx: 'jsx',
  mjs: 'javascript',
  cjs: 'javascript',
  rs: 'rust',
  py: 'python',
  rb: 'ruby',
  go: 'go',
  java: 'java',
  kt: 'kotlin',
  swift: 'swift',
  c: 'c',
  h: 'c',
  cpp: 'cpp',
  hpp: 'cpp',
  cs: 'csharp',
  php: 'php',
  sh: 'bash',
  bash: 'bash',
  zsh: 'bash',
  ps1: 'powershell',
  json: 'json',
  jsonc: 'json',
  yaml: 'yaml',
  yml: 'yaml',
  toml: 'toml',
  ini: 'ini',
  md: 'markdown',
  mdx: 'markdown',
  css: 'css',
  scss: 'scss',
  less: 'less',
  html: 'html',
  xml: 'xml',
  svg: 'xml',
  sql: 'sql',
  graphql: 'graphql',
  gql: 'graphql',
  lua: 'lua',
  dockerfile: 'dockerfile',
  diff: 'diff',
  patch: 'diff',
};

export function languageFromPath(path: string): string {
  const name = path.split(/[\\/]/).pop() ?? '';
  if (/^dockerfile$/i.test(name)) return 'dockerfile';
  const ext = name.includes('.') ? name.split('.').pop()!.toLowerCase() : '';
  return EXTENSION_MAP[ext] ?? 'plaintext';
}

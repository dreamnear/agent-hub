import type { ReactElement } from 'react';
import { useEffect, useState } from 'react';
import { plugins } from '../plugins';
import { decoExt, useDeco } from '../lib/fileDeco';

/// Material Icon Theme 风格的精选子集文件图标（Git 变更列表用，工单限定范围仅 GitPanel）。
/// 色彩例外依据（同驱动徽标 C/O/P/D 先例）：文件类型配色是图标本体艺术——
/// SVG 内固定 fill 不随三态主题，不进 --c-* 体系（本组件头注释即门禁例外记录；
/// bui 门禁只扫 components/bui/，本组件不在其域）。
/// 体量控制：类型色圆角方块 + 白色扩展名缩写（16×16 手写极简），全子集 <3KB gz；
/// 不打包完整 material-icon-theme（数百 SVG 会爆预算）。

/// 类型色（Material Icon Theme 语言色系近似值）
const COLOR: Record<string, string> = {
  md: '#519aba',
  json: '#cbcb41',
  ts: '#3178c6',
  tsx: '#3178c6',
  js: '#f1e05a',
  mjs: '#f1e05a',
  rs: '#dea584',
  go: '#00add8',
  py: '#3572a5',
  java: '#b07219',
  yml: '#cb171e',
  yaml: '#cb171e',
  toml: '#9c4121',
  html: '#e34c26',
  css: '#563d7c',
  scss: '#c6538c',
  png: '#a074c4',
  jpg: '#a074c4',
  jpeg: '#a074c4',
  gif: '#a074c4',
  svg: '#ffb13b',
  webp: '#a074c4',
  lock: '#6d8086',
  sh: '#89e051',
  env: '#89e051',
  gitignore: '#e34c26',
  license: '#6d8086',
  txt: '#6d8086',
  file: '#6d8086',
};

/// 白色缩写标签（≤3 字符保 16px 可读；json 用花括号形体）
const LABEL: Record<string, string> = {
  md: 'MD',
  json: '{ }',
  ts: 'TS',
  tsx: 'TSX',
  js: 'JS',
  mjs: 'MJS',
  rs: 'RS',
  go: 'GO',
  py: 'PY',
  java: 'JV',
  yml: 'YML',
  yaml: 'YML',
  toml: 'TML',
  html: '<>',
  css: '#',
  scss: 'SC',
  png: 'PNG',
  jpg: 'JPG',
  jpeg: 'JPG',
  gif: 'GIF',
  svg: 'SVG',
  webp: 'WEB',
  lock: 'LCK',
  sh: 'SH',
  env: 'ENV',
  gitignore: 'GIT',
  license: 'LIC',
  txt: 'TXT',
};

/** 文件名 → 图标键：取小写扩展名；点开头取点名（.env/.gitignore）；LICENSE 全名
 * 特判；不在子集 → 'file' 兜底（通用文件形）。 */
export function extFor(name: string): string {
  const base = (name.split('/').pop() ?? name).toLowerCase();
  if (base === 'license' || base === 'licence') return 'license';
  if (base.startsWith('.')) {
    const dotName = base.slice(1);
    return dotName in COLOR ? dotName : 'file';
  }
  const dot = base.lastIndexOf('.');
  if (dot < 0) return 'file';
  const ext = base.slice(dot + 1);
  return ext in COLOR ? ext : 'file';
}

function PluginDecoIcon({ pluginId, icon }: { pluginId: string; icon: string }): ReactElement {
  const [ticket, setTicket] = useState<string | null>(null);
  useEffect(() => {
    let alive = true;
    void plugins
      .viewTicket(pluginId)
      .then((t) => {
        if (alive) setTicket((t as { ticket: string }).ticket ?? null);
      })
      .catch(() => {});
    return () => {
      alive = false;
    };
  }, [pluginId]);
  if (ticket == null) return <FallbackIcon ext="file" />;
  return (
    <img
      className="file-icon"
      width="14"
      height="14"
      src={`/plugin-view/${encodeURIComponent(pluginId)}/~${ticket}/${icon}`}
      alt="file"
      decoding="async"
    />
  );
}

function FallbackIcon({ ext }: { ext: string }): ReactElement {
  const key = ext in COLOR ? ext : 'file';
  const color = COLOR[key] ?? COLOR.file;
  return (
    <svg className="file-icon" width="14" height="14" viewBox="0 0 16 16" aria-hidden="true" data-icon={key}>
      <rect x="1" y="1" width="14" height="14" rx="3" fill={color} />
      {key === 'file' ? (
        <path
          d="M5.5 3h3.2L11.5 5.8V12a1 1 0 0 1-1 1h-5a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z"
          fill="#fff"
          opacity="0.92"
        />
      ) : (
        <text
          x="8"
          y="10.7"
          textAnchor="middle"
          fontSize="5.5"
          fontFamily="ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"
          fontWeight="700"
          fill="#fff"
        >
          {LABEL[key] ?? ''}
        </text>
      )}
    </svg>
  );
}

/// 类型色方块 + 白色缩写（file 兜底 = 白描页形，无文字）。
/// B3 装饰命中时走插件素材（<img> 票据路由，禁用 inline——ADR-0013 拍板 4）。
export default function FileIcon({ name }: { name: string }): ReactElement {
  const ext = decoExt(name);
  const d = useDeco(ext);
  if (d != null) return <PluginDecoIcon pluginId={d.pluginId} icon={d.icon} />;
  const key = extFor(name);
  const color = COLOR[key] ?? COLOR.file;
  return (
    <svg
      className="file-icon"
      width="14"
      height="14"
      viewBox="0 0 16 16"
      aria-hidden="true"
      data-icon={key}
    >
      <rect x="1" y="1" width="14" height="14" rx="3" fill={color} />
      {key === 'file' ? (
        <path
          d="M5.5 3h3.2L11.5 5.8V12a1 1 0 0 1-1 1h-5a1 1 0 0 1-1-1V4a1 1 0 0 1 1-1z"
          fill="#fff"
          opacity="0.92"
        />
      ) : (
        <text
          x="8"
          y="10.7"
          textAnchor="middle"
          fontSize="5.5"
          fontFamily="ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"
          fontWeight="700"
          fill="#fff"
        >
          {LABEL[key] ?? ''}
        </text>
      )}
    </svg>
  );
}

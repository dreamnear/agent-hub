# web

仅开发用。`npm run build` 产物归 server 静态托管。

## Beautiful UI 组件体系（批1 起）

组件源：beautifului.dev（Shane Levine / Turbo，MIT），copy-paste 模式逐组件移植，
版权与依赖归属见 [NOTICE](./NOTICE)。

- `src/bui/theme.css`：Tailwind `--color-*` → 本工程 `--c-*` token 的唯一色值入口；
  `@custom-variant dark` 对齐 useTheme 的 `data-theme` 机制。bui/ 组件禁硬编码色值。
- `src/bui/bui.css`：Tailwind v4 无 preflight 分层引入（theme + utilities 两层，
  手写 CSS 恒优先），preflight 不进产物。
- `src/components/bui/`：移植组件集中目录，文件名与官网一致，文件头标来源注释。

## bundle 体积基线（Beautiful UI 批1，2026-09-22，vite build）

| 产物 | 原始 | gzip |
|---|---|---|
| JS（app 页全部） | 841.48 kB | **243.01 kB**（预算 <300KB ✓） |
| CSS（含 Tailwind theme/utilities 层） | 44.96 kB | **9.47 kB**（批1 前 8.96 kB，+0.51 kB） |

批2 起每批对比本基线；Motion 引入后重点盯 JS 增量。

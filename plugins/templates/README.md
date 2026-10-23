# 插件模板体系（ADR-0015）

母本目录：`plugins/templates/<profile>/`。v1 交付 `hook-observer`（B4，ADR-0014
观察档全样）；后续 profile 随扩展点补齐。`{{…}}` 为占位符（复制后手工替换；
`server plugin init` 脚手架命令属 ADR-0015 的机械面，尚未交付——模板即样例即
测试：每个 profile 必须过静态审计（零 errors），见 `crates/server/src/plugins/mod.rs`
的模板审计单测。

## 渲染页/视图资产外链要求（适用于一切模板与插件）

- 视图与 plugin-iframe 渲染页运行在 sandbox iframe：CSP `connect-src 'none'` +
  `script-src 'self'`——**禁止外部 URL 资源**，资产一律随插件目录自持；
- 网络访问只经宿主代理（`hub.rpc` → `POST /api/plugins/{id}/rpc`，权限判定在 hub）；
- CSS 色值走宿主 `--c-*` token（`hub:init` / `hub:theme` 下发），散值会进审计 warning。

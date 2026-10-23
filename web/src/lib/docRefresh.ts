/// 文件面板自动刷新周期（agent-hub-docs-tree-refresh #36 主修）。
///
/// 文档树对已展开目录、打开中文件在面板/页签挂载期按此周期重取——
/// 与插件导航 10s 轮询模式同族（15-30s 取 20s）；不引入后端文件监视器。
/// 单一事实源：FilesTree 周期重取与 FileContentView refetchInterval 共用。
export const DOC_REFRESH_MS = 20_000;

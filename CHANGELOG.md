# CHANGELOG

本文件记录 agent-hub（claude-view）每个版本的显著变更。**版本规则（2026-10-13 立）**：版本 = main head 上的 `vX.Y.Z` tag（部署/发布后打）；语义化——新特性 +0.1.0，缺陷修复 +0.0.1；**每个版本必须给出条目**，缺条目不得打 tag。

格式参照 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/)。

## [0.2.0] - 2026-10-13

**今日发布**（7800 正式环境部署态；2026-10-14 仓库历史重置，本条目随重置后的初始提交基线，内容等同重置前 `b3c1b6b` 树）。主体为本轮 slash-skills 三轮交付 + v0.1.1（2026-09-28）以来 445 提交的能力块补录（粒度摘要；重置前逐提交史仅存本地 bundle 备份）。

### Added（本轮：/ 补全体系）

- **`/` 补全数据源扩展**：全局技能（`~/.claude/skills`）、项目级 skills/commands（API 带 `?cwd=`，子目录 `ns:name`）、插件命令与技能（`plugin:` 前缀 + `enabledPlugins` 启用闸 + project 作用域 cwd 匹配）、frontmatter name/description 解析——补全面板数据源对齐 TUI
- **菜单过滤模糊查找**：`lib/fuzzy` 子序列匹配（大小写不敏感，前缀 > 连续 > 词首 > 靠前计分，同分短名优先，乱序/缺字符不命中，空查询回落全量）
- **菜单键盘操控**：Enter 选中候选仅替换 draft（尾空格关菜单，二次 Enter 才发送）、`↓↑` 移动高亮、`Esc` 关闭回落直发——修复菜单打开态 Enter 误提交中间态 draft 的缺陷
- **菜单可用性**：按来源分组展示（内置/技能/插件）+ 候选上限 50 + 滚动

### Added（区间补录：v0.1.1 → v0.2.0 主要能力块）

- **Agent 体系**：claude/opencode/dsh/omp 多驱动统一（driver-unify）、助手会话（hub-assistant）与单例闸、助手内定时任务（assistant-cron）、模型/思考档控制、AskUserQuestion 提问卡结构化渲染
- **Git 面板**：提交 Timeline 与文件清单两级联动、变更区行组件统一复用、子仓不进父仓变更列表、meta 行去重、图片对照 diff（binary 双窗格）、D 文件全删镜像、路径语义对齐 git-status（cwd≠仓库根 400 根修）
- **文件体系**：文件查看进右面板 + 主窗口多 Tab（面板-主功能区模型）、diff 页签化与跳源、文件树 cwd 泛化
- **终端**：网页端终端回归（纯工具定位，PTY 既有依赖不变）
- **远程/网关**：gateway-node 系列（p1-p3 + sweep）、nodelink 远程实例、多实例管理、远程安装
- **工程化**：设置页 i18n、三态主题体系（auto/dark/light + token 红线）、bundle 预算门禁（gz ≤300KB）、r 系列行为回归

### Fixed

- 菜单打开态 Enter 误提交中间态（本轮 round 3 人审缺陷，根因=菜单无键控）
- 变更区 meta 行与分区头同文重复、tree 行居中回归、pi 忙态用户消息双渲染等（详见各工单报告）

### 工单索引

- 本轮 slash-skills：管理仓需求 `docs/requirements/agent-hub-slash-skills/`（调研定案先A后B，A 已落地）
- B 阶段（SDK 精确校准源）：dreamnear/agent-hub#1；优化批：#2；spawn 首发时序加固：#3

## [0.1.1] - 2026-09-28

- fix(test): git panel 子模块测试允许 file 协议（git≥2.38.1 CVE-2022-39253 默认禁，6d48075 回流）

## [0.1.0]

- 初始版本基线（详见 git log）

# r2 修复报告

## FAIL-1 修复：分组 BUG

**修改文件**：
- `crates/server/src/models.rs:31` — `map_group` match 臂增加 `"working"`
- `crates/server/tests/models.rs` — `map_group_table` 增加 `"working"` 断言

**验证**：
```bash
curl "http://127.0.0.1:7800/api/agents?all=1"
# 返回 state:"running" → group:"working" ✅
```

## FAIL-2 修复：poll/watcher 后台任务自杀

**修改文件**：
- `crates/server/src/drivers/claude/watcher.rs:112` — `spawn_watch` 将 `return` 改为 `warn!` + continue
- `crates/server/src/drivers/claude/watcher.rs:131` — `spawn_poll` 将 `return` 改为 `warn!` + continue
- `crates/server/tests/ws_events.rs` — 新增 `poll_continues_when_no_receivers` 测试

**验证**：
```bash
# 启动 server，等 3s
grep -E "lagged|closed|exiting" /tmp/server.log
# 无任何输出 ✅（后台任务存活）
```

## 全部门禁

- cargo fmt: PASS ✅
- cargo clippy -D warnings: PASS ✅
- cargo test: 5/5 ws_events + 2/2 models + 3/3 projects = 10 tests PASS ✅
- npm --prefix web run build: PASS ✅
- npx --prefix web tsc --noEmit: PASS ✅

## 提交

```bash
git add -A
git commit -m "fix: 修复 r1 验收 FAIL-1（分组 working）+ FAIL-2（后台任务自杀）"
```

修复完成，等待 dr-tester r2 验收。

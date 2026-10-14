import { describe, expect, it } from 'vitest';
import { splitSystemBlocks } from './systemBlocks';

const taskNotification = [
  '<task-notification>',
  '<task-id>a5aed4bd4ba252007</task-id>',
  '<tool-use-id>call_b479623edd9846ffb2d3fbbd</tool-use-id>',
  '<output-file>/private/tmp/claude-tmp/tasks/a5aed4bd4ba252007.output</output-file>',
  '<status>completed</status>',
  '<summary>Background command "后台轮询 pipeline 4076 至完成" completed (exit code 0)</summary>',
  '</task-notification>',
].join('\n');

describe('splitSystemBlocks', () => {
  it('normal message returns single text segment', () => {
    const segs = splitSystemBlocks('# 标题\n\n正文段落');
    expect(segs).toEqual([{ kind: 'text', text: '# 标题\n\n正文段落' }]);
  });

  it('task-notification message collapses to one sys segment with summary label', () => {
    const segs = splitSystemBlocks(taskNotification);
    expect(segs).toHaveLength(1);
    expect(segs[0].kind).toBe('sys');
    expect(segs[0].tag).toBe('task-notification');
    expect(segs[0].label).toBe(
      'Background command "后台轮询 pipeline 4076 至完成" completed (exit code 0)',
    );
    // 原文完整保留（展开可见，不丢内容）
    expect(segs[0].text).toBe(taskNotification);
  });

  it('system-reminder without summary falls back to 系统通知', () => {
    const text = '<system-reminder>\nThe user named this session "penpot".\n</system-reminder>';
    const segs = splitSystemBlocks(text);
    expect(segs).toHaveLength(1);
    expect(segs[0].tag).toBe('system-reminder');
    expect(segs[0].label).toBe('系统通知');
  });

  it('tags inside fenced code blocks are not processed', () => {
    const text = `看这段示例：\n\`\`\`xml\n${taskNotification}\n\`\`\`\n完`;
    const segs = splitSystemBlocks(text);
    expect(segs).toEqual([{ kind: 'text', text }]);
  });

  it('tags inside inline code are not processed', () => {
    const text = '配置里写 `<task-notification>` 字样即可';
    const segs = splitSystemBlocks(text);
    expect(segs).toEqual([{ kind: 'text', text }]);
  });

  it('consecutive blocks each become their own segment', () => {
    const text = `${taskNotification}\n<system-reminder>\n用户把会话命名为 x。\n</system-reminder>\n后续说明`;
    const segs = splitSystemBlocks(text);
    expect(segs.map((s) => s.kind)).toEqual(['sys', 'sys', 'text']);
    expect(segs[0].tag).toBe('task-notification');
    expect(segs[1].tag).toBe('system-reminder');
    expect(segs[2].text).toBe('\n后续说明');
  });

  it('block surrounded by text keeps text parts', () => {
    const text = `前文\n${taskNotification}\n后文`;
    const segs = splitSystemBlocks(text);
    expect(segs.map((s) => s.kind)).toEqual(['text', 'sys', 'text']);
    expect(segs[0].text).toBe('前文\n');
    expect(segs[2].text).toBe('\n后文');
  });

  it('unclosed tag (streaming half) stays plain text', () => {
    const text = '<task-notification>\n<task-id>abc';
    const segs = splitSystemBlocks(text);
    expect(segs).toEqual([{ kind: 'text', text }]);
  });

  it('same tag written inside a block body does not break parsing', () => {
    // summary 文本里提到标签名（不带尖括号闭合形态）不影响
    const text = '<system-reminder>\n看到 <task-notification> 提醒请勿惊慌\n</system-reminder>';
    const segs = splitSystemBlocks(text);
    expect(segs).toHaveLength(1);
    expect(segs[0].tag).toBe('system-reminder');
  });

  it('splits JSON-escaped tool output form (literal \\n between tags)', () => {
    // 真实取证：tool_result 里的 XML 标签原文、换行是字面 \n（JSON 转义文本）
    const text = 'done\\n<task-notification>\\n<task-id>t1</task-id>\\n<summary>后台命令已完成</summary>\\n</task-notification>';
    const segs = splitSystemBlocks(text);
    const sys = segs.find((s) => s.kind === 'sys');
    expect(sys?.tag).toBe('task-notification');
    expect(sys?.label).toBe('后台命令已完成');
  });

  it('folds bare total_tokens reminder block (token budget notice)', () => {
    const text = '编译通过。\n<total_tokens>15000000 tokens left</total_tokens>\n继续。';
    const segs = splitSystemBlocks(text);
    const sys = segs.find((s) => s.kind === 'sys');
    expect(sys?.tag).toBe('total_tokens');
    expect(sys?.label).toBe('系统通知');
    expect(segs.filter((s) => s.kind === 'text').map((s) => s.text).join('')).toContain('编译通过。');
  });

  it('folds a block that contains inline code (tags straddle the protected span)', () => {
    // 2026-09-29 实测泄漏形态：块内 note 含行内代码，开闭标签落在保护区切分的不同
    // 碎段——碎段扫描法误判"未闭合"导致整块裸显；全串扫描 + 仅标签在代码区才跳过
    const text = [
      '<task-notification>',
      '<task-id>abc123</task-id>',
      '<status>completed</status>',
      '<note>检查 `docs/plan/tasks.md` 第 3 行</note>',
      '<summary>后台命令已完成</summary>',
      '</task-notification>',
    ].join('\n');
    const segs = splitSystemBlocks(text);
    const sys = segs.find((s) => s.kind === 'sys');
    expect(sys?.tag).toBe('task-notification');
    expect(sys?.label).toBe('后台命令已完成');
    expect(sys?.text).toContain('docs/plan/tasks.md');
  });

  it('does not fold when the block tags themselves are inside inline code', () => {
    const text = '示例 `<task-notification><task-id>t1</task-id></task-notification>` 这样写';
    expect(splitSystemBlocks(text)).toEqual([{ kind: 'text', text }]);
  });
});

// —— 2026-10-02 通用检测 + 语义映射（command/stdout/未知 tag 兜底）——

const effortSample = [
  '<command-name>/effort</command-name>',
  '<command-message>effort</command-message>',
  '<command-args>xhigh</command-args>',
  '<local-command-stdout>Set effort level to xhigh</local-command-stdout>',
].join('\n');

describe('通用 XML 块检测 + 语义映射', () => {
  it('/effort 真实样本：command 三件套合并为执行段 + stdout 结果段', () => {
    const segs = splitSystemBlocks(effortSample);
    expect(segs.map((s) => s.kind)).toEqual(['command', 'stdout']);
    expect(segs[0].label).toBe('/effort xhigh');
    // 执行段(三件套原文) + 结果段(stdout 原文)覆盖整条消息（块间换行归一）
    expect((segs[0].text + '\n' + segs[1].text).replace(/\n+/g, '\n')).toBe(effortSample);
    expect(segs[1].kind).toBe('stdout');
    expect(segs[1].label).toBe('Set effort level to xhigh');
  });

  it('缺 message/args 的不完整三件套 → 兜底 sys 段（不强行合并）', () => {
    const text = [
      '<command-name>/effort</command-name>',
      '<local-command-stdout>ok</local-command-stdout>',
    ].join('\n');
    const segs = splitSystemBlocks(text);
    expect(segs.map((s) => s.kind)).toEqual(['sys', 'stdout']);
  });

  it('未知 tag 纯块消息 → 通用兜底 sys 段（将来冒新 tag 自动兜住）', () => {
    const text = '<future-thing>\nsome new harness payload\n</future-thing>';
    const segs = splitSystemBlocks(text);
    expect(segs).toHaveLength(1);
    expect(segs[0].kind).toBe('sys');
    expect(segs[0].tag).toBe('future-thing');
    expect(segs[0].text).toBe(text);
  });

  it('代码围栏反例：围栏内含 <command-name> 不被语义化（原样文本）', () => {
    const text = [
      '示例：',
      '```xml',
      '<command-name>/effort</command-name>',
      '<command-message>effort</command-message>',
      '<command-args>xhigh</command-args>',
      '```',
      '完',
    ].join('\n');
    expect(splitSystemBlocks(text)).toEqual([{ kind: 'text', text }]);
  });

  it('散文夹尖括号（非整体块结构）→ 原样文本，不走通用管线', () => {
    const text = '前面 prose\n<future-thing>内容</future-thing>\n后面 prose';
    expect(splitSystemBlocks(text)).toEqual([{ kind: 'text', text }]);
  });

  it('混排模式下白名单语义保持（total_tokens 混散文仍折叠）', () => {
    const text = '编译通过。\n<total_tokens>15000000 tokens left</total_tokens>\n继续。';
    const segs = splitSystemBlocks(text);
    const sys = segs.find((s) => s.kind === 'sys');
    expect(sys?.tag).toBe('total_tokens');
  });
});

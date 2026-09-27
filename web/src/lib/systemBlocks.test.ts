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
});

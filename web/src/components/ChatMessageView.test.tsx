// @vitest-environment happy-dom
import { describe, expect, it } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import ChatMessageView from './ChatMessageView';
import { combineToolCalls } from '../hooks/combineToolCalls';
import BuiThinking from './bui/Thinking';
import BuiToolChips from './bui/ToolChips';
import BuiCodeBlock from './bui/CodeBlock';
import type { ChatMessage } from '../types';

const base: ChatMessage = {
  kind: 'user',
  rawType: null,
  text: null,
  toolUseId: null,
  toolName: null,
  input: null,
  result: null,
  error: null,
  ts: null,
};

function msg(partial: Partial<ChatMessage>): ChatMessage {
  return { ...base, ...partial };
}

function renderMsg(msg: ChatMessage): ReturnType<typeof render> {
  const item = combineToolCalls([msg])[0];
  return render(<ChatMessageView item={item} />);
}

describe('ChatMessageView', () => {
  it('renders tool_use as a BUI tool card with running state in header', () => {
    const msg: ChatMessage = {
      ...base,
      kind: 'tool_use',
      toolName: 'Bash',
      toolUseId: 'toolu_01',
      input: { command: 'cargo build' },
    };
    const { container } = renderMsg(msg);
    // 反馈批2：工具卡换 BUI ToolChips（data-bui 根），状态经 header 文本承载
    const bui = container.querySelector('[data-bui]');
    expect(bui).not.toBeNull();
    expect(bui?.textContent).toContain('Bash · 运行中');
    // 输入摘要 chip 与全文 detail 均在
    expect(bui?.textContent).toContain('cargo build');
  });

  it('renders thinking as a BUI collapsed trace', () => {
    const msg: ChatMessage = { ...base, kind: 'thinking', text: '内部推理' };
    const { container } = renderMsg(msg);
    const bui = container.querySelector('[data-bui]');
    expect(bui).not.toBeNull();
    // 默认收起（aria-expanded=false），header 为「思考完成」，全文在 DOM 可展开
    const toggle = bui?.querySelector('button[aria-expanded]');
    expect(toggle?.getAttribute('aria-expanded')).toBe('false');
    expect(bui?.textContent).toContain('思考完成');
    expect(bui?.textContent).toContain('内部推理');
  });

  it('renders user text as a bubble', () => {
    renderMsg({ ...base, kind: 'user', text: '你好' });
    expect(screen.getByText('你好')).toBeTruthy();
  });

  it('renders assistant plain text as markdown (heading/list/code)', () => {
    const { container } = renderMsg({
      ...base,
      kind: 'assistant',
      text: '### 标题\n\n- 列表项\n\n```js\nconst a = 1;\n```',
    });
    expect(container.querySelector('.md-body h3')?.textContent).toBe('标题');
    expect(container.querySelector('.md-body li')?.textContent).toBe('列表项');
    // 代码块换 BUI CodeBlock（data-bui），全文在案
    const bui = container.querySelector('[data-bui]');
    expect(bui).not.toBeNull();
    expect(bui?.textContent).toContain('const a = 1;');
    expect(bui?.textContent).toContain('js');
  });

  it('renders assistant text containing ANSI via the color-preserving path', () => {
    const { container } = renderMsg({
      ...base,
      kind: 'assistant',
      text: '\x1b[32mok\x1b[0m plain **text**',
    });
    // 含 ANSI → 保色 span，不做 markdown 解析（** 保持原样）
    expect(container.querySelector('.md-body')).toBeNull();
    const ansi = container.querySelector('.ansi');
    expect(ansi).not.toBeNull();
    expect(ansi?.textContent).toContain('**text**');
  });

  it('keeps user text as plain text even with markdown syntax', () => {
    renderMsg({ ...base, kind: 'user', text: '**加粗**与 ### 标题' });
    expect(screen.getByText('**加粗**与 ### 标题')).toBeTruthy();
  });

  it('renders uploaded image @path as an inline thumbnail (r72), raw path in title only', () => {
    renderMsg({
      ...base,
      kind: 'user',
      text: '试试贴图，看看这张图片说了什么\n@/var/folders/jf/T/claude-view-uploads/e54054b6.png',
    });
    const img = document.querySelector('.img-thumb img') as HTMLImageElement;
    expect(img).not.toBeNull();
    expect(img.getAttribute('src')).toBe('/api/images/e54054b6.png');
    expect(img.getAttribute('loading')).toBe('lazy');
    expect(img.getAttribute('alt')).toBe('[Image #1]');
    // 原路径不上屏，仅 title 透出
    expect(document.querySelector('.img-thumb')?.getAttribute('title')).toBe(
      '/var/folders/jf/T/claude-view-uploads/e54054b6.png',
    );
    expect(screen.queryByText(/claude-view-uploads/)).toBeNull();
  });

  it('falls back to the placeholder pill when the thumbnail fails to load', () => {
    const { container } = renderMsg({
      ...base,
      kind: 'user',
      text: '@/var/folders/jf/T/claude-view-uploads/e54054b6.png',
    });
    fireEvent.error(container.querySelector('.img-thumb img') as HTMLImageElement);
    const pill = container.querySelector('.img-pill');
    expect(pill).not.toBeNull();
    expect(pill?.getAttribute('title')).toBe('/var/folders/jf/T/claude-view-uploads/e54054b6.png');
  });

  it('tool_use input JSON containing an uploads image path renders a thumbnail (Read scenario)', () => {
    const items = combineToolCalls([
      msg({
        kind: 'tool_use',
        toolName: 'Read',
        toolUseId: 't1',
        input: { file_path: '/tmp/claude-view-uploads/abc-def.png' },
      }),
    ]);
    const { container } = render(<ChatMessageView item={items[0]} />);
    const img = container.querySelector('.img-thumb img') as HTMLImageElement | null;
    expect(img?.getAttribute('src')).toBe('/api/images/abc-def.png');
  });

  it('merged card shows failure state on error result', () => {
    const items = combineToolCalls([
      msg({ kind: 'tool_use', toolName: 'Bash', toolUseId: 't1' }),
      { ...base, kind: 'tool_result', toolUseId: 't1', error: true },
    ]);
    const { container } = render(<ChatMessageView item={items[0]} />);
    expect(container.querySelector('[data-bui]')?.textContent).toContain('失败');
  });

  it('renders tool_result output containing ANSI via the color-preserving path', () => {
    const items = combineToolCalls([
      msg({ kind: 'tool_use', toolName: 'Bash', toolUseId: 't1' }),
      { ...base, kind: 'tool_result', toolUseId: 't1', error: false, text: '\x1b[2m Duration \x1b[22m 2.31s' },
    ]);
    const { container } = render(<ChatMessageView item={items[0]} />);
    // ANSI 保色 span 在 BUI detail 内
    const ansi = container.querySelector('[data-bui] .ansi');
    expect(ansi).not.toBeNull();
    expect(container.querySelector('[data-bui]')?.textContent).toContain('2.31s');
  });

  it('renders tool_result output without ANSI as plain detail text', () => {
    const items = combineToolCalls([
      msg({ kind: 'tool_use', toolName: 'Bash', toolUseId: 't1' }),
      { ...base, kind: 'tool_result', toolUseId: 't1', error: false, text: 'plain output' },
    ]);
    const { container } = render(<ChatMessageView item={items[0]} />);
    expect(container.querySelector('[data-bui] .ansi')).toBeNull();
    expect(container.querySelector('[data-bui]')?.textContent).toContain('plain output');
  });

  it('renders cross-session message as a distinguished card with markdown body and collapsed disclaimer', () => {
    const text = [
      'Another Claude session sent a message:',
      '<cross-session-message from-name="open-plan" from-mode="bypass" from="uds:/tmp/x/59078.sock">【open 侧回报】\n\n### 已完成</cross-session-message>',
      'This came from another Claude session and is automated.',
    ].join('\n');
    const { container } = renderMsg({ ...base, kind: 'user', text });
    expect(container.querySelector('.cross-card')).not.toBeNull();
    // 新样式：› Message from @source: peek (click to expand)
    expect(container.querySelector('.cross-from')?.textContent).toBe('@open-plan');
    const peek = container.querySelector('.cross-peek')?.textContent;
    expect(peek).toContain('open 侧回报');
    expect(container.querySelector('.cross-hint')?.textContent).toContain('点击展开');
    // disclaimer 折叠区
    const disc = container.querySelector('.cross-disclaimer') as HTMLDetailsElement | null;
    expect(disc).not.toBeNull();
    expect(disc?.textContent).toContain('此为其他会话代理消息');
  });
});

describe('BUI components smoke (Beautiful UI 批2)', () => {
  it('Thinking static mode shows working shimmer label and rows', () => {
    const { container } = render(
      <BuiThinking mode="static" working rows={[{ primary: '步骤一' }]} />,
    );
    expect(container.querySelector('[aria-expanded]')).not.toBeNull();
    expect(container.textContent).toContain('步骤一');
  });

  it('ToolChips static mode renders all rows immediately without animation steps', () => {
    const { container } = render(
      <BuiToolChips
        static
        steps={[
          { icon: 'run', label: 'Bash', chip: 'npm test', mono: true, detailMono: false, detail: [{ text: 'all green' }] },
          { icon: 'read', label: 'Read', chip: 'a.ts', mono: true, detailMono: false, detail: [] },
        ]}
        diffs={[]}
        labels={{ header: 'Bash · 完成', more: '' }}
      />,
    );
    // static：两行同帧直显（无逐行入场）
    expect(container.textContent).toContain('Bash');
    expect(container.textContent).toContain('Read');
    expect(container.textContent).toContain('npm test');
  });

  it('CodeBlock renders line numbers and filename header', () => {
    const { container } = render(
      <BuiCodeBlock lines={['const a = 1;', 'const b = 2;']} filename="demo.ts" />,
    );
    expect(container.textContent).toContain('demo.ts');
    expect(container.textContent).toContain('Copy');
    expect(container.textContent).toContain('const a = 1;');
    // 行号 gutter
    expect(container.textContent).toContain('2');
  });
});

describe('ACP 工具卡（批2 任务9：omp update 真实形态）', () => {
  // fixture 形态 = ACP v1 tool_call/update（含批1 fake 与 omp 实测字段）
  const acpToolUse = msg({
    kind: 'tool_use',
    rawType: 'acp_tool_call',
    toolUseId: 'tc1',
    toolName: 'Edit file',
    input: {
      sessionUpdate: 'tool_call',
      toolCallId: 'tc1',
      title: 'Edit file',
      kind: 'edit',
      content: [
        { type: 'diff', path: '/repo/src/app.rs', oldText: 'let a = 1;\nlet b = 2;\n', newText: 'let a = 1;\nlet b = 3;\nlet c = 4;\n' },
      ],
    },
  });

  function renderAcp(extra: Partial<ChatMessage>): ReturnType<typeof render> {
    const upd = msg({
      kind: 'tool_result',
      rawType: 'acp_tool_update',
      toolUseId: 'tc1',
      result: { sessionUpdate: 'tool_call_update', toolCallId: 'tc1', status: 'completed' },
      ...extra,
    });
    const items = combineToolCalls([acpToolUse, upd]);
    return render(<ChatMessageView item={items[0]} />);
  }

  it('edit 工具卡：diff 进 chips（文件名 + 增删行数），完成态在 header', () => {
    const { container } = renderAcp({});
    const bui = container.querySelector('[data-bui]') as HTMLElement;
    expect(bui.textContent).toContain('Edit file · 完成');
    // 真实 diff chip（非 JSON dump）：data-diffchip + 增删计数
    const chip = container.querySelector('[data-diffchip]') as HTMLElement;
    expect(chip).not.toBeNull();
    expect(chip.textContent).toContain('app.rs');
    expect(chip.textContent).toContain('+3');
    expect(chip.textContent).toContain('−2');
    // hover 预览：oldText 行红（del）、newText 行绿（add）
    fireEvent.mouseEnter(chip);
    const preview = [...document.body.querySelectorAll('.fixed.z-50')].pop() as HTMLElement;
    expect(preview.textContent).toContain('let b = 2;');
    expect(preview.textContent).toContain('let c = 4;');
    const delLine = [...preview.querySelectorAll('[class*="bg-red-tint"]')];
    const addLine = [...preview.querySelectorAll('[class*="bg-green-tint"]')];
    expect(delLine).toHaveLength(2); // oldText 两行
    expect(addLine).toHaveLength(3); // newText 三行
  });

  it('无 result → 运行中；failed → 失败', () => {
    const running = combineToolCalls([acpToolUse])[0];
    const r1 = render(<ChatMessageView item={running} />);
    expect((r1.container.querySelector('[data-bui]') as HTMLElement).textContent).toContain(
      'Edit file · 运行中',
    );
    r1.unmount();
    const { container } = renderAcp({
      result: { sessionUpdate: 'tool_call_update', toolCallId: 'tc1', status: 'failed' },
    });
    expect((container.querySelector('[data-bui]') as HTMLElement).textContent).toContain(
      'Edit file · 失败',
    );
  });

  it('content 文本块进 detail；in_progress 也算运行中', () => {
    const use = msg({
      kind: 'tool_use',
      rawType: 'acp_tool_call',
      toolUseId: 'tc2',
      toolName: 'Search code',
      input: {
        sessionUpdate: 'tool_call',
        toolCallId: 'tc2',
        title: 'Search code',
        kind: 'search',
        content: [{ type: 'content', content: { type: 'text', text: 'found 3 hits' } }],
      },
    });
    const upd = msg({
      kind: 'tool_result',
      rawType: 'acp_tool_update',
      toolUseId: 'tc2',
      result: { sessionUpdate: 'tool_call_update', toolCallId: 'tc2', status: 'in_progress' },
    });
    const { container } = render(<ChatMessageView item={combineToolCalls([use, upd])[0]} />);
    const bui = container.querySelector('[data-bui]') as HTMLElement;
    expect(bui.textContent).toContain('Search code · 运行中');
    expect(bui.textContent).toContain('found 3 hits');
  });

  it('未知工具（无 title/content）：通用卡降级不崩', () => {
    const use = msg({
      kind: 'tool_use',
      rawType: 'acp_tool_call',
      toolUseId: 'tc3',
      toolName: null,
      input: { sessionUpdate: 'tool_call', toolCallId: 'tc3', kind: 'other' },
    });
    const upd = msg({
      kind: 'tool_result',
      rawType: 'acp_tool_update',
      toolUseId: 'tc3',
      result: { sessionUpdate: 'tool_call_update', toolCallId: 'tc3', status: 'completed' },
    });
    const { container } = render(<ChatMessageView item={combineToolCalls([use, upd])[0]} />);
    const bui = container.querySelector('[data-bui]') as HTMLElement;
    expect(bui.textContent).toContain('· 完成'); // 状态照常
    expect(bui.textContent).toContain('other'); // kind 兜底作 label
  });

  it('同名不同目录的 diff 块互不覆盖（ocr-review 中）', () => {
    const use = msg({
      kind: 'tool_use',
      rawType: 'acp_tool_call',
      toolUseId: 'tc4',
      toolName: 'Multi edit',
      input: {
        sessionUpdate: 'tool_call',
        toolCallId: 'tc4',
        title: 'Multi edit',
        kind: 'edit',
        content: [
          { type: 'diff', path: '/repo/a/app.rs', oldText: 'a-old', newText: 'a-new' },
          { type: 'diff', path: '/repo/b/app.rs', oldText: 'b-old', newText: 'b-new' },
        ],
      },
    });
    const upd = msg({
      kind: 'tool_result',
      rawType: 'acp_tool_update',
      toolUseId: 'tc4',
      result: { sessionUpdate: 'tool_call_update', toolCallId: 'tc4', status: 'completed' },
    });
    const { container } = render(<ChatMessageView item={combineToolCalls([use, upd])[0]} />);
    const chips = [...container.querySelectorAll('[data-diffchip]')];
    expect(chips).toHaveLength(2);
    // 各自 hover 预览对应各自内容（撞名时后写覆盖前写 → 两个 chip 预览同文）
    fireEvent.mouseEnter(chips[0] as HTMLElement);
    const p1 = [...document.body.querySelectorAll('.fixed.z-50')].pop() as HTMLElement;
    expect(p1.textContent).toContain('a-old');
    expect(p1.textContent).not.toContain('b-old');
    fireEvent.mouseEnter(chips[1] as HTMLElement);
    const p2 = [...document.body.querySelectorAll('.fixed.z-50')].pop() as HTMLElement;
    expect(p2.textContent).toContain('b-old');
    expect(p2.textContent).not.toContain('a-old');
  });

  it('省略 path 的多个 diff 块不互相覆盖（ocr-review 中）', () => {
    const use = msg({
      kind: 'tool_use',
      rawType: 'acp_tool_call',
      toolUseId: 'tc5',
      toolName: 'Anonymous diffs',
      input: {
        sessionUpdate: 'tool_call',
        toolCallId: 'tc5',
        kind: 'edit',
        content: [
          { type: 'diff', oldText: 'x-old', newText: 'x-new' },
          { type: 'diff', oldText: 'y-old', newText: 'y-new' },
        ],
      },
    });
    const upd = msg({
      kind: 'tool_result',
      rawType: 'acp_tool_update',
      toolUseId: 'tc5',
      result: { sessionUpdate: 'tool_call_update', toolCallId: 'tc5', status: 'completed' },
    });
    const { container } = render(<ChatMessageView item={combineToolCalls([use, upd])[0]} />);
    const chips = [...container.querySelectorAll('[data-diffchip]')];
    expect(chips).toHaveLength(2);
    fireEvent.mouseEnter(chips[0] as HTMLElement);
    const p1 = [...document.body.querySelectorAll('.fixed.z-50')].pop() as HTMLElement;
    expect(p1.textContent).toContain('x-old');
    expect(p1.textContent).not.toContain('y-old');
  });

  it('collapses task-notification user message into a sys-block summary bar', () => {
    const { container } = renderMsg(
      msg({
        kind: 'user',
        text: '<task-notification>\n<task-id>t1</task-id>\n<status>completed</status>\n<summary>后台命令已完成 (exit code 0)</summary>\n</task-notification>',
      }),
    );
    const bar = container.querySelector('details.sys-block');
    expect(bar).not.toBeNull();
    // 摘要条弱化可见：标签 + summary 文本；原始 XML 在展开体里不丢
    expect(bar?.textContent).toContain('task-notification');
    expect(bar?.querySelector('summary')?.textContent).toContain('后台命令已完成 (exit code 0)');
    expect(bar?.querySelector('pre')?.textContent).toContain('<task-id>t1</task-id>');
  });

  it('keeps the same tag inside code blocks rendered as plain markdown', () => {
    const text = '示例：\n```xml\n<task-notification>\n<task-id>t1</task-id>\n</task-notification>\n```';
    const { container } = renderMsg(msg({ kind: 'assistant', text }));
    expect(container.querySelector('details.sys-block')).toBeNull();
    expect(container.textContent).toContain('<task-notification>');
  });

  it('folds system block inside tool card detail with 工具输出 badge', () => {
    const items = combineToolCalls([
      msg({ kind: 'tool_use', toolName: 'Bash', toolUseId: 't9' }),
      {
        ...base,
        kind: 'tool_result',
        toolUseId: 't9',
        error: false,
        text: 'ok\n<task-notification>\n<task-id>t1</task-id>\n<summary>后台命令已完成</summary>\n</task-notification>',
      },
    ]);
    const { container } = render(<ChatMessageView item={items[0]} />);
    const bar = container.querySelector('[data-bui] details.sys-block--tool');
    expect(bar).not.toBeNull();
    expect(bar?.textContent).toContain('工具输出');
    expect(bar?.querySelector('summary')?.textContent).toContain('后台命令已完成');
  });

  it('leaves plain tool output without system blocks untouched', () => {
    const items = combineToolCalls([
      msg({ kind: 'tool_use', toolName: 'Bash', toolUseId: 't10' }),
      { ...base, kind: 'tool_result', toolUseId: 't10', error: false, text: 'plain output' },
    ]);
    const { container } = render(<ChatMessageView item={items[0]} />);
    expect(container.querySelector('details.sys-block')).toBeNull();
    expect(container.querySelector('[data-bui]')?.textContent).toContain('plain output');
  });
});

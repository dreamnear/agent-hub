// @vitest-environment happy-dom
// BUI 门禁与备用组件冒烟（Beautiful UI 反馈轮 + 批5 判据）
import { readFileSync, readdirSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { render } from '@testing-library/react';
import AgentScreen from './AgentScreen';
import ApprovalCard from './ApprovalCard';
import ContextCards from './ContextCards';
import Flowchart from './Flowchart';
import FineTuneCard from './FineTuneCard';
import InsightCards from './InsightCards';
import SelectionActions from './SelectionActions';

const BUI_DIR = 'src/components/bui';

// 白名单语义（主题无关，允许）：text-white（彩底白字）、bg-white/N 与 bg-black/N
// （带透明度的遮罩/高光层）、bg-white（MiniSwitch 滑块等小元素原样）。
// 禁用（会破坏三态的面板色）：text-black、
// bg-white 豁免：仅 MiniSwitch 滑块白点一处（官网原样，主题无关）；bg-black 须用任意值 rgba（遮罩黑）。
// 全部数字调色板类（gray-500 系）与 JSX 内联 hex——这些不随 --c-* 三态
// （r62 dark 芯片白底缺陷的防再犯门禁）。
const BANNED =
  /text-black|bg-(?:gray|slate|zinc|neutral|stone|red|green|blue|orange|yellow|amber|lime|purple|pink|cyan|teal|indigo|violet|fuchsia|rose|emerald|sky)-\d|text-(?:gray|slate|zinc|neutral|stone|red|green|blue|orange|yellow|amber|lime|purple|pink|cyan|teal|indigo|violet|fuchsia|rose|emerald|sky)-\d/;

describe('BUI color gate（批反馈门禁）', () => {
  it('keeps every bui component free of non-token color utilities and inline hex styles', () => {
    const offenders: string[] = [];
    for (const f of readdirSync(BUI_DIR)) {
      if (!f.endsWith('.tsx') || f.endsWith('.test.tsx')) continue;
      const src = readFileSync(`${BUI_DIR}/${f}`, 'utf8');
      // 1) Tailwind 默认调色板类
      const utilMatch = src.match(BANNED);
      if (utilMatch) offenders.push(`${f}: ${utilMatch[0]}`);
      // 2) style={{ ... '#hex' ... }} 内联色值（TAG_PALETTE 等演示数据数组内的 hex 豁免——
      //    只查 JSX style 对象形态）
      if (/style=\{\{[^}]*['"]#[0-9a-fA-F]{3,8}['"]/.test(src)) {
        offenders.push(`${f}: inline hex style`);
      }
    }
    expect(offenders, `非 token 颜色残留:\n${offenders.join('\n')}`).toEqual([]);
  });

  it('keeps _deps stubs free of hardcoded palette classes', () => {
    const offenders: string[] = [];
    for (const f of readdirSync(`${BUI_DIR}/_deps`)) {
      if (!f.endsWith('.tsx')) continue;
      const src = readFileSync(`${BUI_DIR}/_deps/${f}`, 'utf8');
      const hit = src.match(/text-black|bg-(?:gray|slate)-\d|text-(?:gray|slate)-\d/);
      if (hit) offenders.push(`_deps/${f}: ${hit[0]}`);
    }
    expect(offenders).toEqual([]);
  });
});

describe('BUI standby components mount smoke（批5 判据：类挂载正确、无样式报错）', () => {
  it('mounts ApprovalCard', () => {
    const { container } = render(<ApprovalCard />);
    expect(container.firstElementChild).not.toBeNull();
  });

  it('mounts ContextCards', () => {
    const { container } = render(<ContextCards />);
    expect(container.firstElementChild).not.toBeNull();
  });

  it('mounts InsightCards', () => {
    const { container } = render(<InsightCards />);
    expect(container.firstElementChild).not.toBeNull();
  });

  it('mounts Flowchart', () => {
    const { container } = render(<Flowchart />);
    expect(container.firstElementChild).not.toBeNull();
  });

  it('mounts FineTuneCard', () => {
    const { container } = render(<FineTuneCard />);
    expect(container.firstElementChild).not.toBeNull();
  });

  it('mounts SelectionActions', () => {
    const { container } = render(<SelectionActions />);
    expect(container.firstElementChild).not.toBeNull();
  });

  it('mounts AgentScreen', () => {
    const { container } = render(<AgentScreen />);
    expect(container.firstElementChild).not.toBeNull();
  });
});

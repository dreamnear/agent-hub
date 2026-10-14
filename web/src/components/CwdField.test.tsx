// @vitest-environment happy-dom
// CwdField：工程树扁平化选项（主仓+worktree）、选树选项回填输入框、手输不受选项限制、空值一次性回填。
import { afterEach, describe, expect, it, vi } from 'vitest';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor } from '@testing-library/react';
import { useState, type ReactElement } from 'react';
import CwdField from './CwdField';
import type { GitTreeGroup } from '../types';

const projectTreeMock = vi.fn((): Promise<GitTreeGroup[]> => Promise.resolve([]));
vi.mock('../api', () => ({
  api: {
    projectTree: () => projectTreeMock(),
  },
}));

const TREE = [
  {
    main: { path: '/repo/a', name: 'alpha', isMain: true, isGit: true, branch: 'main', head: null },
    worktrees: [
      { path: '/repo/a-wt', name: 'alpha-wt', isMain: false, isGit: true, branch: 'feat/x', head: null },
    ],
  },
  {
    main: { path: '/repo/b', name: 'beta', isMain: true, isGit: true, branch: 'dev', head: null },
    worktrees: [],
  },
];

function Harness({ onPick }: { onPick?: (v: string) => void }): ReactElement {
  const [v, setV] = useState('');
  return (
    <>
      <div data-testid="v">{v}</div>
      <CwdField
        label="工程 (cwd)"
        inputId="cwd-input"
        value={v}
        onChange={(x) => {
          onPick?.(x);
          setV(x);
        }}
      />
    </>
  );
}

const wrap = (ui: ReactElement): ReactElement => (
  <QueryClientProvider client={new QueryClient({ defaultOptions: { queries: { retry: false } } })}>
    {ui}
  </QueryClientProvider>
);

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe('CwdField（工程树回填 + 恒可手输）', () => {
  it('空值一次性回填首个主仓（既有「打开即有默认 cwd」体验）', async () => {
    projectTreeMock.mockResolvedValue(TREE);
    const { getByTestId } = render(wrap(<Harness />));
    await waitFor(() => expect(getByTestId('v').textContent).toBe('/repo/a'));
  });

  it('工程树扁平化选项：主仓 optgroup + worktree 子项（分组按主仓名/分支）', async () => {
    projectTreeMock.mockResolvedValue(TREE);
    const { container } = render(wrap(<Harness />));
    await waitFor(() => expect(container.querySelector('.cwd-field-pick')).not.toBeNull());
    const pick = container.querySelector('.cwd-field-pick') as HTMLSelectElement;
    const optgroups = pick.querySelectorAll('optgroup');
    expect(optgroups).toHaveLength(2); // alpha / beta 两组
    expect((optgroups[0] as HTMLOptGroupElement).label).toBe('alpha · main');
    const options = [...pick.querySelectorAll('option')].map((o) => o.value);
    expect(options).toContain('/repo/a');
    expect(options).toContain('/repo/a-wt'); // worktree 全列
    expect(options).toContain('/repo/b');
  });

  it('选树选项 → 回填输入框值', async () => {
    projectTreeMock.mockResolvedValue(TREE);
    const onPick = vi.fn();
    const { container } = render(wrap(<Harness onPick={onPick} />));
    await waitFor(() => expect(container.querySelector('.cwd-field-pick')).not.toBeNull());
    const pick = container.querySelector('.cwd-field-pick') as HTMLSelectElement;
    fireEvent.change(pick, { target: { value: '/repo/a-wt' } });
    expect(onPick).toHaveBeenCalledWith('/repo/a-wt');
  });

  it('手输新目录不受选项限制（树外绝对路径照常上报）', async () => {
    projectTreeMock.mockResolvedValue([]);
    const onPick = vi.fn();
    const { container, getByLabelText } = render(wrap(<Harness onPick={onPick} />));
    // 树为空：无回填下拉（选项源缺位不挡手输）
    expect(container.querySelector('.cwd-field-pick')).toBeNull();
    fireEvent.change(getByLabelText('工程 (cwd)'), { target: { value: '/brand/new/dir' } });
    expect(onPick).toHaveBeenCalledWith('/brand/new/dir');
  });
});
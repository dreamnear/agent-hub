// @vitest-environment happy-dom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import ProjectNotesDialog, {
  clampNotePos,
  clampNoteSize,
  isNotePinned,
  loadNotePos,
  loadNoteSize,
  NOTE_MIN_SIZE,
  saveNotePos,
  saveNoteSize,
  setNotePinned,
} from './ProjectNotesDialog';

const getNoteMock = vi.fn();
const putNoteMock = vi.fn();
vi.mock('../api', () => ({
  api: {
    getNote: (...a: unknown[]) => getNoteMock(...a),
    putNote: (...a: unknown[]) => putNoteMock(...a),
  },
}));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  localStorage.clear();
});

const ta = (): HTMLTextAreaElement => screen.getByLabelText('便签内容') as HTMLTextAreaElement;
const saveBtn = (): HTMLButtonElement => screen.getByRole('button', { name: '保存' }) as HTMLButtonElement;
const pinBtn = (): HTMLButtonElement => screen.getByRole('button', { name: /钉住/ }) as HTMLButtonElement;

describe('ProjectNotesDialog 工程便签', () => {
  it('打开即按 cwd 拉取既有便签；改动前保存禁用，改动后保存并回调 putNote + 已保存提示', async () => {
    getNoteMock.mockResolvedValue({ content: '端口: 8080\n' });
    putNoteMock.mockResolvedValue({ content: 'x' });
    render(<ProjectNotesDialog cwd="/repo/main" title="main" onClose={() => {}} />);

    await waitFor(() => expect(ta().value).toBe('端口: 8080\n'));
    expect(getNoteMock).toHaveBeenCalledWith('/repo/main');
    expect(saveBtn().disabled).toBe(true);

    fireEvent.change(ta(), { target: { value: '端口: 8080\n账号 admin/test123\n' } });
    expect(saveBtn().disabled).toBe(false);
    fireEvent.click(saveBtn());
    await waitFor(() =>
      expect(putNoteMock).toHaveBeenCalledWith('/repo/main', '端口: 8080\n账号 admin/test123\n'),
    );
    expect(screen.getByRole('status').textContent).toContain('已保存');
  });

  it('无便签工程从空文本开始', async () => {
    getNoteMock.mockResolvedValue({ content: null });
    render(<ProjectNotesDialog cwd="/repo/other" title="other" onClose={() => {}} />);
    await waitFor(() => expect(ta().value).toBe(''));
  });

  it('读取失败展示错误（不回显内容）', async () => {
    getNoteMock.mockRejectedValue(new Error('boom'));
    render(<ProjectNotesDialog cwd="/repo/err" title="err" onClose={() => {}} />);
    await waitFor(() => expect(screen.getByText(/boom/)).toBeTruthy());
  });
});

describe('钉住（pin）按工程记忆', () => {
  it('图钉点击流转：未钉→钉（写 localStorage + aria-pressed）→再点取消（清除）', async () => {
    getNoteMock.mockResolvedValue({ content: '' });
    render(<ProjectNotesDialog cwd="/repo/main" title="main" onClose={() => {}} />);
    await waitFor(() => expect(ta().disabled).toBe(false));

    expect(isNotePinned('/repo/main')).toBe(false);
    fireEvent.click(pinBtn());
    expect(pinBtn().getAttribute('aria-pressed')).toBe('true');
    expect(isNotePinned('/repo/main')).toBe(true);
    expect(isNotePinned('/repo/other')).toBe(false); // 钉态按工程隔离

    fireEvent.click(pinBtn());
    expect(pinBtn().getAttribute('aria-pressed')).toBe('false');
    expect(isNotePinned('/repo/main')).toBe(false);
  });

  it('setNotePinned 挂载前预置 → 卡片初始即钉住态', async () => {
    setNotePinned('/repo/pin', true);
    getNoteMock.mockResolvedValue({ content: null });
    render(<ProjectNotesDialog cwd="/repo/pin" title="pin" onClose={() => {}} />);
    expect(pinBtn().getAttribute('aria-pressed')).toBe('true');
  });

  it('cwd 变化（App 层实例复用）时钉住态同步该工程（防跨工程串台，ocr-review 中）', async () => {
    setNotePinned('/repo/a', true);
    getNoteMock.mockResolvedValue({ content: null });
    const view = render(<ProjectNotesDialog cwd="/repo/a" title="A" onClose={() => {}} />);
    expect(pinBtn().getAttribute('aria-pressed')).toBe('true');
    // 无 key 复用实例切到未钉工程：钉住态必须跟新 cwd 走
    view.rerender(<ProjectNotesDialog cwd="/repo/b" title="B" onClose={() => {}} />);
    expect(pinBtn().getAttribute('aria-pressed')).toBe('false');
    expect(getNoteMock).toHaveBeenCalledWith('/repo/b');
  });
});

describe('位置记忆（localStorage 按工程）', () => {
  it('saveNotePos/loadNotePos 存取往返；load 对坏数据返回 null', () => {
    saveNotePos('/repo/main', { x: 120, y: 80 });
    expect(loadNotePos('/repo/main', 560, 400)).toEqual({ x: 120, y: 80 });
    expect(loadNotePos('/repo/none', 560, 400)).toBeNull();
    localStorage.setItem('notes_card_pos:/repo/bad', 'not-json');
    expect(loadNotePos('/repo/bad', 560, 400)).toBeNull();
  });

  it('clampNotePos 把越界位置拉回视口内（视口变小防出界）', () => {
    const inViewport = clampNotePos({ x: 5000, y: 5000 }, 560, 400);
    expect(inViewport.x).toBeLessThanOrEqual(window.innerWidth - 560 - 8);
    expect(inViewport.y).toBeLessThanOrEqual(window.innerHeight - 400 - 8);
    expect(clampNotePos({ x: -50, y: -50 }, 560, 400)).toEqual({ x: 8, y: 8 });
  });

  it('头部拖动（pointer 事件）更新卡片位置并在松开时写 localStorage', async () => {
    getNoteMock.mockResolvedValue({ content: '' });
    const { container } = render(
      <ProjectNotesDialog cwd="/repo/drag" title="drag" onClose={() => {}} />,
    );
    await waitFor(() => expect(ta().disabled).toBe(false));
    const head = container.querySelector<HTMLElement>('.notes-head');
    if (!head) throw new Error('notes-head 未渲染');
    // 挂载后已落初始位（happy-dom 无真实布局，getBoundingClientRect 全 0——落位为 clamp(0)）
    await waitFor(() => expect(container.querySelector('.notes-card--float')).not.toBeNull());
    const startX = 10;
    const startY = 10;
    fireEvent.pointerDown(head, { pointerId: 1, clientX: startX, clientY: startY });
    fireEvent.pointerMove(head, { pointerId: 1, clientX: startX + 40, clientY: startY + 25 });
    fireEvent.pointerUp(head, { pointerId: 1 });
    const pos = loadNotePos('/repo/drag', 0, 0);
    expect(pos).not.toBeNull();
    expect(pos?.x).toBeGreaterThan(startX - 1);
    expect(pos?.y).toBeGreaterThan(startY - 1);
  });

  it('头部按钮（图钉/关闭）不触发拖动劫持：pointerdown 于按钮上时拖动不启动', async () => {
    getNoteMock.mockResolvedValue({ content: null });
    const onClose = vi.fn();
    const { container } = render(
      <ProjectNotesDialog cwd="/repo/headbtn" title="hb" onClose={onClose} />,
    );
    await waitFor(() => expect(ta().disabled).toBe(false));
    const head = container.querySelector<HTMLElement>('.notes-head');
    if (!head) throw new Error('notes-head 未渲染');
    // 按钮上 pointerdown → 拖动不应启动（点关闭后无 dragging 残留且 onClose 生效）
    const closeBtn = screen.getByRole('button', { name: '关闭便签' });
    fireEvent.pointerDown(head, { pointerId: 1, clientX: 5, clientY: 5 });
    fireEvent.pointerUp(head, { pointerId: 1 });
    fireEvent.click(closeBtn);
    expect(onClose).toHaveBeenCalledTimes(1);
    // 头部空白处 pointerdown 才启动拖动（dragging 类挂卡片上）
    fireEvent.pointerDown(head, { pointerId: 2, clientX: 5, clientY: 5 });
    expect(container.querySelector('.notes-card--dragging')).not.toBeNull();
  });

  it('右下角拖拽 resize：宽高随指针变化并写 localStorage；低于最小尺寸被 clamp', async () => {
    getNoteMock.mockResolvedValue({ content: null });
    const { container } = render(
      <ProjectNotesDialog cwd="/repo/resize" title="rz" onClose={() => {}} />,
    );
    await waitFor(() => expect(ta().disabled).toBe(false));
    const handle = container.querySelector<HTMLElement>('.notes-resize-handle');
    if (!handle) throw new Error('resize handle 未渲染');
    fireEvent.pointerDown(handle, { pointerId: 1, clientX: 100, clientY: 100 });
    fireEvent.pointerMove(handle, { pointerId: 1, clientX: 400, clientY: 350 });
    fireEvent.pointerUp(handle, { pointerId: 1 });
    const stored = loadNoteSize('/repo/resize');
    expect(stored).not.toBeNull();
    expect(stored?.w).toBeGreaterThanOrEqual(NOTE_MIN_SIZE.w);
    expect(stored?.h).toBeGreaterThanOrEqual(NOTE_MIN_SIZE.h);
  });

  it('右下角向小拖：宽高被 clamp 不小于最小 240×180 后落盘（shrink 不破下限）', async () => {
    getNoteMock.mockResolvedValue({ content: null });
    const { container } = render(
      <ProjectNotesDialog cwd="/repo/shrink" title="sh" onClose={() => {}} />,
    );
    await waitFor(() => expect(ta().disabled).toBe(false));
    const handle = container.querySelector<HTMLElement>('.notes-resize-handle');
    if (!handle) throw new Error('resize handle 未渲染');
    // 从 100,100 向左上拖 200px → 请求尺寸远小于最小，落盘应为 clamp 后下限
    fireEvent.pointerDown(handle, { pointerId: 1, clientX: 100, clientY: 100 });
    fireEvent.pointerMove(handle, { pointerId: 1, clientX: -100, clientY: -100 });
    fireEvent.pointerUp(handle, { pointerId: 1 });
    const stored = loadNoteSize('/repo/shrink');
    expect(stored).not.toBeNull();
    expect(stored?.w).toBe(NOTE_MIN_SIZE.w);
    expect(stored?.h).toBe(NOTE_MIN_SIZE.h);
  });

  it('尺寸记忆：saveNoteSize/loadNoteSize 往返；clampNoteSize 把过小尺寸抬到最小 240×180', () => {
    saveNoteSize('/repo/sz', { w: 700, h: 500 });
    expect(loadNoteSize('/repo/sz')).toEqual({ w: 700, h: 500 });
    expect(clampNoteSize({ w: 50, h: 40 })).toEqual(NOTE_MIN_SIZE);
    expect(loadNoteSize('/repo/none')).toBeNull();
    localStorage.setItem('notes_card_size:/repo/bad', '{{');
    expect(loadNoteSize('/repo/bad')).toBeNull();
  });
});

describe('便签 per 实例隔离（任务8）', () => {
  const remoteApi = {
    getNote: vi.fn(() => Promise.resolve({ content: 'remote 内容' })),
    putNote: vi.fn(() => Promise.resolve({ content: null })),
  };

  it('传入 per-instance api 时读写走该 api（不落本机 api）', async () => {
    render(
      <ProjectNotesDialog
        cwd="/repo/main"
        title="r"
        api={remoteApi as never}
        instanceId="instA"
        onClose={() => {}}
      />,
    );
    await waitFor(() => expect(remoteApi.getNote).toHaveBeenCalledWith('/repo/main'));
    expect(getNoteMock).not.toHaveBeenCalled();
    fireEvent.change(ta(), { target: { value: '远程改动' } });
    fireEvent.click(saveBtn());
    await waitFor(() => expect(remoteApi.putNote).toHaveBeenCalledWith('/repo/main', '远程改动'));
    expect(putNoteMock).not.toHaveBeenCalled();
  });

  it('同 cwd 两实例钉态互不串（localStorage 键带实例段）', () => {
    setNotePinned('/repo/main', true, 'instA');
    expect(isNotePinned('/repo/main', 'instA')).toBe(true);
    expect(isNotePinned('/repo/main', 'instB')).toBe(false);
    expect(isNotePinned('/repo/main', null)).toBe(false); // 本机独立
  });
});

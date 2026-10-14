import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type PointerEvent as ReactPointerEvent,
  type ReactElement,
  type ReactNode,
} from 'react';
import { useI18n } from '../i18n';
import './PanelShell.css';

/// 宽度约束（面板模板规格）：工程栏 [220,560]px 默认 280；Git 栏略窄 [200,480]
/// 默认 240（分支/状态列表窄栏够用）；文件树栏同族 [200,480] 默认 260。双击手柄复位默认。
export const PANEL_W = { min: 220, max: 560, default: 280 } as const;
export const PANEL_GIT_W = { min: 200, max: 480, default: 240 } as const;
export const PANEL_FILES_W = { min: 200, max: 480, default: 260 } as const;
const ARROW_STEP = 16;

/// 持久化键（跟随 hub_* 键模式）：App 初始化 loadPanelPrefs 读，变更后写回。
/// hub_panel_git_h（旧 Git 槽定高键）随竖栏化废弃，残留值无害不再读写。
export const PANEL_W_KEY = 'hub_panel_right_w';
export const PANEL_GIT_W_KEY = 'hub_panel_git_w';
export const PANEL_FILES_W_KEY = 'hub_panel_files_w';

const clamp = (v: number, min: number, max: number): number => Math.min(max, Math.max(min, v));

/// 读持久化的面板宽度（工程 + Git + 文件各自一键；越界/坏值回退默认）。
export function loadPanelPrefs(): { width: number; gitWidth: number; filesWidth: number } {
  const num = (key: string, fallback: number, min: number, max: number): number => {
    const raw = typeof window === 'undefined' ? null : window.localStorage.getItem(key);
    const v = raw == null ? NaN : Number(raw);
    return Number.isFinite(v) ? clamp(v, min, max) : fallback;
  };
  return {
    width: num(PANEL_W_KEY, PANEL_W.default, PANEL_W.min, PANEL_W.max),
    gitWidth: num(PANEL_GIT_W_KEY, PANEL_GIT_W.default, PANEL_GIT_W.min, PANEL_GIT_W.max),
    filesWidth: num(PANEL_FILES_W_KEY, PANEL_FILES_W.default, PANEL_FILES_W.min, PANEL_FILES_W.max),
  };
}

/// 桌面判定（≥769px，与工程 ≤768px 断点对齐）：手柄/双竖栏桌面限定，
/// 移动端抽屉内树 + Git 堆叠（行为零变化）。App 布局分支同用此钩子。
export function useDesktop(): boolean {
  const [desktop, setDesktop] = useState(
    () => typeof window !== 'undefined' && window.matchMedia('(min-width: 769px)').matches,
  );
  useEffect(() => {
    const mq = window.matchMedia('(min-width: 769px)');
    const onChange = (e: MediaQueryListEvent): void => setDesktop(e.matches);
    mq.addEventListener('change', onChange);
    return () => mq.removeEventListener('change', onChange);
  }, []);
  return desktop;
}

interface Props {
  width: number;
  onWidthChange: (w: number) => void;
  /// 宽度 clamp 区间与双击复位值（Git 栏传 PANEL_GIT_W 各值）
  min?: number;
  max?: number;
  resetWidth?: number;
  children: ReactNode;
}

/// 单面板壳：内容槽 + 左缘拖拽手柄（Pointer Events 统一鼠标/触摸，
/// setPointerCapture 保证拖出命中区仍持续跟踪）；键盘箭头 ±16px 微调，
/// 双击复位。宽度状态由 App 受控持有，工程/Git 两竖栏各挂一壳分别持久化。
export default function PanelShell({
  width,
  onWidthChange,
  min = PANEL_W.min,
  max = PANEL_W.max,
  resetWidth = PANEL_W.default,
  children,
}: Props): ReactElement {
  const t = useI18n();
  const desktop = useDesktop();
  const drag = useRef<{ x: number; w: number } | null>(null);

  const onHandleDown = (e: ReactPointerEvent<HTMLElement>): void => {
    // 不调 preventDefault：pointerdown 上它会抑制兼容鼠标事件（click/dblclick），
    // 双击复位就永远收不到；防选中交给 CSS user-select:none
    e.currentTarget.setPointerCapture(e.pointerId);
    drag.current = { x: e.clientX, w: width };
  };

  const onHandleMove = (e: ReactPointerEvent<HTMLElement>): void => {
    const d = drag.current;
    if (d == null) return;
    // 面板在右：向左拖 = 变宽
    onWidthChange(clamp(d.w + (d.x - e.clientX), min, max));
  };

  const onHandleUp = (e: ReactPointerEvent<HTMLElement>): void => {
    e.currentTarget.releasePointerCapture(e.pointerId);
    drag.current = null;
  };

  const onWidthKey = (e: ReactKeyboardEvent<HTMLElement>): void => {
    // 分隔条左移（ArrowLeft）= 面板变宽，右移 = 变窄
    if (e.key === 'ArrowLeft') {
      e.preventDefault();
      onWidthChange(clamp(width + ARROW_STEP, min, max));
    } else if (e.key === 'ArrowRight') {
      e.preventDefault();
      onWidthChange(clamp(width - ARROW_STEP, min, max));
    }
  };

  return (
    <div className="panel-shell">
      {desktop ? (
        <div
          className="panel-handle-w"
          role="separator"
          aria-orientation="vertical"
          aria-label={t('panel.resizeWidth')}
          tabIndex={0}
          onPointerDown={onHandleDown}
          onPointerMove={onHandleMove}
          onPointerUp={onHandleUp}
          onKeyDown={onWidthKey}
          onDoubleClick={() => onWidthChange(resetWidth)}
        />
      ) : null}
      <div className="panel-slot">{children}</div>
    </div>
  );
}

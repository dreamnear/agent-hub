import { useEffect, useRef } from 'react';

/**
 * 覆盖层 Esc 统一键控（#20）：`active` 期间挂 document 级 keydown 监听，
 * Escape 触发 `onEscape`。挂在 document 而非 backdrop div 的 onKeyDown——
 * 后者焦点落在 body（点击 backdrop 后焦点回 body）时永不触发（#20 实测根因：
 * 菜单/历史弹窗/⌘K 三处 Esc 全部失效同源）。
 *
 * handler 经 ref 取最新（免 effect 随闭包重建）；`active` 翻 false 即摘监听，
 * 覆盖层卸载 = backdrop 一并卸载（React 同步提交），Esc 后的下一次点击直接
 * 落到下层真实元素（验收：backdrop 不吞点击）。
 */
export function useEscapeKey(active: boolean, onEscape: () => void): void {
  const handlerRef = useRef(onEscape);
  handlerRef.current = onEscape;
  useEffect(() => {
    if (!active) return;
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') handlerRef.current();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [active]);
}

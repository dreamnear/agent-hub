// Beautiful UI 备用组件依赖桩（批5）：官网 atoms/StreamText 的最小等价实现
// （按字符流出 text，onProgress 进度回调，onDone 完成）。
import { useEffect, useState, type ReactElement } from 'react';

export function StreamText({
  text,
  onProgress,
  onDone,
  className = '',
}: {
  text: string;
  onProgress?: (ratio: number) => void;
  onDone?: () => void;
  className?: string;
}): ReactElement {
  const [n, setN] = useState(0);
  useEffect(() => {
    setN(0);
  }, [text]);
  useEffect(() => {
    if (n >= text.length) {
      onDone?.();
      return;
    }
    const t = setTimeout(() => {
      setN((c) => {
        const next = Math.min(c + 1, text.length);
        onProgress?.(next / Math.max(1, text.length));
        return next;
      });
    }, 24);
    return () => clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [n, text]);
  return <span className={className}>{text.slice(0, n)}</span>;
}

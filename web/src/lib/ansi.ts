// ANSI SGR 子集解析（P5 C5）：转义序列 → span 段，零依赖。
// 支持：前景 30-37/90-97、背景 40-47/100-107、bold、reset（0=全清、49=清背景）。其余 SGR 忽略。

export interface AnsiSegment {
  text: string;
  fg?: string;
  bg?: string;
  bold?: boolean;
}

const FG = [
  'oklch(20% 0 0)',
  'oklch(45% 0.18 25)',
  'oklch(45% 0.15 145)',
  'oklch(55% 0.14 85)',
  'oklch(45% 0.18 260)',
  'oklch(48% 0.16 300)',
  'oklch(45% 0.12 195)',
  'oklch(60% 0.01 0)',
];
const FG_BRIGHT = [
  'oklch(55% 0.01 0)', // 90 亮黑
  'oklch(55% 0.2 25)',
  'oklch(60% 0.17 145)',
  'oklch(70% 0.15 90)',
  'oklch(62% 0.16 260)',
  'oklch(62% 0.17 300)',
  'oklch(60% 0.14 195)',
  'oklch(80% 0.01 0)', // 97 亮白
];
// 标准 SGR 背景八色：40 黑 … 47 白（下标与 40-47/100-107 一一对应，此前错位一格）
const BG = [
  'oklch(25% 0.01 0)', // 黑
  'darkred',
  'darkgreen',
  'darkorange',
  'darkblue',
  'darkmagenta',
  'darkcyan',
  'oklch(80% 0.01 0)', // 白（浅灰适配浅色 UI）
];

/// 解析含 ANSI 转义的文本为渲染段；无转义时返回单段原文。
export function parseAnsi(text: string): AnsiSegment[] {
  if (!text.includes('\x1b[')) return [{ text }];
  const segments: AnsiSegment[] = [];
  let fg: string | undefined;
  let bg: string | undefined;
  let bold = false;
  let buf = '';

  const flush = (): void => {
    if (buf) segments.push({ text: buf, fg, bg, bold });
    buf = '';
  };

  for (let i = 0; i < text.length; ) {
    if (text[i] === '\x1b' && text[i + 1] === '[') {
      const end = text.indexOf('m', i + 2);
      if (end === -1) {
        // 序列被截断：缓冲内容照常输出
        flush();
        buf += text.slice(i);
        flush();
        break;
      }
      flush();
      for (const code of text.slice(i + 2, end).split(';')) {
        const n = parseInt(code || '0', 10);
        if (n === 0) {
          fg = bg = undefined;
          bold = false;
        } else if (n === 1) bold = true;
        else if (n === 39) fg = undefined;
        else if (n === 49) bg = undefined;
        else if (n === 22) bold = false;
        else if (n >= 30 && n <= 37) fg = FG[n - 30];
        else if (n >= 90 && n <= 97) fg = FG_BRIGHT[n - 90];
        else if (n >= 40 && n <= 47) bg = BG[n - 40];
        else if (n >= 100 && n <= 107) bg = BG[n - 100];
      }
      i = end + 1;
    } else {
      buf += text[i];
      i += 1;
    }
  }
  flush();
  return segments.length > 0 ? segments : [{ text }];
}

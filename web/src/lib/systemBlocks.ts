// 系统通知块切分：harness 注入的裸文本 XML 块（<task-notification>/<system-reminder>，
// 新形态往 SYS_TAGS 加一行）→ 独立 sys 段，UI 折叠成摘要条。只处理裸文本区——
// 围栏代码块与行内代码内的同标签原样保留（防误伤用户代码）；块未闭合（流式半截）
// 按普通文本不折叠。ponytail: 不识别 ~~~ 围栏与 4 空格缩进代码块，出现误伤再扩 PROTECTED_RE。

export interface SystemBlockSegment {
  kind: 'text' | 'sys';
  /// text 段原文 / sys 段完整原文（含标签，展开摘要条可见）
  text: string;
  /// sys 段标签名（如 task-notification）
  tag?: string;
  /// sys 段摘要：块内 <summary> 文本，缺省「系统通知」
  label?: string;
}

const SYS_TAGS = ['task-notification', 'system-reminder'];
const PROTECTED_RE = /```[\s\S]*?```|`[^`\n]*`/g;

/// 消息文本 → 段数组。无系统块（含只在代码里出现的）时返回原文本单段（快路径）。
export function splitSystemBlocks(text: string): SystemBlockSegment[] {
  if (!SYS_TAGS.some((t) => text.includes(`<${t}>`))) return [{ kind: 'text', text }];

  const segs: SystemBlockSegment[] = [];
  // 保护区（代码）先挖出原样回填，系统块只在裸文本区找
  let last = 0;
  let m: RegExpExecArray | null;
  PROTECTED_RE.lastIndex = 0;
  const pushPlain = (raw: string): void => {
    let pos = 0; // 已定为普通文本的游标（未闭合块不前移，字符不丢）
    let scan = 0; // 扫描游标
    while (scan < raw.length) {
      const open = earliestOpen(raw, scan);
      if (open == null) break;
      const close = `</${open.tag}>`;
      const closeIdx = raw.indexOf(close, open.end);
      if (closeIdx === -1) {
        scan = open.end; // 流式半截：按普通文本，跳过该开标签继续扫
        continue;
      }
      segs.push({ kind: 'text', text: raw.slice(pos, open.start) });
      const body = raw.slice(open.end, closeIdx);
      const summary = /<summary>([\s\S]*?)<\/summary>/.exec(body)?.[1].trim();
      segs.push({
        kind: 'sys',
        text: raw.slice(open.start, closeIdx + close.length),
        tag: open.tag,
        label: summary || '系统通知',
      });
      pos = scan = closeIdx + close.length;
    }
    const rest = raw.slice(pos);
    if (rest) segs.push({ kind: 'text', text: rest });
  };
  while ((m = PROTECTED_RE.exec(text)) !== null) {
    pushPlain(text.slice(last, m.index));
    segs.push({ kind: 'text', text: m[0] });
    last = m.index + m[0].length;
  }
  pushPlain(text.slice(last));

  // 无 sys 段 → 原文单段（代码里出现同标签等场景，渲染零变化）；
  // 有 sys 段时丢空白 text 段（块间换行）防渲染空节点
  if (!segs.some((s) => s.kind === 'sys')) return [{ kind: 'text', text }];
  return segs.filter((s) => s.kind === 'sys' || s.text.trim() !== '');
}

/// 裸文本区里最早出现的系统块开标签；无则 null
function earliestOpen(raw: string, from: number): { tag: string; start: number; end: number } | null {
  let best: { tag: string; start: number; end: number } | null = null;
  for (const tag of SYS_TAGS) {
    const idx = raw.indexOf(`<${tag}>`, from);
    if (idx !== -1 && (best == null || idx < best.start)) best = { tag, start: idx, end: idx + tag.length + 2 };
  }
  return best;
}

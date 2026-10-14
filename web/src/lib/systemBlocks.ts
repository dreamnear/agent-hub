// 系统通知块切分（2026-10-02 通用检测 + 语义映射）：harness 注入的裸文本 XML 块
// → 独立 sys 段，UI 折叠/语义化渲染。检测从白名单制升级为通用 XML 块检测：
// 消息整体呈 XML 块结构（移除所有良构块后余白）→ 一律进系统块管线，将来冒新
// tag 自动兜住；白名单语义（task-notification 隐藏 / system-reminder 折叠 /
// total_tokens 混排折叠）保持不变。
// 防误伤护栏：围栏代码块/行内代码内的同标签原样保留（防误伤用户代码）；块未
// 闭合（流式半截）按普通文本不折叠；白名单之外的消息（散文夹尖括号）不走通用
// 管线，原文渲染。
// ponytail: 不识别 ~~~ 围栏与 4 空格缩进代码块，出现误伤再扩 PROTECTED_RE。
import { t } from '../i18n';

export interface SystemBlockSegment {
  kind: 'text' | 'sys' | 'command' | 'stdout';
  /// text 段原文 / 其余段完整原文（含标签，展开可见）
  text: string;
  /// sys 段标签名（如 task-notification）
  tag?: string;
  /// sys 段摘要：块内 <summary> 文本，缺省「系统通知」；command 段=命令行
  ///（如 /effort xhigh）；stdout 段=首行预览
  label?: string;
}

// 白名单语义：task-notification 隐藏、system-reminder 折叠、total_tokens 混排折叠
const SYS_TAGS = ['task-notification', 'system-reminder', 'total_tokens'];
const PROTECTED_RE = /```[\s\S]*?```|`[^`\n]*`/g;
// 通用 XML 块：简单开标签（无属性——harness 系统块不带属性）+ 匹配闭标签
const XML_OPEN_RE = /<([a-zA-Z][a-zA-Z0-9_-]*)>/g;

interface XmlBlock {
  tag: string;
  start: number;
  end: number; // 开标签结束位置
  close: number; // 闭标签结束位置
}

/// 全串良构 XML 块扫描（保护区内的开/闭标签不算；未闭合不算）。
function scanBlocks(text: string): XmlBlock[] {
  const protectedRanges: [number, number][] = [];
  PROTECTED_RE.lastIndex = 0;
  let m: RegExpExecArray | null;
  while ((m = PROTECTED_RE.exec(text)) !== null) {
    protectedRanges.push([m.index, m.index + m[0].length]);
  }
  const inProtected = (i: number): boolean =>
    protectedRanges.some(([a, b]) => i >= a && i < b);

  const blocks: XmlBlock[] = [];
  XML_OPEN_RE.lastIndex = 0;
  while ((m = XML_OPEN_RE.exec(text)) !== null) {
    const tag = m[1];
    const start = m.index;
    const end = m.index + m[0].length;
    if (inProtected(start)) continue;
    const closeTag = `</${tag}>`;
    const close = text.indexOf(closeTag, end);
    if (close === -1 || inProtected(close)) continue;
    blocks.push({ tag, start, end, close: close + closeTag.length });
    XML_OPEN_RE.lastIndex = close + closeTag.length; // 块体内部不重复扫
  }
  return blocks;
}

/// 移除所有块后的剩余文本（「消息主体是 XML 块」判定：剩余为纯空白）。
function remainderOutside(text: string, blocks: XmlBlock[]): string {
  let out = '';
  let pos = 0;
  for (const b of blocks) {
    out += text.slice(pos, b.start);
    pos = b.close;
  }
  out += text.slice(pos);
  return out;
}

/// 消息文本 → 段数组。
/// 通用模式（全部块移除后仅剩空白）：所有块进系统管线——command 三件套合并为
/// 紧凑执行段、local-command-stdout 归执行结果段、其余 tag 兜底 sys 段。
/// 混排模式（散文+块）：仅白名单 tag 折叠（既有语义不变），其余原样文本。
export function splitSystemBlocks(text: string): SystemBlockSegment[] {
  const blocks = scanBlocks(text);
  if (blocks.length === 0) return [{ kind: 'text', text }];

  const universal = remainderOutside(text, blocks).trim() === '';
  if (universal) return universalSegments(text, blocks);

  // 混排模式：白名单折叠（既有语义），非白名单块当普通文本（游标不越过）
  const segs: SystemBlockSegment[] = [];
  let pos = 0;
  for (const b of blocks) {
    if (!SYS_TAGS.includes(b.tag)) continue;
    segs.push({ kind: 'text', text: text.slice(pos, b.start) });
    const bodyText = bodyOf(text, b);
    const summary = /<summary>([\s\S]*?)<\/summary>/.exec(bodyText)?.[1].trim();
    segs.push({
      kind: 'sys',
      text: text.slice(b.start, b.close),
      tag: b.tag,
      label: summary || t('sys.defaultLabel'),
    });
    pos = b.close;
  }
  const rest = text.slice(pos);
  if (rest) segs.push({ kind: 'text', text: rest });
  if (!segs.some((s) => s.kind === 'sys')) return [{ kind: 'text', text }];
  return segs.filter((s) => s.kind !== 'text' || s.text.trim() !== '');
}

/// 通用模式段构造：command 三件套（command-name/message/args）合并为紧凑执行段，
/// local-command-stdout 归执行结果段，其余 tag 兜底 sys 段。块间空白段丢弃。
function universalSegments(text: string, blocks: XmlBlock[]): SystemBlockSegment[] {
  const out: SystemBlockSegment[] = [];
  let i = 0;
  while (i < blocks.length) {
    const b = blocks[i];
    if (b.tag === 'command-name') {
      // 三件套连续块合并：name/message/args → ⌘ 命令行
      const trio = blocks.slice(i, i + 3);
      const names = new Set(trio.map((x) => x.tag));
      if (
        trio.length === 3 &&
        names.has('command-message') &&
        names.has('command-args')
      ) {
        const raw = text.slice(trio[0].start, trio[2].close);
        const cmd = bodyOf(text, trio.find((x) => x.tag === 'command-name')!);
        const args = bodyOf(text, trio.find((x) => x.tag === 'command-args')!);
        out.push({
          kind: 'command',
          text: raw,
          label: `${cmd} ${args}`.trim(),
        });
        i += 3;
        continue;
      }
    }
    if (b.tag === 'local-command-stdout') {
      const firstLine = bodyOf(text, b).split('\n').find((l) => l.trim() !== '') ?? '';
      out.push({ kind: 'stdout', text: text.slice(b.start, b.close), label: firstLine.trim() });
      i += 1;
      continue;
    }
    const bodyText = bodyOf(text, b);
    const summary = /<summary>([\s\S]*?)<\/summary>/.exec(bodyText)?.[1].trim();
    out.push({
      kind: 'sys',
      text: text.slice(b.start, b.close),
      tag: b.tag,
      label: summary || t('sys.defaultLabel'),
    });
    i += 1;
  }
  return out;
}

/// 块体文本（开闭标签之间）。
function bodyOf(text: string, b: XmlBlock): string {
  return text.slice(b.end, b.close - b.tag.length - 3);
}

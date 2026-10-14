/**
 * slash 菜单模糊查找（agent-hub-slash-skills round 2 人审，S5）：
 * 输入字符序列对 name 做大小写不敏感子序列匹配（TUI 式）。
 *
 * 排序权重（比分越高越优，同分时短名优先、字典序收尾）：
 * 前缀 > 连续 > 词首（`:`/`-`/`_`/`/` 边界或串首）> 命中位置靠前。
 * 乱序/缺字符（子序列不成立）返回 null = 不命中，调用方过滤。
 * 空查询返回 0（不淘汰、不排序）——调用方据此回落全量清单。
 */

/** 单字符给分：连续 +20 / 词首 +12 / 靠前最多 +30 / 基础 10 */
const BASE = 10;
const CONTIGUOUS = 20;
const WORD_START = 12;
const NEAR_FRONT = 30;
/** 前缀（query 从 name 首位起连续命中）整体加成，保证前缀匹配排最前 */
const PREFIX_BONUS = 100;

export function fuzzyScore(name: string, query: string): number | null {
  const n = name.toLowerCase();
  const q = query.toLowerCase();
  if (q === '') return 0;
  let score = 0;
  let ni = 0;
  let prevIdx = -2;
  let allContiguous = true;
  for (const ch of q) {
    const idx = n.indexOf(ch, ni);
    if (idx < 0) return null;
    const contiguous = idx === prevIdx + 1;
    if (!contiguous) allContiguous = false;
    const wordStart = idx === 0 || /[:-_/]/.test(n[idx - 1] ?? '');
    score += BASE + (contiguous ? CONTIGUOUS : 0) + (wordStart ? WORD_START : 0) + Math.max(0, NEAR_FRONT - idx);
    prevIdx = idx;
    ni = idx + 1;
  }
  // 前缀判定：从首位起连续命中整个 query
  const isPrefix = n.startsWith(q) && allContiguous;
  return score + (isPrefix ? PREFIX_BONUS : 0);
}

/** slash 候选的模糊过滤+排序（空查询回落全量原序，调用方后续截 50）。 */
export function fuzzyFilter<T extends { name: string }>(items: T[], query: string): T[] {
  if (query === '') return items;
  return items
    .map((item) => ({ item, score: fuzzyScore(item.name, query) }))
    .filter((x): x is { item: T; score: number } => x.score != null)
    .sort(
      (a, b) =>
        b.score - a.score ||
        a.item.name.length - b.item.name.length ||
        a.item.name.localeCompare(b.item.name),
    )
    .map((x) => x.item);
}

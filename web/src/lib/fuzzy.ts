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

/**
 * 求值缓存（agent-hub-commands-perf P1）：键 = 清单数组引用 + query 字符串，
 * 值 = 结果数组（只存既有元素引用，不复制对象）。外层 WeakMap 挂在清单引用上，
 * 清单被替换（重新拉取）后旧缓存随引用一起可回收。菜单开着时组件因消息流等
 * 无关原因重渲染，同 (清单引用, query) 的重复求值退化为一次 Map 查找。
 *
 * 取舍（落点 lib 层而非 hook 层 useMemo）：
 * 1. memo 语义可纯单测证明（同引用+同查询 → 结果引用相等），不依赖 React；
 * 2. 覆盖一切调用方，未来新入口（命令面板等）自动获得缓存；
 * 3. hook 层 useMemo 会被 `query == null` 提前返回破坏 hooks 顺序（须重构），
 *    且 memo 生命周期绑死单个 hook 实例。
 *
 * 契约：items 与返回数组视为不可变——原地修改后再查会命中陈旧缓存（现调用点
 * 每次拉取都替换新数组、消费侧只 filter/slice/map，满足）。每清单缓存查询数
 * 上限 MAX_CACHED_QUERIES，超限整体清空（防长会话输入膨胀；简单清空即够，
 * 命中率敏感场景再升级 LRU）。
 */
const memoCache = new WeakMap<object, Map<string, unknown[]>>();
const MAX_CACHED_QUERIES = 32;

/** slash 候选的模糊过滤+排序（空查询回落全量原序，调用方后续截 50）。
 * 重复求值（同清单引用+同查询）命中缓存直接返回同一结果引用。 */
export function fuzzyFilter<T extends { name: string }>(items: T[], query: string): T[] {
  if (query === '') return items;
  let queries = memoCache.get(items);
  if (queries === undefined) {
    queries = new Map();
    memoCache.set(items, queries);
  } else {
    const hit = queries.get(query);
    if (hit !== undefined) return hit as T[];
  }
  const result = items
    .map((item) => ({ item, score: fuzzyScore(item.name, query) }))
    .filter((x): x is { item: T; score: number } => x.score != null)
    .sort(
      (a, b) =>
        b.score - a.score ||
        a.item.name.length - b.item.name.length ||
        a.item.name.localeCompare(b.item.name),
    )
    .map((x) => x.item);
  if (queries.size >= MAX_CACHED_QUERIES) queries.clear();
  queries.set(query, result);
  return result;
}

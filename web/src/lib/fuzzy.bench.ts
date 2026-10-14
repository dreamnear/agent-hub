/**
 * fuzzyFilter 基准脚本（agent-hub-commands-perf P3）——零依赖，node 直跑：
 *   cd web && npm run bench
 * （= node --experimental-strip-types src/lib/fuzzy.bench.ts，node ≥22.6）
 *
 * 三路对照（合成 2000 条清单，query 'code'）：
 * - uncached：改动前算法快照（本文件内联副本，map+filter+sort 全量求值；勿与
 *   lib 同步——它就是「无缓存」基线）
 * - cold：现行 fuzzyFilter 每次迭代换新 query（必 miss）= 击键路径单次求值
 * - warm：同 (清单引用, query) 重复求值 = 组件重渲染路径（命中缓存）
 *
 * 结论口径：cold ≈ uncached（缓存不劣化击键冷路径）；warm 相对 uncached 的
 * 倍率即重复求值收益。数字如实打印，由报告引用。
 */
import { fuzzyFilter } from './fuzzy.ts';

/** 改动前算法快照（权重与 fuzzy.ts 一致），仅供 bench 对照。 */
function fuzzyFilterUncached<T extends { name: string }>(items: T[], query: string): T[] {
  function score(name: string, q: string): number | null {
    const n = name.toLowerCase();
    const qq = q.toLowerCase();
    if (qq === '') return 0;
    let s = 0;
    let ni = 0;
    let prev = -2;
    let allContiguous = true;
    for (const ch of qq) {
      const idx = n.indexOf(ch, ni);
      if (idx < 0) return null;
      const contiguous = idx === prev + 1;
      if (!contiguous) allContiguous = false;
      const wordStart = idx === 0 || /[:-_/]/.test(n[idx - 1] ?? '');
      s += 10 + (contiguous ? 20 : 0) + (wordStart ? 12 : 0) + Math.max(0, 30 - idx);
      prev = idx;
      ni = idx + 1;
    }
    return s + (n.startsWith(qq) && allContiguous ? 100 : 0);
  }
  if (query === '') return items;
  return items
    .map((item) => ({ item, score: score(item.name, query) }))
    .filter((x): x is { item: T; score: number } => x.score != null)
    .sort(
      (a, b) =>
        b.score - a.score ||
        a.item.name.length - b.item.name.length ||
        a.item.name.localeCompare(b.item.name),
    )
    .map((x) => x.item);
}

const VERBS = [
  'review',
  'fix',
  'plan',
  'test',
  'deploy',
  'build',
  'scan',
  'refactor',
  'explain',
  'translate',
];
const NOUNS = [
  'code',
  'docs',
  'tests',
  'config',
  'security',
  'performance',
  'ui',
  'api',
  'database',
  'cache',
];

/** 合成清单：namespace:verb-noun-i 形态，长度与词形贴近真实 slash 命令。 */
function makeList(size: number): { name: string; source: string; description: string | null }[] {
  const items: { name: string; source: string; description: string | null }[] = [];
  for (let i = 0; i < size; i += 1) {
    const verb = VERBS[i % VERBS.length];
    const noun = NOUNS[Math.floor(i / VERBS.length) % NOUNS.length];
    items.push({ name: `skill-${i % 7}:${verb}-${noun}-${i}`, source: 'skill', description: null });
  }
  return items;
}

const LIST = makeList(2000);
const QUERY = 'code';
const RUNS = 201;

// 健全性：缓存路径与不缓存快照结果必须一致（防基准测两套算法），否则退出非零
const viaCache = JSON.stringify(fuzzyFilter(LIST, QUERY).map((x) => x.name));
const viaSnapshot = JSON.stringify(fuzzyFilterUncached(LIST, QUERY).map((x) => x.name));
if (viaCache !== viaSnapshot) {
  console.error('FATAL: cached fuzzyFilter diverges from uncached snapshot');
  process.exit(1);
}

/** 一批调用的均摊单次耗时（µs）。 */
function perCallUs(batch: number, fn: (i: number) => void): number {
  const t0 = performance.now();
  for (let i = 0; i < batch; i += 1) fn(i);
  return ((performance.now() - t0) * 1000) / batch;
}

function median(xs: number[]): number {
  const sorted = [...xs].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)];
}

// JIT 预热 + 预置 warm 条目（正式计时前完成，不计入样本）
for (let i = 0; i < 50; i += 1) {
  fuzzyFilterUncached(LIST, QUERY);
  fuzzyFilter(LIST, `warm${i}`);
}
fuzzyFilter(LIST, QUERY);

let coldSeq = 0;
const uncachedRuns: number[] = [];
const coldRuns: number[] = [];
const warmRuns: number[] = [];
for (let i = 0; i < RUNS; i += 1) {
  uncachedRuns.push(perCallUs(1, () => fuzzyFilterUncached(LIST, QUERY)));
  coldRuns.push(
    perCallUs(1, () => {
      coldSeq += 1;
      return fuzzyFilter(LIST, `code${coldSeq % 100000}`);
    }),
  );
  warmRuns.push(perCallUs(2000, () => fuzzyFilter(LIST, QUERY)));
}

const uncachedUs = median(uncachedRuns);
const coldUs = median(coldRuns);
const warmUs = median(warmRuns);

console.log(`fuzzyFilter benchmark — list ${LIST.length} items, query "${QUERY}"`);
console.log(`node ${process.version}, ${RUNS} runs per scenario, median per-eval time`);
console.log('');
console.log('scenario                                | per-eval | vs uncached');
console.log('----------------------------------------|----------|------------');
console.log(
  `uncached full recompute (pre-change)    | ${uncachedUs.toFixed(2).padStart(7)}us | 1x`,
);
console.log(
  `cold eval, new query (keystroke path)   | ${coldUs.toFixed(2).padStart(7)}us | ${(coldUs / uncachedUs).toFixed(2)}x`,
);
console.log(
  `warm repeat eval (re-render, cache hit) | ${warmUs.toFixed(2).padStart(7)}us | ${(warmUs / uncachedUs).toFixed(4)}x`,
);
console.log('');
console.log(
  `repeat-eval speedup (uncached / warm): ${(uncachedUs / warmUs).toFixed(0)}x on same list+query`,
);

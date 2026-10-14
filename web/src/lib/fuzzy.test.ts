import { describe, expect, it } from 'vitest';
import { fuzzyFilter, fuzzyScore } from './fuzzy';

describe('fuzzyScore（slash 菜单模糊查找，round 2 人审 S5）', () => {
  it('子序列命中：anys 命中 anysearch', () => {
    expect(fuzzyScore('anysearch', 'anys')).not.toBeNull();
  });

  it('前缀优先：前缀命中的名字排在非前缀子序列命中之前', () => {
    const ranked = fuzzyFilter(
      [{ name: 'anysearch' }, { name: 'search-any' }, { name: 'analytics' }],
      'any',
    );
    // 'anysearch' 前缀命中应排最前；'analytics' 词首子序列次之；'search-any' 末位
    expect(ranked[0]?.name).toBe('anysearch');
    expect(ranked).toHaveLength(3);
  });

  it('乱序不命中：字符齐全但顺序颠倒不命中（om 对 model）', () => {
    expect(fuzzyScore('model', 'om')).toBeNull();
    // 缺字符同样不命中：eccrv 对 anysearch（无第二 c 与 v）
    expect(fuzzyScore('anysearch', 'eccrv')).toBeNull();
  });

  it('大小写不敏感：ANY 命中 anysearch，得分与 any 一致', () => {
    expect(fuzzyScore('anysearch', 'ANY')).not.toBeNull();
    expect(fuzzyScore('anysearch', 'ANY')).toBe(fuzzyScore('anysearch', 'any'));
  });

  it('空输入回落：空查询返回 0（不淘汰不排序），调用方回落全量', () => {
    expect(fuzzyScore('anysearch', '')).toBe(0);
    const items = [{ name: 'b' }, { name: 'a' }];
    expect(fuzzyFilter(items, '')).toEqual(items); // 原序回落
  });

  it('连续/词首优于离散：命令 ns:name 的词首字符加权', () => {
    // 'ecc:code-review' 输 ecc：c 命中词首（: 后）应得高分
    const s = fuzzyScore('ecc:code-review', 'ecc');
    expect(s).not.toBeNull();
    expect(s!).toBeGreaterThan(100); // 前缀加成生效
  });

  it('短名优先：同分（同前缀命中）时短名排前', () => {
    const ranked = fuzzyFilter([{ name: 'model-extended' }, { name: 'model' }], 'mod');
    expect(ranked[0]?.name).toBe('model');
  });
});

describe('fuzzyFilter 求值缓存（agent-hub-commands-perf P1 memo 语义）', () => {
  it('memo 命中：相同清单引用 + 相同查询返回同一结果引用（不重算）', () => {
    const items = [{ name: 'alpha' }, { name: 'alphabet' }, { name: 'beta' }];
    const first = fuzzyFilter(items, 'al');
    const second = fuzzyFilter(items, 'al');
    expect(second).toBe(first); // 引用相等 = 命中缓存，未重算
    expect(second.map((x) => x.name)).toEqual(['alpha', 'alphabet']);
  });

  it('memo 键含查询：同清单不同查询各自重算，互不串值', () => {
    const items = [{ name: 'alpha' }, { name: 'beta' }];
    const byAl = fuzzyFilter(items, 'al');
    const byBe = fuzzyFilter(items, 'be');
    expect(byBe).not.toBe(byAl);
    expect(byAl.map((x) => x.name)).toEqual(['alpha']);
    expect(byBe.map((x) => x.name)).toEqual(['beta']);
  });

  it('memo 键含清单引用：不同清单同查询互不串值', () => {
    const itemsA = [{ name: 'alpha' }];
    const itemsB = [{ name: 'beta' }, { name: 'alphabet' }];
    const fromA = fuzzyFilter(itemsA, 'al');
    const fromB = fuzzyFilter(itemsB, 'al');
    expect(fromA).not.toBe(fromB);
    expect(fromA.map((x) => x.name)).toEqual(['alpha']);
    expect(fromB.map((x) => x.name)).toEqual(['alphabet']); // beta 无 'l'，不命中
  });

  it('缓存命中与首次求值语义一致：换新清单引用重算结果内容相等', () => {
    const items = Array.from({ length: 200 }, (_, i) => ({ name: `cmd-${i}-filter` }));
    const cold = fuzzyFilter(items, 'cmd');
    expect(fuzzyFilter(items, 'cmd')).toBe(cold); // 命中
    expect(fuzzyFilter([...items], 'cmd')).toEqual(cold); // 新引用重算，内容一致
  });

  it('空查询不进缓存：始终原样返回输入引用（回落语义不变）', () => {
    const items = [{ name: 'alpha' }];
    expect(fuzzyFilter(items, '')).toBe(items);
    expect(fuzzyFilter(items, '')).toBe(items);
  });

  it('查询数超上限清空缓存后仍正确（防长会话膨胀，不串值）', () => {
    const items = [{ name: 'alpha' }, { name: 'beta' }];
    for (let i = 0; i < 40; i += 1) fuzzyFilter(items, `q${i}`); // 超过 MAX_CACHED_QUERIES=32
    const again = fuzzyFilter(items, 'al'); // 早期条目已被清空 → 重算
    expect(again.map((x) => x.name)).toEqual(['alpha']);
  });
});

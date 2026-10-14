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

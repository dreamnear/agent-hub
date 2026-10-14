// @vitest-environment happy-dom
import { describe, expect, it } from 'vitest';
import { parseAnsi } from './ansi';

describe('parseAnsi', () => {
  it('returns single segment when no escape codes', () => {
    expect(parseAnsi('plain text')).toEqual([{ text: 'plain text' }]);
  });

  it('splits colored segments', () => {
    const segs = parseAnsi('\x1b[31merror\x1b[0m plain');
    expect(segs).toHaveLength(2);
    expect(segs[0].text).toBe('error');
    expect(segs[0].fg).toContain('25'); // red hue
    expect(segs[1].text).toBe(' plain');
    expect(segs[1].fg).toBeUndefined();
  });

  it('handles bold and bright colors', () => {
    const segs = parseAnsi('\x1b[1;92mbright green');
    expect(segs[0].bold).toBe(true);
    expect(segs[0].fg).toBeDefined();
  });

  it('renders truncated sequences as literal text', () => {
    const segs = parseAnsi('text \x1b[31');
    const joined = segs.map((s) => s.text).join('');
    expect(joined).toBe('text \x1b[31');
  });

  it('maps standard 8 background colors without index shift', () => {
    // 40=黑背景（此前错位取到红），47=白背景（此前越界 undefined）
    expect(parseAnsi('\x1b[40mbk')[0].bg).not.toBe('darkred');
    expect(parseAnsi('\x1b[47mbk')[0].bg).toBeDefined();
    // 49 清背景、90=亮黑（此前错位取到红）
    expect(parseAnsi('\x1b[44mxx\x1b[49mplain')[1].bg).toBeUndefined();
    expect(parseAnsi('\x1b[90mgrey')[0].fg).not.toContain('25');
  });
});

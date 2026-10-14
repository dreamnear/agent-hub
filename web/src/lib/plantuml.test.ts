// @vitest-environment happy-dom
// PlantUML 渲染服务接入：~h hex 编码、服务地址读写、未配置回退 null。
import { afterEach, describe, expect, it } from 'vitest';
import { plantumlBase, plantumlHex, plantumlSvgUrl, setPlantumlBase } from './plantuml';

afterEach(() => {
  setPlantumlBase('');
});

describe('plantumlHex', () => {
  it('ASCII 逐字节两位 hex', () => {
    expect(plantumlHex('AB')).toBe('4142');
  });

  it('中文等多字节走 UTF-8（TextEncoder 口径）', () => {
    // 「删」 UTF-8 = E5 88 A0
    expect(plantumlHex('删')).toBe('e588a0');
    expect(plantumlHex('a删')).toBe('61e588a0');
  });
});

describe('plantumlBase / plantumlSvgUrl', () => {
  it('未配置 → null（安全缺省：文档图不默认发外部）', () => {
    expect(plantumlBase()).toBe('');
    expect(plantumlSvgUrl('@startuml\nA->B\n@enduml')).toBeNull();
  });

  it('配置后拼 /svg/~h<hex>；尾部斜杠归一', () => {
    setPlantumlBase('https://puml.example.com/plantuml/');
    expect(plantumlBase()).toBe('https://puml.example.com/plantuml');
    expect(plantumlSvgUrl('AB')).toBe('https://puml.example.com/plantuml/svg/~h4142');
  });

  it('纯空白等同未配置', () => {
    setPlantumlBase('   ');
    expect(plantumlSvgUrl('AB')).toBeNull();
  });
});

// @vitest-environment happy-dom
import { describe, expect, it } from 'vitest';
import { splitImageRefs, imageUrlOf } from './image';

const up = '/var/folders/jf/T/claude-view-uploads/e54054b6-48fe-4a42.png';

describe('splitImageRefs', () => {
  it('单图：收敛为 [Image #1] 胶囊段，路径保留在 path', () => {
    const segs = splitImageRefs(`试试贴图，看看这张图片说了什么\n@${up}`);
    expect(segs).toEqual([
      { kind: 'text', text: '试试贴图，看看这张图片说了什么\n' },
      { kind: 'image', text: '[Image #1]', path: up },
    ]);
  });

  it('多图：按出现顺序编号 1..N，大小写不敏感（.JPG/.WebP 命中）', () => {
    const segs = splitImageRefs(
      '@/tmp/claude-view-uploads/a.webp 中间文本 @/tmp/claude-view-uploads/b.JPG',
    );
    expect(segs.filter((s) => s.kind === 'image').map((s) => s.text)).toEqual([
      '[Image #1]',
      '[Image #2]',
    ]);
    expect(segs.some((s) => s.kind === 'text' && s.text === ' 中间文本 ')).toBe(true);
  });

  it('无图：原样返回单段文本', () => {
    expect(splitImageRefs('普通消息，无引用')).toEqual([
      { kind: 'text', text: '普通消息，无引用' },
    ]);
  });

  it('非上传路径的 @文本不误伤（@提及、非 uploads 目录、非图片扩展名）', () => {
    const t = '@张三 看下 @/tmp/foo.png 和 @/x/claude-view-uploads/readme.md';
    expect(splitImageRefs(t)).toEqual([{ kind: 'text', text: t }]);
  });

  it('r72 宽泛匹配：无 @ 前缀的裸路径（Read 结果/工具 JSON）同样命中', () => {
    const segs = splitImageRefs(
      '已读取图片 {"file_path":"/tmp/claude-view-uploads/x.png"}',
    );
    expect(segs.some((s) => s.kind === 'image' && s.path === '/tmp/claude-view-uploads/x.png')).toBe(
      true,
    );
    // JSON 转义引号紧贴扩展名不误吞
    expect(segs.find((s) => s.kind === 'image')?.path).not.toContain('"');
  });
});

describe('r72b CLI 图片引用形态', () => {
  const tmpImg = '/private/tmp/claude-tmp/776cecd0-aa7c-4cf9/images/22.png';

  it('assistant 括号形态：整串消费只取路径，不留 [Image: source: ] 残壳', () => {
    const segs = splitImageRefs(`看这张图 [Image: source: ${tmpImg}]`);
    expect(segs).toEqual([
      { kind: 'text', text: '看这张图 ' },
      { kind: 'image', text: '[Image #1]', path: tmpImg },
    ]);
  });

  it('user 气泡裸 claude-tmp 路径命中（/tmp 与 /private/tmp 两形态）', () => {
    const a = splitImageRefs(`图在这 /tmp/claude-tmp/s1/images/3.png 请看`);
    expect(a.find((s) => s.kind === 'image')?.path).toBe('/tmp/claude-tmp/s1/images/3.png');
    const b = splitImageRefs(`图在这 ${tmpImg} 请看`);
    expect(b.find((s) => s.kind === 'image')?.path).toBe(tmpImg);
  });

  it('[Image #22] 无路径占位保持纯文本（无 src 可取，不猜路径）', () => {
    expect(splitImageRefs('这两个订单都是合并订单的一部分 [Image #22]')).toEqual([
      { kind: 'text', text: '这两个订单都是合并订单的一部分 [Image #22]' },
    ]);
  });

  it('白名单外括号形态：命中 image 段但 imageUrlOf 为 null（回落 pill）', () => {
    const segs = splitImageRefs('[Image: source: /Users/x/report.png]');
    expect(segs).toHaveLength(1);
    expect(segs[0].kind).toBe('image');
    expect(segs[0].path).toBe('/Users/x/report.png');
    expect(imageUrlOf('/Users/x/report.png')).toBeNull();
  });

  it('imageUrlOf：claude-tmp 路径 → by-path 端点（path 编码透传）', () => {
    expect(imageUrlOf(tmpImg)).toBe(
      '/api/images/by-path?path=%2Fprivate%2Ftmp%2Fclaude-tmp%2F776cecd0-aa7c-4cf9%2Fimages%2F22.png',
    );
    expect(imageUrlOf('/tmp/claude-tmp/s1/images/3.png')).toBe(
      '/api/images/by-path?path=%2Ftmp%2Fclaude-tmp%2Fs1%2Fimages%2F3.png',
    );
  });

  it('imageUrlOf：claude-tmp-evil 前缀混淆与非 tmp 路径 → null', () => {
    expect(imageUrlOf('/tmp/claude-tmp-evil/x.png')).toBeNull();
    expect(imageUrlOf('/var/folders/other/x.png')).toBeNull();
    expect(imageUrlOf('/private/tmp/claude-tmp/readme.md')).toBeNull();
  });
});

describe('imageUrlOf', () => {
  it('uploads 图片路径 → /api/images/<落盘名>（无 token 时 URL 干净）', () => {
    expect(imageUrlOf(up)).toBe('/api/images/e54054b6-48fe-4a42.png');
  });

  it('非 uploads 路径/非图片扩展名 → null（渲染层回落 pill）', () => {
    expect(imageUrlOf('/tmp/foo.png')).toBeNull();
    expect(imageUrlOf('/x/claude-view-uploads/readme.md')).toBeNull();
    expect(imageUrlOf('')).toBeNull();
  });
});

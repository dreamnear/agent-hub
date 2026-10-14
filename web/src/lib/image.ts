// C3 图片辅助：base64 编码与超限/HEIC 压图（canvas → jpeg）。
// 以及已发送消息的上传图片路径 → [Image #n] 占位/缩略图收敛（纯展示层：
// 底层文本与注入 CLI 的 @引用链路不动，路径仍随消息数据保留）。

import { resourceTokenQuery } from '../api';

export interface ImageRefSegment {
  kind: 'text' | 'image';
  text: string;
  /// image 段的原路径（hover title 透出，排障用）
  path?: string;
}

/// 图片引用形态（r72b 扩 CLI 形态，三选一交替、先到先得）：
/// 1. `[Image: source: <绝对路径>.<ext>]`：jsonl 里 CLI 记录的 TUI 粘贴图形态
///    （整串消费只捕路径，不留 "[Image: source: ]" 残壳）；
/// 2. uploads 裸路径：网页上传链路（@ 可选，r72 语义不变）；
/// 3. `/tmp[/private]/claude-tmp/...` 裸路径：CLI TUI 粘贴图临时落盘形态
///    （macOS 实际形态带 /private 前缀）。
/// `[Image #22]` 无路径占位不匹配（无 src 可取，保持纯文本不渲染）。
const IMAGE_REF_RE =
  /\[Image: source: (\/[^\s"\]]+\.(?:png|jpe?g|webp|gif))\]|@?(\/[^\s"]*claude-view-uploads\/[^\s"]+\.(?:png|jpe?g|webp|gif))\b|(\/(?:private\/)?tmp\/claude-tmp\/[^\s"\]]+\.(?:png|jpe?g|webp|gif))\b/gi;

/// 把消息文本按上传图片引用切段：image 段带 [Image #n]（n 为该消息内出现顺序），
/// 其余为原样 text 段。无匹配时返回单段原文。
export function splitImageRefs(text: string): ImageRefSegment[] {
  const segs: ImageRefSegment[] = [];
  let last = 0;
  let n = 0;
  for (const m of text.matchAll(IMAGE_REF_RE)) {
    const idx = m.index;
    if (idx > last) segs.push({ kind: 'text', text: text.slice(last, idx) });
    n += 1;
    segs.push({ kind: 'image', text: `[Image #${n}]`, path: m[1] ?? m[2] ?? m[3] });
    last = idx + m[0].length;
  }
  if (last < text.length) segs.push({ kind: 'text', text: text.slice(last) });
  return segs.length > 0 ? segs : [{ kind: 'text', text }];
}

/// 图片引用路径 → 缩略图端点相对 URL（token 附 query）。
/// uploads 图走 /api/images/<落盘名>（r72 不变）；claude-tmp 图按绝对路径走
/// /api/images/by-path（后端目录前缀白名单把关）。提取失败（白名单外路径）返回
/// null，渲染层回落占位 pill。
export function imageUrlOf(path: string): string | null {
  const m = /claude-view-uploads\/([^/\\\s]+\.(?:png|jpe?g|webp|gif))/i.exec(path);
  if (m) return `/api/images/${encodeURIComponent(m[1])}${resourceTokenQuery()}`;
  if (/^\/(?:private\/)?tmp\/claude-tmp\/.+\.(?:png|jpe?g|webp|gif)$/i.test(path)) {
    const params = new URLSearchParams({ path });
    const token = resourceTokenQuery();
    if (token) params.set('token', token.slice('?token='.length));
    return `/api/images/by-path?${params.toString()}`;
  }
  return null;
}

export function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = (): void => {
      resolve((reader.result as string).split(',')[1] ?? '');
    };
    reader.onerror = (): void => reject(new Error('read file failed'));
    reader.readAsDataURL(file);
  });
}

/// 压图：绘制到 canvas 后导出 jpeg（长边 ≤2048，质量 0.85）。
/// 解码失败（HEIC 等浏览器不支持格式）返回 null。
export function compressToJpeg(
  file: File,
): Promise<{ base64: string; name: string } | null> {
  return new Promise((resolve) => {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = (): void => {
      URL.revokeObjectURL(url);
      const max = 2048;
      const scale = Math.min(1, max / Math.max(img.width, img.height));
      const canvas = document.createElement('canvas');
      canvas.width = Math.round(img.width * scale);
      canvas.height = Math.round(img.height * scale);
      const ctx = canvas.getContext('2d');
      if (!ctx) {
        resolve(null);
        return;
      }
      ctx.drawImage(img, 0, 0, canvas.width, canvas.height);
      const dataUrl = canvas.toDataURL('image/jpeg', 0.85);
      resolve({ base64: dataUrl.split(',')[1] ?? '', name: file.name.replace(/\.[^.]*$/, '') + '.jpg' });
    };
    img.onerror = (): void => {
      URL.revokeObjectURL(url);
      resolve(null);
    };
    img.src = url;
  });
}

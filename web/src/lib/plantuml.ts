/// PlantUML 渲染服务接入（2026-10-06 用户需求：可配置渲染服务、插件式）。
/// - 服务地址存 localStorage 'hub_plantuml_base'（渲染是前端行为，按浏览器配置）；
///   空串 = 未配置（安全缺省：内网文档图不默认发往公共服务）。
/// - 编码走 PlantUML 原生 ~h hex（TextEncoder UTF-8 → hex），零依赖免 deflate。
/// - SVG 输出 <img> 呈现（不 inline SVG，防脚本注入，与图片对照同口径）。

const STORAGE_KEY = 'hub_plantuml_base';

export function plantumlBase(): string {
  try {
    return (window.localStorage.getItem(STORAGE_KEY) ?? '').trim().replace(/\/+$/, '');
  } catch {
    return '';
  }
}

export function setPlantumlBase(url: string): void {
  try {
    window.localStorage.setItem(STORAGE_KEY, url.trim());
  } catch {
    // 存储不可用时仅内存语义（与 hub_theme 同口径）——不 throw
  }
}

/** UTF-8 → hex（PlantUML ~h 格式） */
export function plantumlHex(text: string): string {
  return Array.from(new TextEncoder().encode(text))
    .map((b) => b.toString(16).padStart(2, '0'))
    .join('');
}

/** 渲染 URL；未配置服务返回 null（调用方回退源码展示） */
export function plantumlSvgUrl(text: string): string | null {
  const base = plantumlBase();
  if (!base) return null;
  return `${base}/svg/~h${plantumlHex(text)}`;
}

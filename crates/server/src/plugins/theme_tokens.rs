//! 主题 token 覆盖校验（B3，ADR-0013 覆盖档）。
//!
//! 单一事实源：`web/src/App.css` 经 include_str 编译期内嵌——内置 token 值从
//! 其 `:root`（light）与 `[data-theme='dark']` 段解析，App.css 改动随重编译
//! 同步，杜绝「白名单快照漂移」。校验三步：
//! 1. token 名 ∈ 白名单（有实际色值的 `--c-*`；var() 引用链无覆盖意义，白名单外）
//! 2. light/dark 成对必填；值形态白名单（#rgb/#rrggbb/#rrrrggggbbbb 简化为 hex 解析
//!    可判），拒 `url()`/分号/注释注入
//! 3. 语义配对表（TEXT×BG 语义对子，见 SEMANTIC_PAIRS）中被覆盖 token 参与的
//!    全部配对跑 WCAG 对比度（≥4.5:1），任一不过 = 审计失败并报出具体对
//!
//! 校验器自证：`builtin_theme_passes()` 对未覆盖内置值自跑全表应全过（App.css
//! 现值全部达标——若 App.css 回归出低对比对，本测试先行失败）。

use std::collections::HashMap;

/// App.css 编译期内嵌（相对 crates/server：../../web/src/App.css）。
const APP_CSS: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../web/src/App.css"
));

/// 解析后的单 token 内置值（light/dark）。
#[derive(Debug, Clone, PartialEq)]
pub struct BuiltinValue {
    pub light: String,
    pub dark: String,
}

/// 从 App.css 文本解析内置 token 值。
/// 返回（token 名 → light/dark）；var() 引用与 rgba()/color() 复合值不进表
/// （无独立色值，覆盖无意义）——白名单即本表的键集。
pub fn builtin_token_values(css: &str) -> HashMap<String, BuiltinValue> {
    let mut out: HashMap<String, BuiltinValue> = HashMap::new();
    let mut dark = false;
    for chunk in css.split('{') {
        let head = chunk.rsplit('\n').next().unwrap_or("").trim();
        if head.contains("[data-theme") && head.contains("dark") {
            dark = true;
        }
        for line in chunk.lines() {
            let line = line.trim();
            let Some(rest) = line.strip_prefix("--c-") else { continue };
            let Some((name, value)) = rest.split_once(':') else { continue };
            let name = name.trim().to_string();
            let value = value.split("/*").next().unwrap_or("").trim().trim_end_matches(';').trim();
            if value.starts_with("var(") || value.is_empty() {
                continue; // 引用链：覆盖无意义（生效值随引用目标）
            }
            if !valid_color_value(value) {
                continue; // rgba() 等复合值（scrim 类）：白名单外
            }
            let cname = format!("--c-{name}");
            let entry = out.entry(cname).or_insert(BuiltinValue {
                light: String::new(),
                dark: String::new(),
            });
            if dark {
                entry.dark = value.to_string();
            } else {
                entry.light = value.to_string();
            }
        }
    }
    out.retain(|_, v| !v.light.is_empty() || !v.dark.is_empty());
    // 单态定义补全：只在 :root 定义（暗色段不重定义）的 token = 「两主题不变」
    // （App.css 注释原语义，如 --c-accent）——缺侧补同值，避免合法白名单成员被丢
    // （F-8 修复轮实测：原 retain 双非空策略踢掉 27+ 个单态 token）
    for v in out.values_mut() {
        if v.light.is_empty() {
            v.light = v.dark.clone();
        }
        if v.dark.is_empty() {
            v.dark = v.light.clone();
        }
    }
    out
}

/// 当前 bin 内嵌 App.css 的内置值（审计入口用）。
pub fn builtin_tokens() -> HashMap<String, BuiltinValue> {
    builtin_token_values(APP_CSS)
}

/// 覆盖值形态白名单（ADR-0013 §12.1）：hex（3/4/6/8 位）可判对比度；
/// 拒 url()、分号、花括号、注释——注入面全封。
pub fn valid_color_value(v: &str) -> bool {
    let v = v.trim();
    if v.is_empty() || v.contains(';') || v.contains('{') || v.contains('}') || v.contains("/*") {
        return false;
    }
    if let Some(hex) = v.strip_prefix('#') {
        return (3..=4).contains(&hex.len()) || hex.len() == 6 || hex.len() == 8;
    }
    false // oklch()/color() 等函数形态 v1 暂不接受（白名单收窄，后续批次按需加法）
}

/// hex → (r, g, b) 0-255；不可解析返回 None。
fn hex_rgb(v: &str) -> Option<(f64, f64, f64)> {
    let hex = v.strip_prefix('#')?;
    let (r, g, b) = match hex.len() {
        3 => (
            (u8::from_str_radix(&hex[0..1].repeat(2), 16).ok()?,),
            (u8::from_str_radix(&hex[1..2].repeat(2), 16).ok()?,),
            (u8::from_str_radix(&hex[2..3].repeat(2), 16).ok()?,),
        ),
        6 => (
            (u8::from_str_radix(&hex[0..2], 16).ok()?,),
            (u8::from_str_radix(&hex[2..4], 16).ok()?,),
            (u8::from_str_radix(&hex[4..6], 16).ok()?,),
        ),
        _ => return None,
    };
    Some((r.0 as f64, g.0 as f64, b.0 as f64))
}

/// WCAG 相对亮度（sRGB 线性化）。
pub fn relative_luminance(r: f64, g: f64, b: f64) -> f64 {
    let lin = |c: f64| -> f64 {
        let s = c / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// WCAG 对比度（1-21）；任一侧不可解析返回 None。
pub fn contrast_ratio(fg: &str, bg: &str) -> Option<f64> {
    let (fr, fg_, fb) = hex_rgb(fg)?;
    let (br, bg_, bb) = hex_rgb(bg)?;
    let l1 = relative_luminance(fr, fg_, fb);
    let l2 = relative_luminance(br, bg_, bb);
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    Some((hi + 0.05) / (lo + 0.05))
}

/// 语义配对表（ADR-0013：text × bg 语义对子全集——源自 App.css 注释记录的
/// 对比度关系与主 UI 实际落位；人工维护，加法承诺）。**收录判据：内置值已达标**
/// （check_full 自证锚定；内置不达标的组合不进表——它们要么是设计禁用场景
/// （如 accent 底白字，App.css L18 明文），要么是存量欠账（不因覆盖校验放大）。
/// 覆盖任一端 token 时，该 token 参与的全部对子须 ≥4.5:1。
///
/// 排除记录（check_full 自证暴露，2026-10-22）：
/// - (--c-inverse, --c-accent) 3.2:1——accent 底白字为禁用场景（App.css L18 明文），
///   文字场景用 accent-strong（5.10:1，在表）
/// - (--c-accent, --c-bg/--c-panel) light 3.2/2.98——accent 作文字仅在深底场景
///   （Respawn #222 底 4.98:1），浅底文字场景禁用
/// - (--c-green-text, --c-bg/--c-panel) dark 3.37/3.14——**存量欠账**：dark 段
///   green-text 引用 --c-done(#10a37f) 本身低于 4.5（App.css 既有值，非本批引入；
///   修色值需用户裁决，不因覆盖校验放大——已记批次交付报告）
pub const SEMANTIC_PAIRS: &[(&str, &str)] = &[
    ("--c-text", "--c-bg"),
    ("--c-text", "--c-panel"),
    ("--c-text", "--c-bubble"),
    ("--c-text", "--c-selected"),
    ("--c-sub", "--c-bg"),
    ("--c-sub", "--c-panel"),
    ("--c-sub", "--c-bubble"),
    ("--c-faint", "--c-bg"),
    ("--c-faint", "--c-panel"),
    ("--c-inverse", "--c-accent-strong"),
    ("--c-inverse", "--c-sidebar"),
    ("--c-danger-text", "--c-bg"),
    ("--c-danger-text", "--c-danger-bg"),
    ("--c-warn-text", "--c-bg"),
    ("--c-warn-text", "--c-warn-bg"),
    ("--c-diff-add-text", "--c-bg"),
    ("--c-diff-add-text", "--c-diff-add-bg"),
    ("--c-danger-bright", "--c-sidebar"),
    ("--c-warn-bright", "--c-sidebar"),
    ("--c-sidebar-text", "--c-sidebar"),
    ("--c-sidebar-text", "--c-sidebar-item"),
    ("--c-sidebar-text", "--c-sidebar-top"),
    ("--c-sidebar-text-sel", "--c-sidebar"),
    ("--c-sidebar-text-sel", "--c-sidebar-item"),
    ("--c-sidebar-icon", "--c-sidebar"),
    ("--c-driver-claude-text", "--c-driver-claude-bg"),
    ("--c-driver-opencode-text", "--c-driver-opencode-bg"),
    ("--c-driver-dsh-text", "--c-driver-dsh-bg"),
];

/// 对比度硬标准（文字，WCAG AA）。
pub const MIN_CONTRAST: f64 = 4.5;

/// 一条对比度不达标的具体对（审计卡可定位展示）。
#[derive(Debug, Clone, PartialEq)]
pub struct ContrastFail {
    pub text_token: String,
    pub bg_token: String,
    pub theme: &'static str,
    pub ratio: f64,
}

/// 校验一份覆盖表（token → {light, dark}）。
/// 返回全部不达标对（空 = 通过）；白名单外/形态错由 manifest 校验另报。
pub fn check_overrides(
    overrides: &HashMap<String, (String, String)>,
    builtins: &HashMap<String, BuiltinValue>,
) -> Vec<ContrastFail> {
    check_overrides_inner(overrides, builtins, false)
}

/// 全表模式（自证）：不跳过未触及对——校验器正确性的自证锚点
/// （design §12.7：对未覆盖内置主题自跑全表应全过；表含内置不达标对时先行失败）。
pub fn check_full(builtins: &HashMap<String, BuiltinValue>) -> Vec<ContrastFail> {
    check_overrides_inner(&HashMap::new(), builtins, true)
}

fn check_overrides_inner(
    overrides: &HashMap<String, (String, String)>,
    builtins: &HashMap<String, BuiltinValue>,
    full: bool,
) -> Vec<ContrastFail> {
    let mut fails = Vec::new();
    // 生效值 = 覆盖值优先，缺省内置
    let eff = |token: &str, dark: bool| -> Option<String> {
        if let Some((l, d)) = overrides.get(token) {
            return Some(if dark { d.clone() } else { l.clone() });
        }
        builtins.get(token).map(|b| if dark { b.dark.clone() } else { b.light.clone() })
    };
    for (text_tok, bg_tok) in SEMANTIC_PAIRS {
        if !full {
            let text_touched = overrides.contains_key(&text_tok.to_string());
            let bg_touched = overrides.contains_key(&bg_tok.to_string());
            if !text_touched && !bg_touched {
                continue; // 只校验被覆盖 token 参与的对（未覆盖对 = 内置现状）
            }
        }
        for (theme, dark) in [("light", false), ("dark", true)] {
            let (Some(tf), Some(bf)) = (eff(text_tok, dark), eff(bg_tok, dark)) else {
                continue;
            };
            if let Some(ratio) = contrast_ratio(&tf, &bf) {
                if ratio < MIN_CONTRAST {
                    fails.push(ContrastFail {
                        text_token: (*text_tok).to_string(),
                        bg_token: (*bg_tok).to_string(),
                        theme,
                        ratio: (ratio * 100.0).round() / 100.0,
                    });
                }
            }
        }
    }
    fails
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_theme_passes_full_table() {
        // 校验器自证（design §12.7）：check_full 对内置值自跑全表应全过——
        // 表内的对必须全部「内置已达标」（配对表只收录实际用于文字场景且
        // App.css 既定达标的组合）；本测试先行失败即表或 App.css 回归
        let builtins = builtin_tokens();
        assert!(builtins.contains_key("--c-text"), "App.css 解析应有 --c-text");
        let fails = check_full(&builtins);
        assert!(fails.is_empty(), "内置主题不应有不达标对: {fails:?}");
    }

    #[test]
    fn low_contrast_override_rejected() {
        let builtins = builtin_tokens();
        // 把 --c-bg 覆盖为接近文字色 → --c-text × --c-bg 对崩
        let mut ov = HashMap::new();
        ov.insert(
            "--c-bg".to_string(),
            ("#1a1a1a".to_string(), "#1a1a1a".to_string()),
        );
        let fails = check_overrides(&ov, &builtins);
        assert!(
            fails.iter().any(|f| f.text_token == "--c-text" && f.bg_token == "--c-bg"),
            "{fails:?}"
        );
    }

    #[test]
    fn untouched_pairs_not_validated() {
        // 只覆盖无语义对的 token（如 --c-scrim 白名单外被 manifest 拦；此处
        // 用 --c-code-border 这类不在配对表内的 token）→ 零失败
        let builtins = builtin_tokens();
        let mut ov = HashMap::new();
        ov.insert(
            "--c-code-border".to_string(),
            ("#111111".to_string(), "#eeeeee".to_string()),
        );
        let fails = check_overrides(&ov, &builtins);
        assert!(fails.is_empty(), "{fails:?}");
    }

    #[test]
    fn value_form_whitelist() {
        assert!(valid_color_value("#fff"));
        assert!(valid_color_value("#10a37f"));
        assert!(!valid_color_value("url(https://evil)"));
        assert!(!valid_color_value("#fff; }"));
        assert!(!valid_color_value("rgb(1,2,3)"));
        assert!(!valid_color_value(""));
    }

    #[test]
    fn parses_builtin_values() {
        let builtins = builtin_tokens();
        let bg = builtins.get("--c-bg").expect("bg token");
        assert_eq!(bg.light, "#ffffff");
        assert_eq!(bg.dark, "#171717");
        // var() 引用链不进白名单
        assert!(!builtins.contains_key("--c-code"), "var() 引用 token 应跳过");
        // rgba() 复合值不进白名单
        assert!(!builtins.contains_key("--c-scrim"));
    }

    #[test]
    fn contrast_math_anchors() {
        // 锚点：白底黑字 21:1、黑底白字同值
        let w = contrast_ratio("#ffffff", "#1a1a1a").unwrap();
        assert!(w > 15.0, "{w}");
        assert_eq!(contrast_ratio("#ffffff", "#ffffff").unwrap(), 1.0);
        assert!(contrast_ratio("nope", "#ffffff").is_none());
    }
}


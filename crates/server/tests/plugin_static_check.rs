//! 插件侧静态检查（需求红线：插件代码**零直调 hub `/api/*`**——静态检查 + 评审）。
//!
//! 扫描源码仓内随附的示例插件（`plugins/`，即插件侧代码的唯一在仓形态）：
//! 剥离注释后，任何对 `/api/` 的引用都判违规（`https://<外部域名>/api/...` 不受影响，
//! 该形态是插件自行出站的合法路径）。此测试在 `cargo test -p server` 门禁内常跑，
//! 防示例插件把「直调 /api」的坏示范带进生态。

use std::path::{Path, PathBuf};

fn repo_plugins_dir() -> PathBuf {
    // 示例插件随源码仓交付：crates/server 的上两级 = 仓库根
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins")
}

/// 剥离 JS 注释（行/块；字符串字面量保护——demo 里的 URL 字符串不应被注释剥离破坏）。
fn strip_js_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut in_line_comment = false;
    let mut in_block_comment = false;
    let mut string_delim: Option<char> = None;
    while let Some(c) = chars.next() {
        if let Some(d) = string_delim {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == d {
                string_delim = None;
            }
            continue;
        }
        if in_line_comment {
            if c == '\n' {
                in_line_comment = false;
                out.push(c);
            }
            continue;
        }
        if in_block_comment {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                in_block_comment = false;
                out.push(' ');
            }
            continue;
        }
        match c {
            '"' | '\'' | '`' => {
                string_delim = Some(c);
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                chars.next();
                in_line_comment = true;
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                in_block_comment = true;
            }
            _ => out.push(c),
        }
    }
    out
}

fn strip_html_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(start) = rest.find("<!--") {
        out.push_str(&rest[..start]);
        match rest[start..].find("-->") {
            Some(end) => {
                out.push(' ');
                rest = &rest[start + end + 3..];
            }
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

fn collect_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(read) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in read.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sources(&path, out);
        } else if matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("js" | "mjs" | "html" | "css")
        ) {
            out.push(path);
        }
    }
}

/// 直接引用判据：`/api/` 前必须是引号、反引号、空白或 `(`（相对路径调用形态）；
/// `https://gitlab.com/api/v4` 这类外部出站 URL 的前字符是域名，不算直调 hub。
fn references_hub_api(code: &str) -> Vec<usize> {
    let bytes = code.as_bytes();
    let mut hits = Vec::new();
    let mut from = 0;
    while let Some(rel) = code[from..].find("/api/") {
        let at = from + rel;
        let prev_ok = at == 0
            || matches!(bytes[at - 1], b'"' | b'\'' | b'`' | b' ' | b'\t' | b'\n' | b'(' | b'=');
        if prev_ok {
            hits.push(at);
        }
        from = at + 1;
    }
    hits
}

#[test]
#[cfg(unix)]
fn bundled_example_plugins_never_call_hub_api_directly() {
    let dir = repo_plugins_dir();
    if !dir.is_dir() {
        panic!("示例插件目录不存在：{}", dir.display());
    }
    let mut sources = Vec::new();
    collect_sources(&dir, &mut sources);
    assert!(
        sources.len() >= 3,
        "示例插件应包含至少 3 个源文件（backend/view），实际 {}",
        sources.len()
    );
    let mut violations = Vec::new();
    for path in sources {
        let raw = std::fs::read_to_string(&path).unwrap();
        let code = match path.extension().and_then(|e| e.to_str()) {
            Some("html") => strip_js_comments(&strip_html_comments(&raw)),
            Some("css") => raw.clone(),
            _ => strip_js_comments(&raw),
        };
        for at in references_hub_api(&code) {
            let ctx: String = code[at.saturating_sub(40)..(at + 40).min(code.len())]
                .chars()
                .collect();
            violations.push(format!("{}: …{ctx}…", path.display()));
        }
    }
    assert!(
        violations.is_empty(),
        "插件侧代码直调 hub /api/*（零容忍红线）：\n{}",
        violations.join("\n")
    );
}

#[test]
fn hub_api_reference_detection_covers_real_shapes() {
    // 合法：相对路径直调形态必须命中
    assert_eq!(references_hub_api(r#"fetch("/api/agents")"#).len(), 1);
    assert_eq!(references_hub_api("fetch(`/api/plugins/x`)").len(), 1);
    // 合法：外部出站（自定 API base）不算 hub 直调
    assert_eq!(
        references_hub_api("fetch('https://gitlab.com/api/v4/projects')").len(),
        0
    );
    // 注释里的提及被剥离后不算
    assert_eq!(references_hub_api(&strip_js_comments("// 零直调 /api/*\nload();")).len(), 0);
}

//! WASM URL 自动发现 —— 从 chat.deepseek.com 的 HTML / JS bundle 扫出
//! 最新的 `sha3_wasm_bg.<hash>.wasm` 文件名。
//!
//! DeepSeek 每次重新构建前端都会把 wasm 的 content-hash 塞进文件名；
//! webpack/vite 把该字面量保留在 JS bundle 里（用于 `fetch(new URL(...))`），
//! 所以按字节模式 `sha3_wasm_bg.` + hex + `.wasm` 就能捞出来，再拼回
//! `fe-static.deepseek.com/chat/static/` 前缀即可。
//!
//! 任何一步失败（网络 / 403 / 扫描不到）都返回 `Err`，由调用方决定是否
//! 回退到配置里的 URL —— 本模块不做 side effect，只负责 **纯发现**。

use super::client::{ClientError, DsClient};

#[derive(Debug, thiserror::Error)]
pub enum DiscoveryError {
    /// 抓取 HTML / JS 失败
    #[error("fetch failed: {0}")]
    Fetch(#[from] ClientError),
    /// 扫完所有候选 bundle 仍未找到 WASM 引用
    #[error("no wasm reference found in index + {scanned} JS bundle(s)")]
    NotFound { scanned: usize },
}

/// SPA 入口
const CHAT_INDEX_URL: &str = "https://chat.deepseek.com/";
/// wasm 文件名固定前缀
const WASM_PREFIX: &str = "sha3_wasm_bg.";
/// wasm 文件名固定后缀
const WASM_SUFFIX: &str = ".wasm";
/// wasm 静态资源根（与配置里的默认值保持一致）
const WASM_STATIC_BASE: &str = "https://fe-static.deepseek.com/chat/static/";
/// index.html 上扫描的 JS bundle 上限（保护，首页一般也就十几个）
const MAX_JS_SCAN: usize = 20;

/// 发现当前 web client 使用的 WASM URL。
///
/// 返回形如 `https://fe-static.deepseek.com/chat/static/sha3_wasm_bg.7b9ca65ddd.wasm`
pub async fn discover_wasm_url(client: &DsClient) -> Result<String, DiscoveryError> {
    let html = client.fetch_text(CHAT_INDEX_URL).await?;

    // SPA index 常内联 chunk map，优先直接扫 HTML
    if let Some(filename) = scan_wasm_filename(&html) {
        return Ok(format!("{WASM_STATIC_BASE}{filename}"));
    }

    // BFS：从 HTML 里的 <script src>/<link href> 起步，每个 JS body 再挖
    // 字符串字面量里的 `.js` 路径，直到找到 wasm 引用或打满配额。
    let mut queue: Vec<String> = extract_js_urls(&html);
    let mut seen: std::collections::HashSet<String> = queue.iter().cloned().collect();
    let mut scanned = 0usize;

    while let Some(js_url) = queue.pop() {
        if scanned >= MAX_JS_SCAN {
            break;
        }
        scanned += 1;
        let Ok(body) = client.fetch_text(&js_url).await else {
            continue;
        };
        if let Some(filename) = scan_wasm_filename(&body) {
            return Ok(format!("{WASM_STATIC_BASE}{filename}"));
        }
        for url in extract_js_urls_in_body(&body) {
            if seen.insert(url.clone()) {
                queue.push(url);
            }
        }
    }

    Err(DiscoveryError::NotFound { scanned })
}

/// 在任意文本里找首个合法的 `sha3_wasm_bg.<hex>.wasm` 文件名。
fn scan_wasm_filename(text: &str) -> Option<String> {
    let start = text.find(WASM_PREFIX)?;
    let after_prefix = &text[start + WASM_PREFIX.len()..];
    let hex_len = after_prefix
        .bytes()
        .take_while(u8::is_ascii_hexdigit)
        .count();
    if hex_len == 0 {
        return None;
    }
    let rest = &after_prefix[hex_len..];
    if !rest.starts_with(WASM_SUFFIX) {
        return None;
    }
    let hash = &after_prefix[..hex_len];
    Some(format!("{WASM_PREFIX}{hash}{WASM_SUFFIX}"))
}

/// 从 HTML 里抽 `<script src="…">` / `<link … href="…">` 引用的 `.js` 资源。
///
/// 纯字节扫描，避免引入 HTML parser 依赖。
fn extract_js_urls(html: &str) -> Vec<String> {
    let mut out = Vec::new();
    for pair in [("<script", "src=\""), ("<link", "href=\"")] {
        collect_tag_attr(html, pair.0, pair.1, &mut out);
    }
    out
}

fn collect_tag_attr(html: &str, tag: &str, attr: &str, out: &mut Vec<String>) {
    let mut cursor = 0usize;
    while let Some(rel) = html[cursor..].find(tag) {
        let tag_start = cursor + rel;
        let Some(tag_end_rel) = html[tag_start..].find('>') else {
            break;
        };
        let tag_end = tag_start + tag_end_rel;
        if let Some(attr_rel) = html[tag_start..tag_end].find(attr) {
            let value_start = tag_start + attr_rel + attr.len();
            if let Some(end_rel) = html[value_start..tag_end].find('"') {
                let url = &html[value_start..value_start + end_rel];
                if looks_like_js(url) {
                    out.push(normalize_url(url));
                }
            }
        }
        cursor = tag_end + 1;
    }
}

/// 从 JS body 抽 **引号内** 以 `.js` 结尾的路径（webpack/vite chunk map 常用形式）。
///
/// 只保留长度合理、看起来像文件路径的候选；绝对路径 / 协议相对 / 相对路径
/// 统一用 `normalize_url` 拼回 `chat.deepseek.com` 根。
fn extract_js_urls_in_body(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for delim in ['"', '\''] {
        let mut cursor = 0usize;
        while let Some(idx) = body[cursor..].find(delim) {
            let start = cursor + idx + 1;
            let Some(end_rel) = body[start..].find(delim) else {
                break;
            };
            let end = start + end_rel;
            let candidate = &body[start..end];
            // 过滤明显不是路径的字面量
            if (5..=512).contains(&candidate.len())
                && !candidate.contains(['\n', ' ', '<', '>'])
                && looks_like_js(candidate)
            {
                out.push(normalize_url(candidate));
            }
            cursor = end + 1;
        }
    }
    out
}

fn looks_like_js(url: &str) -> bool {
    // ".js" 本体 或 ".js?query"；排除 ".json"
    if let Some(idx) = url.rfind(".js") {
        let tail = &url[idx + 3..];
        return tail.is_empty() || tail.starts_with('?') || tail.starts_with('#');
    }
    false
}

fn normalize_url(raw: &str) -> String {
    if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_string()
    } else if raw.starts_with("//") {
        format!("https:{raw}")
    } else if raw.starts_with('/') {
        format!("https://chat.deepseek.com{raw}")
    } else {
        format!("https://chat.deepseek.com/{raw}")
    }
}

#[cfg(test)]
mod tests {
    use super::{
        extract_js_urls, extract_js_urls_in_body, looks_like_js, normalize_url, scan_wasm_filename,
    };

    #[test]
    fn scans_literal_filename() {
        let s = r#"...fetch("sha3_wasm_bg.abc123def.wasm")..."#;
        assert_eq!(
            scan_wasm_filename(s).as_deref(),
            Some("sha3_wasm_bg.abc123def.wasm")
        );
    }

    #[test]
    fn scans_current_production_hash() {
        // 对齐 config.example.toml 里的样例
        let s = "https://fe-static.deepseek.com/chat/static/sha3_wasm_bg.7b9ca65ddd.wasm";
        assert_eq!(
            scan_wasm_filename(s).as_deref(),
            Some("sha3_wasm_bg.7b9ca65ddd.wasm")
        );
    }

    #[test]
    fn rejects_non_hex_or_bad_suffix() {
        assert!(scan_wasm_filename("sha3_wasm_bg.ghijk.wasm").is_none());
        assert!(scan_wasm_filename("sha3_wasm_bg.abc.js").is_none());
        assert!(scan_wasm_filename("sha3_wasm_bg..wasm").is_none());
        assert!(scan_wasm_filename("nothing here").is_none());
    }

    #[test]
    fn looks_like_js_handles_query_and_hash() {
        assert!(looks_like_js("/x.js"));
        assert!(looks_like_js("/x.js?v=1"));
        assert!(looks_like_js("/x.js#foo"));
        assert!(!looks_like_js("/x.json"));
        assert!(!looks_like_js("/x.jsx"));
        assert!(!looks_like_js("/x.css"));
    }

    #[test]
    fn normalize_handles_protocol_relative_and_absolute() {
        assert_eq!(
            normalize_url("https://cdn.example/x.js"),
            "https://cdn.example/x.js"
        );
        assert_eq!(
            normalize_url("//cdn.example/x.js"),
            "https://cdn.example/x.js"
        );
        assert_eq!(
            normalize_url("/assets/app.js"),
            "https://chat.deepseek.com/assets/app.js"
        );
        assert_eq!(
            normalize_url("assets/app.js"),
            "https://chat.deepseek.com/assets/app.js"
        );
    }

    #[test]
    fn extracts_both_script_and_link_js() {
        let html = r#"<html>
<script src="/a-1.js"></script>
<link rel="modulepreload" href="https://cdn.example/b.js">
<link rel="stylesheet" href="/c.css">
<script type="module" src="/d.js?v=2"></script>
</html>"#;
        let urls = extract_js_urls(html);
        assert_eq!(
            urls,
            vec![
                "https://chat.deepseek.com/a-1.js".to_string(),
                "https://chat.deepseek.com/d.js?v=2".to_string(),
                "https://cdn.example/b.js".to_string(),
            ]
        );
    }

    #[test]
    fn extracts_skips_script_without_src() {
        let html = "<script>console.log('inline')</script><script src=\"/ok.js\"></script>";
        let urls = extract_js_urls(html);
        assert_eq!(urls, vec!["https://chat.deepseek.com/ok.js".to_string()]);
    }

    #[test]
    fn extracts_chunk_refs_from_js_body() {
        // 典型 Vite/webpack chunk map 片段
        let js = r#"const map={0:"assets/wasm-abc.js",1:'/assets/pow-def.js',2:"nope.css"};
import("https://cdn.example/chunk-x.js");"#;
        let urls = extract_js_urls_in_body(js);
        assert!(urls.contains(&"https://chat.deepseek.com/assets/wasm-abc.js".to_string()));
        assert!(urls.contains(&"https://chat.deepseek.com/assets/pow-def.js".to_string()));
        assert!(urls.contains(&"https://cdn.example/chunk-x.js".to_string()));
        assert!(!urls.iter().any(|u| u.ends_with(".css")));
    }

    #[test]
    fn body_extractor_rejects_spaces_and_oversized() {
        let js = r#"let x="path with space.js"; let y="/short.js";"#;
        let urls = extract_js_urls_in_body(js);
        assert_eq!(urls, vec!["https://chat.deepseek.com/short.js".to_string()]);
    }
}

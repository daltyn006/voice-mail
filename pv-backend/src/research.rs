//! Opt-in web research for video jobs (Settings → Research, default OFF).
//!
//! Contract: the film is always watched first — research only supplements.
//! Fetched text lands in `<data>/research/<stem>/notes.md` as
//! `source — claim` lines; the core REDUCE renders those under a fenced
//! `## External context` section, never mixed with film-grounded facts.
//! Dynamic budget: attention <34 → ≤3 sources, <67 → ≤5, else ≤10.
//!
//! Same-origin sense for a desktop app: https/http only, 15 s per request,
//! 512 KiB per page, no credentials/cookies, redirects capped by reqwest
//! defaults. Failures degrade to "research unavailable" — never an error
//! that blocks the transcript/summary the user already has.

use std::path::PathBuf;

const PER_PAGE_TIMEOUT_SECS: u64 = 15;
const MAX_PAGE_BYTES: usize = 512 * 1024;

/// Source budget for an attention value (0–100). Pure.
pub fn budget_for(attention: i32) -> usize {
    if attention < 34 {
        3
    } else if attention < 67 {
        5
    } else {
        10
    }
}

/// True for fetchable web URLs: http(s) with a host. Rejects file:, data:,
/// javascript:, UNC paths, and bare filenames. Pure.
pub fn url_allowed(url: &str) -> bool {
    let url = url.trim();
    let rest = if let Some(r) = url.strip_prefix("https://") {
        r
    } else if let Some(r) = url.strip_prefix("http://") {
        r
    } else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if host.is_empty() || host.contains('\\') || host.contains(' ') {
        return false;
    }
    // No credentials in URLs, no loopback trickery via userinfo.
    !rest.contains('@')
}

/// Cache dir for one job stem (sanitized like drafts stems).
pub fn notes_dir_for(stem: &str) -> PathBuf {
    let mut s: String = stem
        .chars()
        .map(|c| {
            if matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*')
                || (c as u32) < 0x20
            {
                '-'
            } else {
                c
            }
        })
        .collect();
    s = s.trim().trim_matches('.').to_string();
    if s.is_empty() {
        s = "research".to_string();
    }
    if s.len() > 60 {
        s.truncate(60);
    }
    crate::dirs::data_dir().join("research").join(s)
}

/// Strip a page down to readable text: drops script/style/nav boilerplate by
/// tag, decodes common entities, collapses whitespace. Pure, dependency-free.
pub fn extract_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len().min(64 * 1024));
    let chars: Vec<char> = html.chars().collect();
    let mut i = 0;
    let mut skip: Option<&str> = None; // inside script/style: drop contents
    while i < chars.len() {
        let c = chars[i];
        if c == '<' {
            // Comment?
            if chars[i..].starts_with(&['<', '!', '-', '-']) {
                let mut j = i + 4;
                while j + 2 < chars.len()
                    && !(chars[j] == '-' && chars[j + 1] == '-' && chars[j + 2] == '>')
                {
                    j += 1;
                }
                i = (j + 3).min(chars.len());
                continue;
            }
            // Collect the tag span.
            let mut j = i + 1;
            while j < chars.len() && chars[j] != '>' {
                j += 1;
            }
            let tag: String = chars[i + 1..j.min(chars.len())]
                .iter()
                .collect::<String>()
                .to_ascii_lowercase();
            let name = tag
                .trim_start_matches('/')
                .split([' ', '\t', '\n', '\r', '/'])
                .next()
                .unwrap_or("");
            let closing = tag.starts_with('/');
            if closing {
                // Closing tags clear a matching skip first (a `</script>`
                // must never re-arm the skip — that swallowed the page).
                if skip.is_some_and(|s| name == s) {
                    skip = None;
                } else if skip.is_none()
                    && matches!(
                        name,
                        "p" | "div" | "li" | "h1" | "h2" | "h3" | "h4" | "tr" | "article"
                            | "section"
                    )
                {
                    out.push('\n');
                }
            } else if name == "script" || name == "style" {
                skip = Some(if name == "script" { "script" } else { "style" });
            } else if skip.is_none()
                && matches!(
                    name,
                    "p" | "br" | "div" | "li" | "h1" | "h2" | "h3" | "h4" | "tr" | "article"
                        | "section"
                )
            {
                out.push('\n');
            }
            i = (j + 1).min(chars.len());
            continue;
        }
        if skip.is_some() {
            i += 1;
            continue;
        }
        if c == '&' {
            let mut j = i + 1;
            while j < chars.len() && chars[j] != ';' && j - i <= 12 {
                j += 1;
            }
            if j < chars.len() && chars[j] == ';' {
                let ent: String = chars[i..=j].iter().collect();
                let decoded = match ent.as_str() {
                    "&amp;" => Some("&"),
                    "&lt;" => Some("<"),
                    "&gt;" => Some(">"),
                    "&quot;" => Some("\""),
                    "&apos;" | "&#39;" => Some("'"),
                    "&nbsp;" => Some(" "),
                    _ => None,
                };
                if let Some(d) = decoded {
                    out.push_str(d);
                    i = j + 1;
                    continue;
                }
            }
        }
        out.push(c);
        i += 1;
    }
    // Collapse whitespace runs, keep single newlines.
    let mut clean = String::with_capacity(out.len());
    let mut blank = 0;
    for line in out.lines() {
        let t = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if t.is_empty() {
            blank += 1;
            if blank <= 1 {
                clean.push('\n');
            }
            continue;
        }
        blank = 0;
        clean.push_str(&t);
        clean.push('\n');
    }
    clean.trim().to_string()
}

/// Query keywords from a title/topic string: lowercase alnum words, deduped,
/// stop-word filtered, capped. Pure — feeds the search endpoint.
pub fn query_terms(topic: &str, cap: usize) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "a", "an", "and", "or", "of", "to", "in", "on", "for", "with", "day", "lec",
        "lecture", "class", "part", "episode", "film", "video", "watch",
    ];
    let mut out = Vec::new();
    for w in topic.split(|c: char| !c.is_alphanumeric()) {
        let w = w.to_lowercase();
        if w.len() < 3 || STOP.contains(&w.as_str()) || out.contains(&w) {
            continue;
        }
        out.push(w);
        if out.len() >= cap {
            break;
        }
    }
    out
}

fn search_url(query: &str) -> String {
    // DuckDuckGo HTML endpoint (no key, best-effort; failures degrade).
    let q: String = query
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect();
    format!("https://html.duckduckgo.com/html/?q={}", q.trim().replace(' ', "+"))
}

/// Pull result links from a DDG HTML page (pure). Handles both bare
/// `href="https://…"` and the wrapped `/l/?…uddg=<pct-encoded>` form.
pub fn parse_result_links(html: &str, cap: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = html;
    while let Some(i) = rest.find("href=\"") {
        rest = &rest[i + 6..];
        let Some(end) = rest.find('"') else {
            break;
        };
        let raw = &rest[..end];
        rest = &rest[end.min(rest.len())..];
        // Unwrap DDG's redirect wrapper to the real target.
        let url = if let Some(j) = raw.find("uddg=") {
            let enc = &raw[j + 5..];
            let enc = enc.split('&').next().unwrap_or(enc);
            urlencoding_decode(enc)
        } else {
            raw.to_string()
        };
        let url = url.split('&').next().unwrap_or(&url).to_string();
        if url_allowed(&url) && !out.contains(&url) {
            out.push(url);
            if out.len() >= cap {
                break;
            }
        }
    }
    out
}

fn urlencoding_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push((h * 16 + l) as char);
                i += 3;
                continue;
            }
        }
        out.push(if bytes[i] == b'+' { ' ' } else { bytes[i] as char });
        i += 1;
    }
    out
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Fetch one URL to capped text (async; caller provides the runtime).
pub async fn fetch_text(
    client: &reqwest::Client,
    url: &str,
) -> Result<String, String> {
    Ok(extract_text(&fetch_raw(client, url).await?))
}

/// One raw fetch (capped bytes, UTF-8 lossy): link parsing AND text
/// extraction share it — a second GET of the same URL is pure waste.
pub async fn fetch_raw(client: &reqwest::Client, url: &str) -> Result<String, String> {
    if !url_allowed(url) {
        return Err("URL not allowed".to_string());
    }
    let resp = client
        .get(url)
        .timeout(std::time::Duration::from_secs(PER_PAGE_TIMEOUT_SECS))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("server status: {}", resp.status()));
    }
    let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
    let n = bytes.len().min(MAX_PAGE_BYTES);
    Ok(String::from_utf8_lossy(&bytes[..n]).into_owned())
}

/// Run research for one video job: search → fetch up to budget → write
/// `notes.md` (`source — excerpt` lines). Blocking (call from a worker
/// thread, never the UI thread). Returns the note count; zero notes is a
/// normal degrade (offline/blocked), never an error that stops the film.
pub fn research_blocking(topic: &str, stem: &str, attention: i32) -> usize {
    let budget = budget_for(attention);
    let terms = query_terms(topic, 6);
    if terms.is_empty() {
        return 0;
    }
    let dir = notes_dir_for(stem);
    if std::fs::create_dir_all(&dir).is_err() {
        return 0;
    }
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(_) => return 0,
    };
    rt.block_on(async move {
        let client = match reqwest::Client::builder()
            .user_agent("present-voice/0.1 (research; opt-in)")
            .build()
        {
            Ok(c) => c,
            Err(_) => return 0,
        };
        // 1. Candidate links (search page itself counts nothing on failure).
        // Single fetch: raw HTML feeds both the link parser and the
        // bare-URL text fallback.
        let links = match fetch_raw(&client, &search_url(&terms.join(" "))).await {
            Ok(raw) => {
                let mut urls: Vec<String> = extract_text(&raw)
                    .split_whitespace()
                    .filter(|w| url_allowed(w))
                    .take(budget)
                    .map(|s| s.to_string())
                    .collect();
                // Raw HTML parse for real result links (same bytes, no refetch).
                for u in parse_result_links(&raw, budget) {
                    if !urls.contains(&u) {
                        urls.push(u);
                        if urls.len() >= budget {
                            break;
                        }
                    }
                }
                urls.truncate(budget);
                urls
            }
            Err(_) => Vec::new(),
        };
        // 2. Fetch + condense each source.
        let mut notes = String::new();
        notes.push_str(&format!("# Research: {topic}\n\n"));
        let mut kept = 0;
        for url in links.iter().take(budget) {
            if let Ok(text) = fetch_text(&client, url).await {
                let excerpt: String = text.chars().take(1200).collect();
                if excerpt.trim().len() < 80 {
                    continue;
                }
                notes.push_str(&format!("## {url}\n{excerpt}\n\n"));
                kept += 1;
            }
        }
        if kept == 0 {
            return 0;
        }
        notes.push_str(&format!(
            "\nFetched {} source(s) for attention {attention}; film-grounded sections take precedence.\n",
            kept
        ));
        if std::fs::write(dir.join("notes.md"), notes).is_err() {
            return 0;
        }
        kept
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_follow_attention() {
        assert_eq!(budget_for(0), 3);
        assert_eq!(budget_for(33), 3);
        assert_eq!(budget_for(34), 5);
        assert_eq!(budget_for(66), 5);
        assert_eq!(budget_for(67), 10);
        assert_eq!(budget_for(100), 10);
    }

    #[test]
    fn url_gate() {
        assert!(url_allowed("https://example.com/a"));
        assert!(url_allowed("http://example.com/"));
        assert!(!url_allowed("file:///etc/passwd"));
        assert!(!url_allowed("data:text/html,hi"));
        assert!(!url_allowed("javascript:alert(1)"));
        assert!(!url_allowed("https://"));
        assert!(!url_allowed("C:\\movies\\film.mp4"));
        assert!(!url_allowed("https://user:pass@example.com/"));
        assert!(!url_allowed("https://exam ple.com/"));
    }

    #[test]
    fn extractor_drops_boilerplate() {
        let html = "<html><head><style>.x{color:red}</style><script>evil()</script></head>\
            <body><h1>Title</h1><p>Hello <b>world</b> &amp; friends</p></body></html>";
        let t = extract_text(html);
        assert!(t.contains("Title"));
        assert!(t.contains("Hello world & friends"));
        assert!(!t.contains("evil"));
        assert!(!t.contains("color"));
    }

    #[test]
    fn terms_skip_stopwords() {
        let terms = query_terms("Day 4 - The Mitosis Lecture (Part 2)", 6);
        assert!(terms.contains(&"mitosis".to_string()));
        assert!(!terms.contains(&"day".to_string()));
        assert!(!terms.contains(&"part".to_string()));
    }

    #[test]
    fn ddg_links_unwrap() {
        let html = r#"<a href="/l/?kh=-1&uddg=https%3A%2F%2Fexample.com%2Ffilm">x</a> \
            <a href="https://other.org/a">y</a>"#;
        let links = parse_result_links(html, 10);
        assert!(links.iter().any(|u| u == "https://example.com/film"));
        assert!(links.iter().any(|u| u == "https://other.org/a"));
    }

    #[test]
    fn notes_dir_sanitizes() {
        let d = notes_dir_for("A<b>:c/d");
        assert!(!d.to_string_lossy().contains('<'));
        assert!(notes_dir_for("").ends_with("research"));
    }
}

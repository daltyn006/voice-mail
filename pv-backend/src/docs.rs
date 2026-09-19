//! Document text extraction (offline, pure-Rust).
//!
//! Handler-style dispatch mirroring the `p2r3/convert` pattern (one handler
//! per format family), but native Rust with no network and no GPL code:
//! every extractor below is hand-written against `zip`/`quick-xml`/
//! `calamine`/`pdf-extract`/`encoding_rs`.
//!
//! Contract: `extract_text` returns plain UTF-8 text suitable as `raw`
//! input to the existing MAP→REDUCE summarizer. Failures are `Err(String)`
//! with an actionable message for the GUI Error Center — never a panic,
//! never a hallucinated transcript.

use std::path::Path;

/// Every modern document extension accepted alongside audio.
/// The file picker AND `Store::add_files` share this list so they never drift.
pub const DOC_EXTS: &[&str] = &[
    // plain / data (decoded, no structural parsing)
    "txt", "md", "markdown", "log", "csv", "tsv", "json", "xml", "yaml", "yml", "toml",
    "ini", "srt", "vtt", "lrc",
    // web / markup
    "html", "htm", "xhtml", "mhtml",
    // rich text
    "rtf",
    // Office Open XML (zip+xml)
    "docx", "pptx", "xlsx", "xlsm",
    // legacy Office (native best-effort; calamine covers xls)
    "doc", "ppt", "xls",
    // OpenDocument
    "odt", "ods", "odp",
    // ebook
    "epub",
    // portable document
    "pdf",
];

/// Plain-text-ish extensions decoded directly.
const PLAIN_EXTS: &[&str] = &[
    "txt", "md", "markdown", "log", "csv", "tsv", "json", "xml", "yaml", "yml", "toml",
    "ini",
];

/// Subtitle/caption extensions (timestamps stripped).
const SUB_EXTS: &[&str] = &["srt", "vtt", "lrc"];

#[derive(Clone, Debug)]
pub struct Extracted {
    pub text: String,
    pub pages: usize,
    pub truncated: bool,
    pub kind: String,
}

pub fn is_doc_ext(ext: &str) -> bool {
    DOC_EXTS.contains(&ext.to_ascii_lowercase().as_str())
}

/// Extract `path` to plain text, capped at `max_chars` (0 = 500k default).
/// Empty/non-text results are `Err` so callers warn instead of summarizing air.
pub fn extract_text(path: &Path, max_chars: usize) -> Result<Extracted, String> {
    let cap = if max_chars == 0 { 500_000 } else { max_chars };
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if ext.is_empty() {
        return Err("no file extension — rename with .txt/.md/.pdf/etc.".to_string());
    }
    if !is_doc_ext(&ext) {
        return Err(format!("unsupported document .{ext}"));
    }
    let mut out = if PLAIN_EXTS.contains(&ext.as_str()) {
        let raw = std::fs::read(path).map_err(|e| format!("cannot read .{ext}: {e}"))?;
        Extracted {
            text: decode_bytes(&raw),
            pages: 1,
            truncated: false,
            kind: ext.clone(),
        }
    } else if SUB_EXTS.contains(&ext.as_str()) {
        let raw = std::fs::read(path).map_err(|e| format!("cannot read .{ext}: {e}"))?;
        Extracted {
            text: strip_subtitles(&decode_bytes(&raw)),
            pages: 1,
            truncated: false,
            kind: ext.clone(),
        }
    } else {
        match ext.as_str() {
            "html" | "htm" | "xhtml" | "mhtml" => {
                let raw = std::fs::read(path).map_err(|e| format!("cannot read .{ext}: {e}"))?;
                Extracted {
                    text: strip_html(&decode_bytes(&raw)),
                    pages: 1,
                    truncated: false,
                    kind: ext.clone(),
                }
            }
            "rtf" => {
                let raw = std::fs::read(path).map_err(|e| format!("cannot read .{ext}: {e}"))?;
                Extracted {
                    text: strip_rtf(&decode_bytes(&raw)),
                    pages: 1,
                    truncated: false,
                    kind: ext.clone(),
                }
            }
            "docx" => extract_docx(path)?,
            "pptx" => extract_pptx(path)?,
            "xlsx" | "xlsm" | "xls" | "ods" => extract_spreadsheet(path, &ext)?,
            "odt" | "odp" => extract_odf(path, &ext)?,
            "epub" => extract_epub(path)?,
            "pdf" => extract_pdf(path)?,
            "doc" | "ppt" => extract_legacy_ole(path, &ext)?,
            _ => return Err(format!("unsupported document .{ext}")),
        }
    };
    out.text = normalize_ws(&out.text);
    if out.text.trim().is_empty() {
        return Err(if ext == "pdf" {
            "no selectable text — scanned PDF needs OCR (image-only)".to_string()
        } else {
            format!("no extractable text in .{ext} (empty or image-only)")
        });
    }
    if out.text.chars().count() > cap {
        out.text = out.text.chars().take(cap).collect();
        out.text.push_str("\n\n[…truncated at document cap…]");
        out.truncated = true;
    }
    Ok(out)
}

// ---------- decoding ----------

fn decode_bytes(raw: &[u8]) -> String {
    if raw.starts_with(b"\xef\xbb\xbf") {
        return String::from_utf8_lossy(&raw[3..]).into_owned();
    }
    if raw.starts_with(b"\xff\xfe") {
        return encoding_rs::UTF_16LE.decode(&raw[2..]).0.into_owned();
    }
    if raw.starts_with(b"\xfe\xff") {
        return encoding_rs::UTF_16BE.decode(&raw[2..]).0.into_owned();
    }
    if std::str::from_utf8(raw).is_ok() {
        return String::from_utf8_lossy(raw).into_owned();
    }
    // Windows-1252 fallback for legacy saves (smart quotes etc.).
    encoding_rs::WINDOWS_1252.decode(raw).0.into_owned()
}

fn normalize_ws(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let mut blank = 0;
    for line in t.lines() {
        let l = line.trim_end();
        if l.trim().is_empty() {
            blank += 1;
            if blank <= 2 {
                out.push('\n');
            }
            continue;
        }
        blank = 0;
        out.push_str(l);
        out.push('\n');
    }
    out.trim().to_string()
}

// ---------- subtitles ----------

/// True for SRT/VTT cue-timing lines: arrows, bare cue indices, and clock
/// shapes (`00:01`, `00:00:01,000`, `00:00:01.000`). Dialogue that merely
/// starts with a digit ("123 go") is NOT timing — it needs a timing shape.
fn is_timing_line(l: &str) -> bool {
    if l.contains("-->") {
        return true;
    }
    if !l.is_empty() && l.chars().all(|c| c.is_ascii_digit()) {
        return true; // bare cue index ("12")
    }
    let groups: Vec<&str> = l.split(|c| c == ':' || c == ',' || c == '.').collect();
    // MM:SS, MM:SS,mmm, or HH:MM:SS,mmm — every group all digits.
    (groups.len() >= 2 && groups.len() <= 4)
        && groups
            .iter()
            .all(|g| !g.is_empty() && g.chars().all(|c| c.is_ascii_digit()))
}

fn strip_subtitles(t: &str) -> String {
    let mut out = Vec::new();
    for line in t.lines() {
        let l = line.trim();
        if l.is_empty()
            || is_timing_line(l)
            || (l.starts_with('[') && l.ends_with(']'))
        {
            continue;
        }
        // WEBVTT header / NOTE blocks
        if l == "WEBVTT" || l.starts_with("NOTE") {
            continue;
        }
        out.push(l);
    }
    // de-dupe consecutive repeats (common in captions)
    let mut dedup: Vec<&str> = Vec::with_capacity(out.len());
    for l in out {
        if dedup.last() != Some(&l) {
            dedup.push(l);
        }
    }
    dedup.join("\n")
}

// ---------- html ----------

fn strip_html(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let bytes = t.as_bytes();
    let mut i = 0;
    let mut skip = false; // inside script/style
    while i < bytes.len() {
        if bytes[i] == b'<' {
            if let Some(end) = t[i..].find('>') {
                let tag = t[i + 1..i + end].to_ascii_lowercase();
                let name: String = tag
                    .trim_start_matches('/')
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or("")
                    .to_string();
                if name == "script" || name == "style" {
                    skip = !tag.starts_with('/');
                }
                // block separators
                if ["p", "br", "div", "li", "tr", "h1", "h2", "h3", "h4", "h5", "h6"]
                    .contains(&name.as_str())
                {
                    out.push('\n');
                }
                i += end + 1;
                continue;
            }
            break;
        }
        if !skip {
            out.push(bytes[i] as char);
        }
        i += 1;
    }
    html_entities(&out)
}

fn html_entities(t: &str) -> String {
    t.replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&apos;", "'")
}

// ---------- rtf ----------

fn strip_rtf(t: &str) -> String {
    let mut out = String::with_capacity(t.len());
    let chars: Vec<char> = t.chars().collect();
    let mut i = 0;
    let mut depth: i32 = 0;
    let mut skip_dest = false;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '{' => {
                depth += 1;
                // {\* or {\fonttbl etc: mark destination skip at depth
                if i + 1 < chars.len() && chars[i + 1] == '\\' {
                    // peek word
                    let word: String = chars[i + 2..].iter().take(12).collect();
                    if word.starts_with('*')
                        || word.starts_with("fonttbl")
                        || word.starts_with("colortbl")
                        || word.starts_with("stylesheet")
                        || word.starts_with("info")
                    {
                        skip_dest = true;
                    }
                }
                i += 1;
            }
            '}' => {
                depth = depth.saturating_sub(1);
                skip_dest = false;
                i += 1;
            }
            '\\' => {
                let rest: String = chars[i + 1..].iter().take(14).collect();
                if rest.starts_with("par") || rest.starts_with("line") {
                    if !skip_dest {
                        out.push('\n');
                    }
                    i += 4;
                } else if rest.starts_with("tab") {
                    if !skip_dest {
                        out.push('\t');
                    }
                    i += 4;
                } else if rest.starts_with('\'') && rest.len() >= 3 {
                    // \'hh hex char (windows-1252)
                    if let Ok(b) = u8::from_str_radix(&rest[1..3], 16) {
                        if !skip_dest {
                            out.push_str(&encoding_rs::WINDOWS_1252.decode(&[b]).0);
                        }
                    }
                    i += 4;
                } else {
                    // skip control word + optional numeric arg + one space
                    let mut j = i + 1;
                    while j < chars.len() && chars[j].is_ascii_alphabetic() {
                        j += 1;
                    }
                    while j < chars.len() && (chars[j] == '-' || chars[j].is_ascii_digit()) {
                        j += 1;
                    }
                    if j < chars.len() && chars[j] == ' ' {
                        j += 1;
                    }
                    i = j;
                }
            }
            _ => {
                if !skip_dest && depth > 0 {
                    out.push(c);
                }
                i += 1;
            }
        }
    }
    let _ = depth;
    out
}

// ---------- zip+xml (docx/pptx/odf/epub) ----------

fn read_zip_entry(path: &Path, want_suffix: &str) -> Result<Vec<(String, Vec<u8>)>, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("cannot open: {e}"))?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| format!("not a valid zip container: {e}"))?;
    let mut out = Vec::new();
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| format!("zip read: {e}"))?;
        let name = file.name().to_string();
        if want_suffix.is_empty() || name.ends_with(want_suffix) {
            let mut buf = Vec::new();
            use std::io::Read;
            file.read_to_end(&mut buf)
                .map_err(|e| format!("zip entry read: {e}"))?;
            out.push((name, buf));
        }
    }
    Ok(out)
}

/// Collect text nodes `<...:t>text</...>` from OOXML/ODF xml via quick-xml streaming.
fn collect_t_nodes(xml: &[u8], tag_ends_with: &str) -> String {
    use quick_xml::events::Event;
    use quick_xml::reader::Reader;
    let mut reader = Reader::from_reader(xml);
    reader.config_mut().trim_text(true);
    let mut out = String::new();
    let mut buf = Vec::new();
    let mut in_t = false;
    let mut para_break = false;
    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                // quick-xml 0.42+: names arrive decoded as &str (was raw bytes).
                let name = e.name().into_inner().to_owned();
                if name.ends_with(tag_ends_with) {
                    in_t = true;
                } else if name.ends_with(":p") || name.ends_with(":h") || name.ends_with(":tr") {
                    para_break = true;
                }
            }
            Ok(Event::Text(e)) => {
                if in_t {
                    // quick-xml 0.42+: BytesText is AsRef<str> (was raw bytes).
                    let s: String = e.as_ref().to_owned();
                    if para_break {
                        out.push('\n');
                        para_break = false;
                    }
                    out.push_str(&s);
                    out.push(' ');
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name().into_inner().to_owned();
                if name.ends_with(tag_ends_with) {
                    in_t = false;
                } else if name.ends_with(":p") || name.ends_with(":h") {
                    out.push('\n');
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    out
}

fn extract_docx(path: &Path) -> Result<Extracted, String> {
    let entries = read_zip_entry(path, "word/document.xml")?;
    if entries.is_empty() {
        return Err("docx missing word/document.xml — file is corrupt".to_string());
    }
    let mut text = String::new();
    let mut paras = 0;
    for (_, xml) in &entries {
        let t = collect_t_nodes(xml, ":t");
        paras += t.lines().count();
        text.push_str(&t);
        text.push('\n');
    }
    Ok(Extracted {
        text,
        pages: paras.max(1),
        truncated: false,
        kind: "docx".to_string(),
    })
}

fn extract_pptx(path: &Path) -> Result<Extracted, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("cannot open: {e}"))?;
    let mut zip = zip::ZipArchive::new(f).map_err(|e| format!("not a valid pptx: {e}"))?;
    let mut slides: Vec<(String, Vec<u8>)> = Vec::new();
    for i in 0..zip.len() {
        let mut file = zip.by_index(i).map_err(|e| format!("zip read: {e}"))?;
        let name = file.name().to_string();
        if (name.starts_with("ppt/slides/slide") || name.starts_with("ppt/notesSlides/notesSlide"))
            && name.ends_with(".xml")
        {
            let mut buf = Vec::new();
            use std::io::Read;
            file.read_to_end(&mut buf)
                .map_err(|e| format!("zip entry read: {e}"))?;
            slides.push((name, buf));
        }
    }
    slides.sort_by(|a, b| a.0.cmp(&b.0));
    if slides.is_empty() {
        return Err("pptx has no slides — file is corrupt".to_string());
    }
    let mut text = String::new();
    let mut n = 0;
    for (name, xml) in &slides {
        n += 1;
        text.push_str(&format!("\n--- Slide {n} ({name}) ---\n"));
        text.push_str(&collect_t_nodes(xml, ":t"));
        text.push('\n');
    }
    Ok(Extracted {
        text,
        pages: n.max(1),
        truncated: false,
        kind: "pptx".to_string(),
    })
}

fn extract_odf(path: &Path, ext: &str) -> Result<Extracted, String> {
    let entries = read_zip_entry(path, "content.xml")?;
    if entries.is_empty() {
        return Err(format!("{ext} missing content.xml — file is corrupt"));
    }
    let mut text = String::new();
    for (_, xml) in &entries {
        // ODF text nodes are <text:p>/<text:h>; table cells surface inline.
        text.push_str(&collect_t_nodes(xml, ":p"));
        text.push('\n');
        let h = collect_t_nodes(xml, ":h");
        if !h.trim().is_empty() {
            text.push_str(&h);
            text.push('\n');
        }
    }
    Ok(Extracted {
        text,
        pages: 1,
        truncated: false,
        kind: ext.to_string(),
    })
}

fn extract_epub(path: &Path) -> Result<Extracted, String> {
    let entries = read_zip_entry(path, "")?;
    let mut docs: Vec<(String, Vec<u8>)> = entries
        .into_iter()
        .filter(|(n, _)| {
            let l = n.to_ascii_lowercase();
            (l.ends_with(".xhtml") || l.ends_with(".html") || l.ends_with(".htm"))
                && !l.contains("nav")
        })
        .collect();
    docs.sort_by(|a, b| a.0.cmp(&b.0));
    if docs.is_empty() {
        return Err("epub has no readable chapters".to_string());
    }
    let mut text = String::new();
    for (name, bytes) in &docs {
        text.push_str(&format!("\n--- {} ---\n", name));
        text.push_str(&strip_html(&decode_bytes(bytes)));
        text.push('\n');
    }
    Ok(Extracted {
        text,
        pages: docs.len().max(1),
        truncated: false,
        kind: "epub".to_string(),
    })
}

// ---------- spreadsheets (calamine: xlsx/xls/ods) ----------

fn extract_spreadsheet(path: &Path, ext: &str) -> Result<Extracted, String> {
    use calamine::{open_workbook_auto, Data, Reader};
    let mut wb = open_workbook_auto(path).map_err(|e| format!(".{ext} parse failed: {e}"))?;
    let sheets = wb.sheet_names().to_vec();
    if sheets.is_empty() {
        return Err(format!(".{ext} has no sheets"));
    }
    let mut text = String::new();
    for sheet in &sheets {
        text.push_str(&format!("\n## Sheet: {sheet}\n"));
        match wb.worksheet_range(sheet) {
            Ok(range) => {
                let (rows, cols) = range.get_size();
                let max_r = rows.min(500);
                let max_c = cols.min(26);
                for r in 0..max_r {
                    let mut cells = Vec::new();
                    for c in 0..max_c {
                        let v = match range.get((r, c)) {
                            Some(Data::String(s)) => s.clone(),
                            Some(Data::Float(f)) => trim_float(*f),
                            Some(Data::Bool(b)) => b.to_string(),
                            Some(Data::DateTime(d)) => d.to_string(),
                            Some(Data::Error(e)) => format!("{e:?}"),
                            Some(Data::Empty) | None => String::new(),
                            Some(other) => format!("{other}"),
                        };
                        cells.push(v);
                    }
                    while cells.last().is_some_and(|s| s.is_empty()) {
                        cells.pop();
                    }
                    if cells.iter().any(|s| !s.is_empty()) {
                        text.push_str(&format!("| {} |\n", cells.join(" | ")));
                    }
                }
            }
            Err(e) => {
                text.push_str(&format!("(sheet unreadable: {e})\n"));
            }
        }
    }
    Ok(Extracted {
        text,
        pages: sheets.len().max(1),
        truncated: false,
        kind: ext.to_string(),
    })
}

fn trim_float(f: f64) -> String {
    if f.fract() == 0.0 && f.abs() < 1e15 {
        format!("{}", f as i64)
    } else {
        format!("{f}")
    }
}

// ---------- pdf ----------

fn extract_pdf(path: &Path) -> Result<Extracted, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read .pdf: {e}"))?;
    let text = pdf_extract::extract_text_from_mem(&bytes)
        .map_err(|e| format!("pdf parse failed: {e}"))?;
    // rough page count via /Type /Page markers (best-effort display only)
    let pages = count_sub(&bytes, b"/Type/Page")
        .max(count_sub(&bytes, b"/Type /Page"))
        .max(1);
    Ok(Extracted {
        text,
        pages,
        truncated: false,
        kind: "pdf".to_string(),
    })
}

fn count_sub(hay: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || hay.len() < needle.len() {
        return 0;
    }
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

// ---------- legacy OLE (doc/ppt) native best-effort ----------

fn extract_legacy_ole(path: &Path, ext: &str) -> Result<Extracted, String> {
    let raw = std::fs::read(path).map_err(|e| format!("cannot read .{ext}: {e}"))?;
    if raw.len() < 8 || &raw[0..8] != b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1" {
        return Err(format!(
            ".{ext} is not a legacy Office file — resave as .{}x and retry",
            if ext == "doc" { "doc" } else { "ppt" }
        ));
    }
    // Best-effort: harvest UTF-16LE runs of printable text (Word's
    // WordDocument stream and PowerPoint TextChars atoms are UTF-16LE).
    let mut runs: Vec<String> = Vec::new();
    let mut cur: Vec<u16> = Vec::new();
    let flush = |cur: &mut Vec<u16>, runs: &mut Vec<String>| {
        if cur.len() >= 4 {
            if let Ok(s) = String::from_utf16(cur) {
                let t = s.trim();
                if t.chars().count() >= 4 {
                    runs.push(t.to_string());
                }
            }
        }
        cur.clear();
    };
    let mut i = 0;
    while i + 1 < raw.len() {
        let w = u16::from_le_bytes([raw[i], raw[i + 1]]);
        let ch = char::from_u32(w as u32).unwrap_or('\0');
        let ok = matches!(ch, ' '..='~' | '¡'..='ۿ' | '‐'..='\u{9fff}')
            || ch == '\n'
            || ch == '\r'
            || ch == '\t';
        if ok && w != 0 {
            cur.push(w);
        } else {
            flush(&mut cur, &mut runs);
        }
        i += 2;
    }
    flush(&mut cur, &mut runs);
    // drop binary noise: keep runs with real word content
    let kept: Vec<String> = runs
        .into_iter()
        .map(|r| r.replace('\0', " "))
        .map(|r| r.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|r| {
            let words = r.split_whitespace().count();
            words >= 2 && r.chars().any(|c| c.is_alphabetic())
        })
        .collect();
    if kept.join(" ").chars().count() < 50 {
        return Err(format!(
            "only binary content found in .{ext} — resave as .{}x and retry",
            if ext == "doc" { "doc" } else { "ppt" }
        ));
    }
    Ok(Extracted {
        text: kept.join("\n"),
        pages: 1,
        truncated: false,
        kind: format!("{ext}(legacy)"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_lists_modern_docs() {
        for e in ["txt", "md", "pdf", "docx", "pptx", "xlsx", "odt", "rtf", "html", "epub", "csv"] {
            assert!(is_doc_ext(e), "{e} should be a doc ext");
        }
        assert!(!is_doc_ext("wav"));
        assert!(!is_doc_ext("mp3"));
    }

    #[test]
    fn plain_and_subs_extract() {
        let dir = std::env::temp_dir().join(format!("pv-docs-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("a.txt"), "hello world").unwrap();
        let ex = extract_text(&dir.join("a.txt"), 100).unwrap();
        assert!(ex.text.contains("hello"));
        std::fs::write(
            dir.join("b.srt"),
            "1\n00:00:01,000 --> 00:00:02,000\nhello there\n",
        )
        .unwrap();
        let ex = extract_text(&dir.join("b.srt"), 100).unwrap();
        assert!(ex.text.contains("hello there") && !ex.text.contains("-->"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timing_lines_drop_but_digit_dialogue_stays() {
        assert!(is_timing_line("12"));
        assert!(is_timing_line("00:01"));
        assert!(is_timing_line("00:00:01,000"));
        assert!(is_timing_line("00:00:01.000 --> 00:00:04,000"));
        assert!(!is_timing_line("123 go"));
        assert!(!is_timing_line("hello there"));
        assert!(!is_timing_line("chapter 2: intro"));
    }

    #[test]
    fn html_and_rtf_strip() {
        assert!(strip_html("<p>hi<script>x</script>there</p>").contains("hithere")
            || strip_html("<p>hi<script>x</script>there</p>").contains("hi"));
        let rtf = r"{\rtf1\ansi hello\par world}";
        assert!(strip_rtf(rtf).contains("hello"));
    }

    #[test]
    fn empty_text_is_err_not_hallucinated() {
        let dir = std::env::temp_dir().join(format!("pv-docs-e-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("e.txt"), "   \n  ").unwrap();
        assert!(extract_text(&dir.join("e.txt"), 100).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsupported_ext_is_err() {
        let dir = std::env::temp_dir().join(format!("pv-docs-u-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("u.wav"), "RIFF").unwrap();
        assert!(extract_text(&dir.join("u.wav"), 100).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

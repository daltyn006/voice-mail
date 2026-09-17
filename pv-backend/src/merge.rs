//! Similarity grouping + merged-note builder (offline, deterministic).
//!
//! TF-IDF cosine over summaries (or extract heads): no embedding model,
//! no VRAM, instant and unit-testable. Two bands per the locked plan:
//! `suggest` (lo..hi) → quiet chip, `prompt` (>= hi) → auto-prompt bar.
//! Merging itself is concatenation with `--- Source ---` delimiters fed to
//! the existing MAP→REDUCE; this module only builds that input text.

use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct MergeItem {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct MergeGroup {
    pub members: Vec<MergeItem>,
    pub score: f32,
}

fn tokens(t: &str) -> Vec<String> {
    t.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() >= 3)
        .map(|s| s.to_string())
        .collect()
}

/// TF-IDF cosine between two texts (pair corpus of 2 docs).
/// Pure + deterministic; 0.0 = disjoint, 1.0 = identical bags.
pub fn tfidf_cosine(a: &str, b: &str) -> f32 {
    let ta = tokens(a);
    let tb = tokens(b);
    if ta.is_empty() || tb.is_empty() {
        return 0.0;
    }
    let mut df: HashMap<&str, usize> = HashMap::new();
    {
        let sa: HashSet<&str> = ta.iter().map(|s| s.as_str()).collect();
        let sb: HashSet<&str> = tb.iter().map(|s| s.as_str()).collect();
        for t in sa.union(&sb) {
            let mut d = 0;
            if sa.contains(t) {
                d += 1;
            }
            if sb.contains(t) {
                d += 1;
            }
            df.insert(t, d);
        }
    }
    let mut tfa: HashMap<&str, f32> = HashMap::new();
    let mut tfb: HashMap<&str, f32> = HashMap::new();
    for t in &ta {
        *tfa.entry(t.as_str()).or_default() += 1.0;
    }
    for t in &tb {
        *tfb.entry(t.as_str()).or_default() += 1.0;
    }
    // Term frequency per own document length (not the max of both — a short
    // doc shares most of its terms with a long one, and normalizing by the
    // long doc would systematically down-weight it).
    let na = ta.len().max(1) as f32;
    for v in tfa.values_mut() {
        *v /= na;
    }
    let nb = tb.len().max(1) as f32;
    for v in tfb.values_mut() {
        *v /= nb;
    }
    // idf over the 2-doc corpus, smoothed
    let idf = |t: &str| -> f32 {
        let d = *df.get(t).unwrap_or(&1) as f32;
        ((2.0 + 1.0) / (d + 1.0)).ln() + 1.0
    };
    let mut dot = 0.0;
    let mut na = 0.0;
    let mut nb = 0.0;
    let mut keys: HashSet<&str> = HashSet::new();
    for k in tfa.keys() {
        keys.insert(k);
    }
    for k in tfb.keys() {
        keys.insert(k);
    }
    for k in keys {
        let wa = tfa.get(k).copied().unwrap_or(0.0) * idf(k);
        let wb = tfb.get(k).copied().unwrap_or(0.0) * idf(k);
        dot += wa * wb;
        na += wa * wa;
        nb += wb * wb;
    }
    if na <= 0.0 || nb <= 0.0 {
        return 0.0;
    }
    (dot / (na.sqrt() * nb.sqrt())).clamp(0.0, 1.0)
}

/// Group items by pairwise score >= `lo`. Returns `(suggest, prompt)` where
/// `prompt` groups have max pairwise score >= `hi`. Connected components.
pub fn suggest_groups(
    items: &[MergeItem],
    lo: f32,
    hi: f32,
) -> (Vec<MergeGroup>, Vec<MergeGroup>) {
    if items.len() < 2 {
        return (Vec::new(), Vec::new());
    }
    let n = items.len();
    let mut adj: Vec<Vec<(usize, f32)>> = vec![Vec::new(); n];
    for i in 0..n {
        for j in (i + 1)..n {
            let s = tfidf_cosine(&items[i].text, &items[j].text);
            if s >= lo {
                adj[i].push((j, s));
                adj[j].push((i, s));
            }
        }
    }
    let mut seen = vec![false; n];
    let mut suggest = Vec::new();
    let mut prompt = Vec::new();
    for i in 0..n {
        if seen[i] {
            continue;
        }
        // BFS component
        let mut comp = Vec::new();
        let mut stack = vec![i];
        seen[i] = true;
        let mut best = 0.0f32;
        while let Some(u) = stack.pop() {
            comp.push(u);
            for (v, s) in &adj[u] {
                best = best.max(*s);
                if !seen[*v] {
                    seen[*v] = true;
                    stack.push(*v);
                }
            }
        }
        if comp.len() >= 2 {
            let members = comp.into_iter().map(|k| items[k].clone()).collect();
            let g = MergeGroup { members, score: best };
            if best >= hi {
                prompt.push(g);
            } else {
                suggest.push(g);
            }
        }
    }
    // highest score first
    suggest.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    prompt.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
    (suggest, prompt)
}

/// Build the merged MAP→REDUCE input: Sources ledger on top, then delimited bodies.
pub fn build_merged_input(members: &[MergeItem], mode: &str) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "MERGE MODE: {}\n{} source(s). Summarize jointly; preserve every metric/date/name/decision.\n\n",
        mode,
        members.len()
    ));
    for m in members {
        out.push_str(&format!("--- Source: {} ({}) ---\n", m.name, m.kind));
    }
    out.push('\n');
    for m in members {
        out.push_str(&format!("--- Source: {} ({}) ---\n{}\n\n", m.name, m.kind, m.text));
    }
    out
}

/// Render the merged `.md` body: Sources section FIRST (locked decision),
/// then Summary, then per-source texts.
pub fn render_merged_md(title: &str, members: &[MergeItem], summary: &str) -> String {
    let mut md = format!("#{title}\n\n## Sources\n");
    for m in members {
        md.push_str(&format!("- {} ({}, {} chars)\n", m.name, m.kind, m.text.chars().count()));
    }
    md.push_str("\n## Summary\n");
    if summary.trim().is_empty() {
        md.push_str("*(summary skipped — raw transcript only)*\n");
    } else {
        md.push_str(summary.trim());
        md.push('\n');
    }
    md.push_str("\n## Source Texts\n");
    for m in members {
        md.push_str(&format!("### {}\n{}\n\n", m.name, m.text.trim()));
    }
    md
}

/// Stable group key for dismiss persistence (sorted member names).
pub fn group_key(members: &[MergeItem]) -> String {
    let mut names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
    names.sort_unstable();
    names.join(" + ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_texts_score_one() {
        let s = tfidf_cosine("mitosis phases prophase metaphase", "mitosis phases prophase metaphase");
        assert!(s > 0.99);
    }

    #[test]
    fn disjoint_texts_score_zero() {
        let s = tfidf_cosine("mitosis cell biology", "quantum chromodynamics gauge");
        assert!(s < 0.2);
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(tfidf_cosine("", "hello world foo"), 0.0);
    }

    #[test]
    fn grouping_bands() {
        let items = vec![
            MergeItem { id: "a".into(), name: "a".into(), kind: "audio".into(), text: "mitosis cell division prophase metaphase anaphase telophase biology lecture".into() },
            MergeItem { id: "b".into(), name: "b".into(), kind: "doc".into(), text: "mitosis cell division prophase metaphase anaphase telophase biology notes".into() },
            MergeItem { id: "c".into(), name: "c".into(), kind: "doc".into(), text: "quantum field theory gauge bosons renormalization".into() },
        ];
        let (suggest, prompt) = suggest_groups(&items, 0.35, 0.80);
        assert_eq!(prompt.len() + suggest.len(), 1);
        let g = prompt.first().or(suggest.first()).unwrap();
        assert_eq!(g.members.len(), 2);
    }

    #[test]
    fn merged_input_has_sources_on_top() {
        let members = vec![MergeItem {
            id: "1".into(),
            name: "a.md".into(),
            kind: "audio".into(),
            text: "hello".into(),
        }];
        let md = render_merged_md("Merged - Day 1 - x", &members, "sum");
        let s = md.find("## Sources").unwrap();
        let u = md.find("## Summary").unwrap();
        assert!(s < u);
    }
}

use anyhow::{Context, Result, anyhow};
use html2md::parse_html;
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::Client;
use reqwest::header::CONTENT_TYPE;
use scraper::{Html, Selector};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use websearch::providers::duckduckgo::{DuckDuckGoConfig, DuckDuckGoProvider};
use websearch::{SearchOptions, web_search};

#[derive(Debug, Clone)]
pub struct MdPage {
    pub query: String,
    pub url: String,
    pub status: u16,
    pub title: Option<String>,
    pub outline: Vec<String>,
    pub markdown: String,
}

#[derive(Debug, Clone)]
pub struct LlmCleanConfig {
    pub concurrency: usize,
    pub timeout_secs: u64,
    pub require_html_content_type: bool,
    pub drop_non_success_status: bool,
    pub max_html_bytes: usize,
    pub max_md_chars: usize,
    pub min_md_chars: usize,
    pub max_link_lines_to_keep: usize,
    pub link_farm_run_threshold: usize,
    pub max_line_len: usize,
    pub max_outline_headings: usize,
}

impl Default for LlmCleanConfig {
    fn default() -> Self {
        Self {
            concurrency: 16,
            timeout_secs: 20,
            require_html_content_type: true,
            drop_non_success_status: true,
            max_html_bytes: 2_000_000,
            max_md_chars: 24_000,
            min_md_chars: 200,
            max_link_lines_to_keep: 40,
            link_farm_run_threshold: 25,
            max_line_len: 2_000,
            max_outline_headings: 24,
        }
    }
}

// Precompiled regex for speed
static RE_SCRIPT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<script\b[^>]*>.*?</script>").unwrap());
static RE_STYLE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?is)<style\b[^>]*>.*?</style>").unwrap());
static RE_NOSCRIPT: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?is)<noscript\b[^>]*>.*?</noscript>").unwrap());

// Remove inline data:image/... blobs in markdown link targets (token poison)
static RE_DATA_IMG: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?is)\(data:image/[^)]*\)").unwrap());

// “Just a link bullet” line
static RE_LINK_ONLY: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"^\s*[-*+]\s+\[[^\]]+\]\([^)]+\)\s*$"#).unwrap());

/// Public API: array of queries + results per query.
pub async fn search(queries: &[String], results_per_query: u32) -> Result<Vec<MdPage>> {
    search_with_config(queries, results_per_query, &LlmCleanConfig::default()).await
}

/// Same as `search`, but configurable.
pub async fn search_with_config(
    queries: &[String],
    results_per_query: u32,
    cfg: &LlmCleanConfig,
) -> Result<Vec<MdPage>> {
    if queries.is_empty() || results_per_query == 0 {
        return Ok(vec![]);
    }

    // 1) DDG search via websearch (no API keys).
    let mut jobs: Vec<(String, String, Option<String>)> = Vec::new();
    let mut seen_urls: HashSet<String> = HashSet::new();

    for q in queries {
        let provider = DuckDuckGoProvider::with_config(DuckDuckGoConfig::default());

        let results = web_search(SearchOptions {
            query: q.clone(),
            max_results: Some(results_per_query),
            provider: Box::new(provider),
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow!("search failed for query='{q}': {e}"))?;

        for r in results {
            if seen_urls.insert(r.url.clone()) {
                // FIX #1: r.title is String, but we store Option<String>.
                let title_opt = if r.title.trim().is_empty() {
                    None
                } else {
                    Some(r.title.clone())
                };
                jobs.push((q.clone(), r.url, title_opt));
            }
        }
    }

    if jobs.is_empty() {
        return Ok(vec![]);
    }

    // 2) Fast parallel fetch + extract + clean + convert.
    let client = Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .timeout(std::time::Duration::from_secs(cfg.timeout_secs))
        .pool_max_idle_per_host(8)
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .context("failed to build reqwest client")?;

    let sem = Arc::new(Semaphore::new(cfg.concurrency));
    let mut set: JoinSet<Result<Option<MdPage>>> = JoinSet::new();

    for (query, url, title) in jobs {
        let client = client.clone();
        let sem = sem.clone();
        let cfg = cfg.clone();

        set.spawn(async move {
            let _permit = sem.acquire().await.expect("semaphore closed");
            crawl_to_llm_markdown(&client, &cfg, &query, &url, title).await
        });
    }

    let mut out = Vec::new();
    while let Some(res) = set.join_next().await {
        match res {
            Ok(Ok(Some(page))) => out.push(page),
            Ok(Ok(None)) => {} // dropped by filters
            Ok(Err(e)) => eprintln!("crawl error: {e:#}"),
            Err(e) => eprintln!("task join error: {e}"),
        }
    }

    Ok(out)
}

async fn crawl_to_llm_markdown(
    client: &Client,
    cfg: &LlmCleanConfig,
    query: &str,
    url: &str,
    title_from_search: Option<String>,
) -> Result<Option<MdPage>> {
    let resp = client
        .get(url)
        .header("Accept", "text/html,application/xhtml+xml")
        .send()
        .await
        .with_context(|| format!("request failed: {url}"))?;

    let status = resp.status().as_u16();

    if cfg.drop_non_success_status && !(200..=299).contains(&status) {
        return Ok(None);
    }

    if cfg.require_html_content_type {
        let is_html = resp
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|v| v.to_ascii_lowercase().contains("text/html"))
            .unwrap_or(false);

        if !is_html {
            return Ok(None);
        }
    }

    let bytes = resp
        .bytes()
        .await
        .with_context(|| format!("failed reading body: {url}"))?;

    let slice = if bytes.len() > cfg.max_html_bytes {
        &bytes[..cfg.max_html_bytes]
    } else {
        bytes.as_ref()
    };

    let html = String::from_utf8_lossy(slice).into_owned();

    // Extract “main-ish” HTML to reduce nav/boilerplate.
    let extracted_html = extract_main_content_html(&html).unwrap_or_else(|| html.clone());

    // Strip script/style/noscript blocks before html2md.
    let stripped_html = strip_script_style_noscript(&extracted_html);

    // Convert.
    let mut md = parse_html(&stripped_html);

    // Clean for LLMs.
    md = clean_markdown_for_llm(&md, cfg);

    if md.chars().count() < cfg.min_md_chars {
        return Ok(None);
    }

    let outline = extract_outline(&md, cfg.max_outline_headings);

    let inferred_title = outline.get(0).cloned();
    let title = title_from_search.or(inferred_title);

    // Compact header to help downstream ingestion/ranking.
    let mut final_md = String::new();
    final_md.push_str("---\n");
    final_md.push_str(&format!("query: {query}\n"));
    final_md.push_str(&format!("url: {url}\n"));
    final_md.push_str(&format!("status: {status}\n"));
    if let Some(t) = &title {
        final_md.push_str(&format!("title: {t}\n"));
    }
    final_md.push_str("---\n\n");

    if !outline.is_empty() {
        final_md.push_str("## Outline\n");
        for h in &outline {
            final_md.push_str("- ");
            final_md.push_str(h);
            final_md.push('\n');
        }
        final_md.push('\n');
    }

    final_md.push_str("## Content\n\n");
    final_md.push_str(&md);

    // Hard cap final size.
    if final_md.chars().count() > cfg.max_md_chars {
        final_md = truncate_at_char_boundary(&final_md, cfg.max_md_chars);
        final_md.push_str("\n\n[...truncated...]\n");
    }

    Ok(Some(MdPage {
        query: query.to_string(),
        url: url.to_string(),
        status,
        title,
        outline,
        markdown: final_md,
    }))
}

/// Heuristic “main content” extractor.
fn extract_main_content_html(html: &str) -> Option<String> {
    let doc = Html::parse_document(html);

    let selectors = [
        "main",
        "article",
        r#"[role="main"]"#,
        "#content",
        "#main-content",
        "#main",
        ".content",
        ".markdown-body",
        ".rustdoc",
        "body",
    ];

    for sel in selectors {
        let selector = match Selector::parse(sel) {
            Ok(s) => s,
            Err(_) => continue,
        };

        if let Some(el) = doc.select(&selector).next() {
            let inner = el.inner_html();
            if inner.trim().len() > 200 {
                return Some(format!(r#"<div id="extracted">{inner}</div>"#));
            }
        }
    }
    None
}

fn strip_script_style_noscript(html: &str) -> String {
    let s = RE_SCRIPT.replace_all(html, "");
    let s = RE_STYLE.replace_all(&s, "");
    let s = RE_NOSCRIPT.replace_all(&s, "");
    s.into_owned()
}

fn clean_markdown_for_llm(md: &str, cfg: &LlmCleanConfig) -> String {
    let mut s = md.replace("\r\n", "\n").replace('\0', "");
    s = RE_DATA_IMG.replace_all(&s, "(image omitted)").into_owned();

    // 1) Drop absurdly long lines early (minified junk, blobs).
    let mut lines: Vec<String> = Vec::new();
    for line in s.lines() {
        if line.chars().count() <= cfg.max_line_len {
            lines.push(line.to_string());
        }
    }

    // 2) Prune “link farms” (large runs of bullet-link-only lines).
    let mut pruned: Vec<String> = Vec::with_capacity(lines.len());
    let mut run: Vec<String> = Vec::new();

    let flush_run = |run: &mut Vec<String>, out: &mut Vec<String>| {
        if run.len() >= cfg.link_farm_run_threshold {
            for l in run.iter().take(cfg.max_link_lines_to_keep) {
                out.push(l.clone());
            }
        } else {
            out.extend(run.drain(..));
        }
        run.clear();
    };

    for line in lines.into_iter() {
        if RE_LINK_ONLY.is_match(&line) {
            run.push(line);
        } else {
            flush_run(&mut run, &mut pruned);
            pruned.push(line);
        }
    }
    flush_run(&mut run, &mut pruned);

    // 3) Normalize whitespace: collapse 3+ blank lines to 2.
    let mut normalized = String::new();
    let mut blank_run = 0usize;

    for line in pruned {
        if line.trim().is_empty() {
            blank_run += 1;
            if blank_run <= 2 {
                normalized.push('\n');
            }
        } else {
            blank_run = 0;
            normalized.push_str(&line);
            normalized.push('\n');
        }
    }

    // 4) Cap before headers get added.
    let mut normalized = normalized.trim().to_string();
    if normalized.chars().count() > cfg.max_md_chars {
        normalized = truncate_at_char_boundary(&normalized, cfg.max_md_chars);
        normalized.push_str("\n\n[...truncated...]\n");
    }

    normalized
}

fn extract_outline(md: &str, max_items: usize) -> Vec<String> {
    let mut out = Vec::new();

    for line in md.lines() {
        let t = line.trim_start();
        if !t.starts_with('#') {
            continue;
        }

        // Count leading hashes safely.
        let mut hash_count = 0usize;
        for ch in t.chars() {
            if ch == '#' {
                hash_count += 1;
            } else {
                break;
            }
        }

        if hash_count == 0 {
            continue;
        }

        // Slice after the leading hashes (find byte index by iterating char_indices).
        let mut cut = 0usize;
        let mut seen = 0usize;
        for (i, ch) in t.char_indices() {
            if ch == '#' {
                seen += 1;
                if seen == hash_count {
                    cut = i + ch.len_utf8();
                    break;
                }
            }
        }

        let rest = t[cut..].trim();
        if !rest.is_empty() && rest.len() <= 120 {
            out.push(rest.to_string());
            if out.len() >= max_items {
                break;
            }
        }
    }

    out
}

fn truncate_at_char_boundary(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let mut end_byte = 0usize;
    let mut count = 0usize;
    for (i, ch) in s.char_indices() {
        if count >= max_chars {
            break;
        }
        end_byte = i + ch.len_utf8();
        count += 1;
    }
    s[..end_byte].to_string()
}

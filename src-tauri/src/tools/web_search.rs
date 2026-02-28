use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use scraper::{Html, Selector};
use serde_json::{json, Value};
use std::collections::HashSet;

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str { "web_search" }

    fn description(&self) -> &str {
        "Search the web for information using multiple search engines in parallel (DuckDuckGo, Bing, Baidu, 360). \
         Results are merged and deduplicated. Supports Chinese and English queries."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query (supports Chinese and English)"
                },
                "num_results": {
                    "type": "integer",
                    "description": "Number of results to return (default 8, max 15)"
                }
            },
            "required": ["query"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        let query = match input["query"].as_str() {
            Some(q) => q.trim(),
            None => return Ok(ToolResult::err("Missing required parameter: query")),
        };
        if query.is_empty() {
            return Ok(ToolResult::err("Query cannot be empty"));
        }
        let num = input["num_results"].as_u64().unwrap_or(8).min(15) as usize;

        let client = build_client();

        // Launch all engines in parallel
        let (ddg, bing, baidu, so) = tokio::join!(
            ddg_search(client.clone(), query, num),
            bing_search(client.clone(), query, num),
            baidu_search(client.clone(), query, num),
            so_search(client.clone(), query, num),
        );

        let mut all: Vec<SearchResult> = Vec::new();
        let mut seen_urls: HashSet<String> = HashSet::new();

        // Merge: interleave results from all engines for diversity
        let sources = [
            ("DuckDuckGo", ddg),
            ("Bing", bing),
            ("Baidu", baidu),
            ("360", so),
        ];

        let mut per_engine: Vec<(&str, Vec<SearchResult>)> = sources
            .into_iter()
            .map(|(name, r)| {
                let results = r.unwrap_or_default();
                if !results.is_empty() {
                    tracing::info!("web_search [{}] got {} results for: {}", name, results.len(), query);
                }
                (name, results)
            })
            .collect();

        // Round-robin merge until we have `num` results
        let mut i = 0;
        while all.len() < num {
            let mut any_added = false;
            for (_, results) in per_engine.iter_mut() {
                if i < results.len() {
                    let r = &results[i];
                    // Deduplicate by normalised URL
                    let key = normalise_url(&r.url);
                    if !key.is_empty() && seen_urls.insert(key) {
                        all.push(results[i].clone());
                        if all.len() >= num { break; }
                        any_added = true;
                    }
                }
            }
            if !any_added { break; }
            i += 1;
        }

        if all.is_empty() {
            return Ok(ToolResult::ok(format!(
                "搜索「{}」未获取到结果（已并行查询 DuckDuckGo / Bing / Baidu / 360）。\n\
                 可能是网络问题，建议使用 browser 工具直接访问相关网站。",
                query
            )));
        }

        Ok(ToolResult::ok(format_results(query, &all)))
    }
}

// ─────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
struct SearchResult {
    title:   String,
    url:     String,
    snippet: String,
}

fn build_client() -> Client {
    Client::builder()
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36")
        .timeout(std::time::Duration::from_secs(12))
        .build()
        .unwrap_or_default()
}

fn format_results(query: &str, results: &[SearchResult]) -> String {
    let mut out = format!("搜索结果：{}\n（已并行查询 DuckDuckGo / Bing / Baidu / 360，共 {} 条）\n\n", query, results.len());
    for (i, r) in results.iter().enumerate() {
        out.push_str(&format!(
            "{}. **{}**\n   {}\n   {}\n\n",
            i + 1,
            r.title,
            r.snippet,
            r.url
        ));
    }
    out
}

/// Normalise URL for deduplication: lowercase scheme+host+path, strip query/fragment.
fn normalise_url(url: &str) -> String {
    let u = url.trim().to_lowercase();
    // Strip scheme
    let u = u.trim_start_matches("https://")
              .trim_start_matches("http://")
              .trim_start_matches("//");
    // Strip query string and fragment
    let u = u.split('?').next().unwrap_or(u);
    let u = u.split('#').next().unwrap_or(u);
    // Strip trailing slash
    u.trim_end_matches('/').to_string()
}

// ─── DuckDuckGo ───────────────────────────────────────────────────────────────

async fn ddg_search(client: Client, query: &str, num: usize) -> Result<Vec<SearchResult>> {
    let url = format!(
        "https://html.duckduckgo.com/html/?q={}&kl=cn-zh",
        urlencoding::encode(query)
    );

    let html = client
        .get(&url)
        .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
        .send().await?.text().await?;

    let doc = Html::parse_document(&html);
    let result_sel  = Selector::parse(".result").map_err(|e| anyhow::anyhow!("{:?}", e))?;
    let title_sel   = Selector::parse(".result__title a, .result__a").map_err(|e| anyhow::anyhow!("{:?}", e))?;
    let snippet_sel = Selector::parse(".result__snippet").map_err(|e| anyhow::anyhow!("{:?}", e))?;

    let mut results = Vec::new();
    for r in doc.select(&result_sel).take(num * 2) {
        let Some(title_el) = r.select(&title_sel).next() else { continue };
        let title = title_el.text().collect::<String>().trim().to_string();
        if title.is_empty() { continue; }

        let href = title_el.value().attr("href").unwrap_or("");
        let url  = extract_ddg_url(href);
        if url.is_empty() { continue; }

        let snippet = r.select(&snippet_sel).next()
            .map(|e| e.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        results.push(SearchResult { title, url, snippet });
        if results.len() >= num { break; }
    }
    Ok(results)
}

fn extract_ddg_url(href: &str) -> String {
    if href.contains("uddg=") {
        if let Some(pos) = href.find("uddg=") {
            let encoded = href[pos + 5..].split('&').next().unwrap_or("");
            if let Ok(d) = urlencoding::decode(encoded) {
                return d.into_owned();
            }
        }
    }
    if href.starts_with("http") { href.to_string() }
    else if href.starts_with("//") { format!("https:{}", href) }
    else { String::new() }
}

// ─── Bing ─────────────────────────────────────────────────────────────────────

async fn bing_search(client: Client, query: &str, num: usize) -> Result<Vec<SearchResult>> {
    let url = format!(
        "https://www.bing.com/search?q={}&setlang=zh-CN&cc=CN&mkt=zh-CN",
        urlencoding::encode(query)
    );

    let html = client
        .get(&url)
        .header("Accept-Language", "zh-CN,zh;q=0.9")
        .send().await?.text().await?;

    let doc = Html::parse_document(&html);
    let result_sel  = Selector::parse("li.b_algo").map_err(|e| anyhow::anyhow!("{:?}", e))?;
    let title_sel   = Selector::parse("h2 a").map_err(|e| anyhow::anyhow!("{:?}", e))?;
    let snippet_sel = Selector::parse(".b_caption p, .b_algoSlug").map_err(|e| anyhow::anyhow!("{:?}", e))?;

    let mut results = Vec::new();
    for r in doc.select(&result_sel).take(num * 2) {
        let Some(a) = r.select(&title_sel).next() else { continue };
        let title = a.text().collect::<String>().trim().to_string();
        let url   = a.value().attr("href").unwrap_or("").to_string();
        if title.is_empty() || url.is_empty() { continue; }

        let snippet = r.select(&snippet_sel).next()
            .map(|e| e.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        results.push(SearchResult { title, url, snippet });
        if results.len() >= num { break; }
    }
    Ok(results)
}

// ─── Baidu ────────────────────────────────────────────────────────────────────

async fn baidu_search(client: Client, query: &str, num: usize) -> Result<Vec<SearchResult>> {
    let url = format!(
        "https://www.baidu.com/s?wd={}&rn={}",
        urlencoding::encode(query),
        num.min(10)
    );

    let html = client
        .get(&url)
        .header("Accept-Language", "zh-CN,zh;q=0.9")
        .header("Accept", "text/html,application/xhtml+xml")
        .send().await?.text().await?;

    let doc = Html::parse_document(&html);

    // Baidu result containers: div with tpl attribute or .result class
    let result_sel  = Selector::parse("div.result, div.c-container, .result-op").map_err(|e| anyhow::anyhow!("{:?}", e))?;
    let title_sel   = Selector::parse("h3 a, .t a").map_err(|e| anyhow::anyhow!("{:?}", e))?;
    let snippet_sel = Selector::parse(".c-abstract, .c-span9, .content-right_8Zs40").map_err(|e| anyhow::anyhow!("{:?}", e))?;
    let url_sel     = Selector::parse(".c-showurl, cite").map_err(|e| anyhow::anyhow!("{:?}", e))?;

    let mut results = Vec::new();
    for r in doc.select(&result_sel).take(num * 3) {
        let Some(a) = r.select(&title_sel).next() else { continue };
        let title = a.text().collect::<String>().trim().to_string();
        if title.is_empty() { continue; }

        // Baidu wraps real URLs in redirect; try to get the displayed URL first
        let displayed_url = r.select(&url_sel).next()
            .map(|e| e.text().collect::<String>().trim().to_string())
            .unwrap_or_default();
        let href = a.value().attr("href").unwrap_or("").to_string();
        let url = if displayed_url.starts_with("http") {
            displayed_url
        } else if href.starts_with("http") {
            href
        } else if !displayed_url.is_empty() {
            format!("https://{}", displayed_url)
        } else {
            continue;
        };

        let snippet = r.select(&snippet_sel).next()
            .map(|e| e.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        results.push(SearchResult { title, url, snippet });
        if results.len() >= num { break; }
    }
    Ok(results)
}

// ─── 360 Search (so.com) ─────────────────────────────────────────────────────

async fn so_search(client: Client, query: &str, num: usize) -> Result<Vec<SearchResult>> {
    let url = format!(
        "https://www.so.com/s?q={}&src=360chrome",
        urlencoding::encode(query)
    );

    let html = client
        .get(&url)
        .header("Accept-Language", "zh-CN,zh;q=0.9")
        .send().await?.text().await?;

    let doc = Html::parse_document(&html);
    let result_sel  = Selector::parse("li.res-list, .res-base").map_err(|e| anyhow::anyhow!("{:?}", e))?;
    let title_sel   = Selector::parse("h3 a").map_err(|e| anyhow::anyhow!("{:?}", e))?;
    let snippet_sel = Selector::parse(".res-desc, .res-comm-con").map_err(|e| anyhow::anyhow!("{:?}", e))?;

    let mut results = Vec::new();
    for r in doc.select(&result_sel).take(num * 2) {
        let Some(a) = r.select(&title_sel).next() else { continue };
        let title = a.text().collect::<String>().trim().to_string();
        let url   = a.value().attr("href").unwrap_or("").to_string();
        if title.is_empty() || url.is_empty() { continue; }

        let snippet = r.select(&snippet_sel).next()
            .map(|e| e.text().collect::<String>().trim().to_string())
            .unwrap_or_default();

        results.push(SearchResult { title, url, snippet });
        if results.len() >= num { break; }
    }
    Ok(results)
}

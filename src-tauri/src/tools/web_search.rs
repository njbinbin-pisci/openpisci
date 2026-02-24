use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};

pub struct WebSearchTool;

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str { "web_search" }

    fn description(&self) -> &str {
        "Search the web for information. Returns a list of results with titles, URLs, and snippets."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "num_results": {
                    "type": "integer",
                    "description": "Number of results to return (default 5, max 10)"
                }
            },
            "required": ["query"]
        })
    }

    fn is_read_only(&self) -> bool { true }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        let query = match input["query"].as_str() {
            Some(q) => q,
            None => return Ok(ToolResult::err("Missing required parameter: query")),
        };
        let num = input["num_results"].as_u64().unwrap_or(5).min(10) as usize;

        // Use DuckDuckGo Instant Answer API (no key required)
        let client = Client::new();
        let url = format!(
            "https://api.duckduckgo.com/?q={}&format=json&no_html=1&skip_disambig=1",
            urlencoding::encode(query)
        );

        match client.get(&url)
            .header("User-Agent", "Pisci-Desktop/0.1")
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                let val: Value = resp.json().await?;
                let mut results = Vec::new();

                // Abstract (direct answer)
                if let Some(abstract_text) = val["Abstract"].as_str() {
                    if !abstract_text.is_empty() {
                        results.push(format!(
                            "**{}**\n{}\nSource: {}",
                            val["Heading"].as_str().unwrap_or("Answer"),
                            abstract_text,
                            val["AbstractURL"].as_str().unwrap_or("")
                        ));
                    }
                }

                // Related topics
                if let Some(topics) = val["RelatedTopics"].as_array() {
                    for topic in topics.iter().take(num.saturating_sub(results.len())) {
                        if let (Some(text), Some(url)) = (
                            topic["Text"].as_str(),
                            topic["FirstURL"].as_str(),
                        ) {
                            results.push(format!("- {}\n  {}", text, url));
                        }
                    }
                }

                if results.is_empty() {
                    Ok(ToolResult::ok(format!(
                        "No results found for: {}\n\nTry a more specific query.",
                        query
                    )))
                } else {
                    Ok(ToolResult::ok(format!(
                        "Search results for: {}\n\n{}",
                        query,
                        results.join("\n\n")
                    )))
                }
            }
            Ok(resp) => Ok(ToolResult::err(format!(
                "Search API returned status: {}", resp.status()
            ))),
            Err(e) => Ok(ToolResult::err(format!("Search request failed: {}", e))),
        }
    }
}

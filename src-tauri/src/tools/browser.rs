/// Browser automation tool via Chrome DevTools Protocol (CDP).
/// Uses Chrome for Testing (auto-downloaded) or system Chrome.
use crate::agent::tool::{ImageData, Tool, ToolContext, ToolResult};
use crate::browser::SharedBrowserManager;
use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use chromiumoxide::cdp::browser_protocol::network::{CookieParam, DeleteCookiesParams};
use serde_json::{json, Value};
use std::time::Duration;
use uuid::Uuid;

const DEFAULT_TIMEOUT_MS: u64 = 15000;
const MAX_CONTENT_BYTES: usize = 50 * 1024; // 50 KB

pub struct BrowserTool {
    manager: SharedBrowserManager,
}

impl BrowserTool {
    pub fn new(manager: SharedBrowserManager) -> Self {
        Self { manager }
    }
}

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str { "browser" }

    fn description(&self) -> &str {
        "Control a Chrome browser via CDP. Navigate pages, click elements, type text, \
         take screenshots (returned to Vision AI), execute JavaScript, manage tabs, \
         and interact with web content. Chrome for Testing is auto-downloaded on first use."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "navigate", "go_back", "go_forward", "reload",
                        "click", "double_click", "right_click", "hover",
                        "type_text", "clear", "press_key",
                        "screenshot", "get_content", "get_text", "get_attribute",
                        "eval_js", "wait_for", "scroll",
                        "select", "check", "uncheck",
                        "list_tabs", "new_tab", "close_tab", "switch_tab",
                        "get_cookies", "set_cookie", "clear_cookies",
                        "get_url", "get_title", "detect_challenge",
                        "launch", "close"
                    ],
                    "description": "Action to perform"
                },
                "url": {
                    "type": "string",
                    "description": "URL for navigate action"
                },
                "selector": {
                    "type": "string",
                    "description": "CSS selector for element actions"
                },
                "text": {
                    "type": "string",
                    "description": "Text to type or option value to select"
                },
                "key": {
                    "type": "string",
                    "description": "Key name for press_key (e.g. 'Enter', 'Tab', 'Escape', 'ArrowDown')"
                },
                "js": {
                    "type": "string",
                    "description": "JavaScript code to execute (for eval_js)"
                },
                "attribute": {
                    "type": "string",
                    "description": "Attribute name to get (for get_attribute)"
                },
                "tab_id": {
                    "type": "string",
                    "description": "Tab identifier (default: active tab)"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Timeout in milliseconds (default: 15000)"
                },
                "full_page": {
                    "type": "boolean",
                    "description": "Capture full page screenshot (default: false)"
                },
                "wait_condition": {
                    "type": "string",
                    "enum": ["navigation", "element", "element_hidden", "network_idle", "human_verification"],
                    "description": "Condition to wait for"
                },
                "scroll_direction": {
                    "type": "string",
                    "enum": ["up", "down", "left", "right", "top", "bottom"],
                    "description": "Scroll direction"
                },
                "scroll_amount": {
                    "type": "integer",
                    "description": "Scroll amount in pixels (default: 300)"
                },
                "cookie_name": {
                    "type": "string",
                    "description": "Cookie name"
                },
                "cookie_value": {
                    "type": "string",
                    "description": "Cookie value"
                },
                "headless": {
                    "type": "boolean",
                    "description": "Launch in headless mode (for 'launch' action, default: true)"
                }
            },
            "required": ["action"]
        })
    }

    fn is_read_only(&self) -> bool { false }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        let action = match input["action"].as_str() {
            Some(a) => a,
            None => return Ok(ToolResult::err("Missing required parameter: action")),
        };

        match action {
            "launch"        => self.launch_browser(&input).await,
            "close"         => self.close_browser().await,
            "navigate"      => self.navigate(&input).await,
            "go_back"       => self.go_back(&input).await,
            "go_forward"    => self.go_forward(&input).await,
            "reload"        => self.reload(&input).await,
            "click"         => self.click(&input).await,
            "double_click"  => self.double_click(&input).await,
            "right_click"   => self.right_click(&input).await,
            "hover"         => self.hover(&input).await,
            "type_text"     => self.type_text(&input).await,
            "clear"         => self.clear(&input).await,
            "press_key"     => self.press_key(&input).await,
            "screenshot"    => self.screenshot(&input).await,
            "get_content"   => self.get_content(&input).await,
            "get_text"      => self.get_text(&input).await,
            "get_attribute" => self.get_attribute(&input).await,
            "eval_js"       => self.eval_js(&input).await,
            "wait_for"      => self.wait_for(&input).await,
            "scroll"        => self.scroll(&input).await,
            "select"        => self.select(&input).await,
            "check"         => self.set_checked(&input, true).await,
            "uncheck"       => self.set_checked(&input, false).await,
            "list_tabs"     => self.list_tabs().await,
            "new_tab"       => self.new_tab(&input).await,
            "close_tab"     => self.close_tab(&input).await,
            "switch_tab"    => self.switch_tab(&input).await,
            "get_cookies"   => self.get_cookies(&input).await,
            "set_cookie"    => self.set_cookie(&input).await,
            "clear_cookies" => self.clear_cookies(&input).await,
            "get_url"       => self.get_url(&input).await,
            "get_title"     => self.get_title(&input).await,
            "detect_challenge" => self.detect_challenge(&input).await,
            _ => Ok(ToolResult::err(format!("Unknown action: {}", action))),
        }
    }
}

impl BrowserTool {
    // ─── Browser lifecycle ────────────────────────────────────────────────────

    async fn launch_browser(&self, input: &Value) -> Result<ToolResult> {
        let requested_headless = input["headless"].as_bool();
        let mut mgr = self.manager.lock().await;
        if let Some(h) = requested_headless {
            let current = mgr.headless();
            if current != h {
                if mgr.is_running() {
                    mgr.close().await;
                }
                mgr.set_headless(h);
            }
        }
        if mgr.is_running() {
            return Ok(ToolResult::ok(format!(
                "Browser already running (headless={})",
                mgr.headless()
            )));
        }
        mgr.launch().await?;
        Ok(ToolResult::ok(format!(
            "Browser launched (headless={})",
            mgr.headless()
        )))
    }

    async fn close_browser(&self) -> Result<ToolResult> {
        let mut mgr = self.manager.lock().await;
        mgr.close().await;
        Ok(ToolResult::ok("Browser closed"))
    }

    // ─── Navigation ───────────────────────────────────────────────────────────

    async fn navigate(&self, input: &Value) -> Result<ToolResult> {
        let url = match input["url"].as_str() {
            Some(u) => u.to_string(),
            None => return Ok(ToolResult::err("navigate requires url")),
        };

        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        let _ = page.goto(&url).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        self.wait_until_navigation_ready(&page, DEFAULT_TIMEOUT_MS).await?;

        let title = page.evaluate("document.title").await
            .ok().and_then(|r| r.into_value::<String>().ok()).unwrap_or_default();
        let current_url = page.evaluate("window.location.href").await
            .ok().and_then(|r| r.into_value::<String>().ok()).unwrap_or_default();
        if let Some(reason) = self.detect_challenge_hint(&page).await? {
            return Ok(ToolResult::ok(format!(
                "Navigated to: {}\nTitle: {}\n\n检测到可能验证码/人机校验: {}\n请人工完成校验后，再调用 browser.wait_for(wait_condition='human_verification') 继续。",
                current_url, title, reason
            )));
        }
        Ok(ToolResult::ok(format!("Navigated to: {}\nTitle: {}", current_url, title)))
    }

    async fn go_back(&self, _input: &Value) -> Result<ToolResult> {
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);
        page.evaluate("history.back()").await.map_err(|e| anyhow::anyhow!("{}", e))?;
        let _ = self.wait_until_navigation_ready(&page, DEFAULT_TIMEOUT_MS).await;
        Ok(ToolResult::ok("Navigated back"))
    }

    async fn go_forward(&self, _input: &Value) -> Result<ToolResult> {
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);
        page.evaluate("history.forward()").await.map_err(|e| anyhow::anyhow!("{}", e))?;
        let _ = self.wait_until_navigation_ready(&page, DEFAULT_TIMEOUT_MS).await;
        Ok(ToolResult::ok("Navigated forward"))
    }

    async fn reload(&self, _input: &Value) -> Result<ToolResult> {
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);
        page.reload().await.map_err(|e| anyhow::anyhow!("{}", e))?;
        let _ = self.wait_until_navigation_ready(&page, DEFAULT_TIMEOUT_MS).await;
        Ok(ToolResult::ok("Page reloaded"))
    }

    // ─── Element interaction ──────────────────────────────────────────────────

    async fn click(&self, input: &Value) -> Result<ToolResult> {
        let selector = self.require_selector(input)?;
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        let element = page.find_element(&selector).await
            .map_err(|e| anyhow::anyhow!("Element '{}' not found: {}", selector, e))?;
        element.click().await.map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(ToolResult::ok(format!("Clicked: {}", selector)))
    }

    async fn double_click(&self, input: &Value) -> Result<ToolResult> {
        let selector = self.require_selector(input)?;
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        let js = format!(
            r#"
            const el = document.querySelector({});
            if (!el) throw new Error('Element not found');
            el.dispatchEvent(new MouseEvent('dblclick', {{bubbles: true}}));
            "#,
            serde_json::to_string(&selector).unwrap()
        );
        page.evaluate(js).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(ToolResult::ok(format!("Double-clicked: {}", selector)))
    }

    async fn right_click(&self, input: &Value) -> Result<ToolResult> {
        let selector = self.require_selector(input)?;
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        let js = format!(
            r#"
            const el = document.querySelector({});
            if (!el) throw new Error('Element not found');
            el.dispatchEvent(new MouseEvent('contextmenu', {{bubbles: true}}));
            "#,
            serde_json::to_string(&selector).unwrap()
        );
        page.evaluate(js).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(ToolResult::ok(format!("Right-clicked: {}", selector)))
    }

    async fn hover(&self, input: &Value) -> Result<ToolResult> {
        let selector = self.require_selector(input)?;
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        let js = format!(
            r#"
            const el = document.querySelector({});
            if (!el) throw new Error('Element not found');
            el.dispatchEvent(new MouseEvent('mouseover', {{bubbles: true}}));
            el.dispatchEvent(new MouseEvent('mouseenter', {{bubbles: true}}));
            "#,
            serde_json::to_string(&selector).unwrap()
        );
        page.evaluate(js).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(ToolResult::ok(format!("Hovered: {}", selector)))
    }

    async fn type_text(&self, input: &Value) -> Result<ToolResult> {
        let selector = self.require_selector(input)?;
        let text = match input["text"].as_str() {
            Some(t) => t.to_string(),
            None => return Ok(ToolResult::err("type_text requires text")),
        };

        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        let element = page.find_element(&selector).await
            .map_err(|e| anyhow::anyhow!("Element '{}' not found: {}", selector, e))?;
        element.click().await.map_err(|e| anyhow::anyhow!("{}", e))?;
        // Set value via JS for reliability
        let js = format!(
            r#"
            const el = document.querySelector({});
            if (el) {{
                el.value = {};
                el.dispatchEvent(new Event('input', {{bubbles: true}}));
                el.dispatchEvent(new Event('change', {{bubbles: true}}));
            }}
            "#,
            serde_json::to_string(&selector).unwrap(),
            serde_json::to_string(&text).unwrap()
        );
        page.evaluate(js).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(ToolResult::ok(format!("Typed into {}: {}", selector, &text[..text.len().min(50)])))
    }

    async fn clear(&self, input: &Value) -> Result<ToolResult> {
        let selector = self.require_selector(input)?;
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        let js = format!(
            r#"
            const el = document.querySelector({});
            if (!el) throw new Error('Element not found');
            el.value = '';
            el.dispatchEvent(new Event('input', {{bubbles: true}}));
            "#,
            serde_json::to_string(&selector).unwrap()
        );
        page.evaluate(js).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(ToolResult::ok(format!("Cleared: {}", selector)))
    }

    async fn press_key(&self, input: &Value) -> Result<ToolResult> {
        let key = match input["key"].as_str() {
            Some(k) => k.to_string(),
            None => return Ok(ToolResult::err("press_key requires key (e.g. 'Enter', 'Tab')")),
        };

        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        // If selector provided, focus element first
        if let Some(selector) = input["selector"].as_str() {
            if let Ok(element) = page.find_element(selector).await {
                let _ = element.click().await;
            }
        }

        // Use keyboard API
        page.evaluate(format!(
            "document.dispatchEvent(new KeyboardEvent('keydown', {{key: '{}', bubbles: true}})); \
             document.dispatchEvent(new KeyboardEvent('keyup', {{key: '{}', bubbles: true}}))",
            key.replace('\'', "\\'"), key.replace('\'', "\\'")
        )).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(ToolResult::ok(format!("Pressed key: {}", key)))
    }

    // ─── Screenshot ───────────────────────────────────────────────────────────

    async fn screenshot(&self, input: &Value) -> Result<ToolResult> {
        let full_page = input["full_page"].as_bool().unwrap_or(false);
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        // Use CDP screenshot command directly
        let params = chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotParams {
            format: Some(chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat::Jpeg),
            quality: Some(75),
            capture_beyond_viewport: Some(full_page),
            ..Default::default()
        };

        let png_bytes = page.screenshot(params)
            .await.map_err(|e| anyhow::anyhow!("Screenshot failed: {}", e))?;

        let b64 = base64::engine::general_purpose::STANDARD.encode(&png_bytes);
        let size_kb = png_bytes.len() / 1024;

        let url = page.evaluate("window.location.href").await
            .ok().and_then(|r| r.into_value::<String>().ok()).unwrap_or_default();
        let title = page.evaluate("document.title").await
            .ok().and_then(|r| r.into_value::<String>().ok()).unwrap_or_default();

        Ok(ToolResult::ok(format!(
            "Screenshot captured: {} KB\nURL: {}\nTitle: {}\nFull page: {}",
            size_kb, url, title, full_page
        )).with_image(ImageData::jpeg(b64)))
    }

    // ─── Content extraction ───────────────────────────────────────────────────

    async fn get_content(&self, input: &Value) -> Result<ToolResult> {
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        let result = page.evaluate("document.documentElement.outerHTML")
            .await.map_err(|e| anyhow::anyhow!("{}", e))?;
        let content = result.into_value::<String>().unwrap_or_default();

        // Truncate large pages
        let truncated = if content.len() > MAX_CONTENT_BYTES {
            format!(
                "{}\n\n... [{} bytes truncated] ...",
                &content[..MAX_CONTENT_BYTES],
                content.len() - MAX_CONTENT_BYTES
            )
        } else {
            content
        };

        Ok(ToolResult::ok(truncated))
    }

    async fn get_text(&self, input: &Value) -> Result<ToolResult> {
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        if let Some(selector) = input["selector"].as_str() {
            let js = format!(
                r#"
                const el = document.querySelector({});
                el ? el.textContent.trim() : null
                "#,
                serde_json::to_string(selector).unwrap()
            );
            let result = page.evaluate(js).await.map_err(|e| anyhow::anyhow!("{}", e))?;
            let text = result.into_value::<Option<String>>()
                .unwrap_or(None)
                .unwrap_or_else(|| "Element not found".to_string());
            Ok(ToolResult::ok(text))
        } else {
            // Get all visible text
            let js = "document.body.innerText";
            let result = page.evaluate(js).await.map_err(|e| anyhow::anyhow!("{}", e))?;
            let text = result.into_value::<String>().unwrap_or_default();
            let truncated = if text.len() > MAX_CONTENT_BYTES {
                format!("{}\n\n... [{} bytes truncated]", &text[..MAX_CONTENT_BYTES], text.len() - MAX_CONTENT_BYTES)
            } else {
                text
            };
            Ok(ToolResult::ok(truncated))
        }
    }

    async fn get_attribute(&self, input: &Value) -> Result<ToolResult> {
        let selector = self.require_selector(input)?;
        let attr = match input["attribute"].as_str() {
            Some(a) => a.to_string(),
            None => return Ok(ToolResult::err("get_attribute requires attribute")),
        };

        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        let js = format!(
            r#"
            const el = document.querySelector({});
            el ? el.getAttribute({}) : null
            "#,
            serde_json::to_string(&selector).unwrap(),
            serde_json::to_string(&attr).unwrap()
        );
        let result = page.evaluate(js).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        let value = result.into_value::<Option<String>>()
            .unwrap_or(None)
            .unwrap_or_else(|| "null".to_string());
        Ok(ToolResult::ok(format!("{}[{}] = {}", selector, attr, value)))
    }

    // ─── JavaScript execution ─────────────────────────────────────────────────

    async fn eval_js(&self, input: &Value) -> Result<ToolResult> {
        let js = match input["js"].as_str() {
            Some(j) => j.to_string(),
            None => return Ok(ToolResult::err("eval_js requires js")),
        };

        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        let result = page.evaluate(js).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        let json_val = result.into_value::<serde_json::Value>()
            .unwrap_or(Value::String("(non-serializable result)".into()));
        Ok(ToolResult::ok(serde_json::to_string_pretty(&json_val).unwrap_or_default()))
    }

    // ─── Wait ─────────────────────────────────────────────────────────────────

    async fn wait_for(&self, input: &Value) -> Result<ToolResult> {
        let condition = input["wait_condition"].as_str().unwrap_or("navigation");
        let timeout_ms = input["timeout_ms"].as_u64().unwrap_or(DEFAULT_TIMEOUT_MS);

        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        match condition {
            "navigation" => {
                self.wait_until_navigation_ready(&page, timeout_ms).await?;
                Ok(ToolResult::ok("Navigation complete"))
            }
            "element" => {
                let selector = self.require_selector(input)?;
                let start = std::time::Instant::now();
                loop {
                    if page.find_element(&selector).await.is_ok() {
                        return Ok(ToolResult::ok(format!("Element found: {}", selector)));
                    }
                    if start.elapsed().as_millis() as u64 >= timeout_ms {
                        return Ok(ToolResult::err(format!("Timeout: element '{}' not found", selector)));
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
            "element_hidden" => {
                let selector = self.require_selector(input)?;
                let start = std::time::Instant::now();
                loop {
                    if page.find_element(&selector).await.is_err() {
                        return Ok(ToolResult::ok(format!("Element hidden: {}", selector)));
                    }
                    if start.elapsed().as_millis() as u64 >= timeout_ms {
                        return Ok(ToolResult::err(format!("Timeout: element '{}' still visible", selector)));
                    }
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
            }
            "network_idle" => {
                self.wait_until_network_idle(&page, timeout_ms).await?;
                Ok(ToolResult::ok("Network idle reached"))
            }
            "human_verification" => {
                let cleared = self.wait_until_no_challenge(&page, timeout_ms).await?;
                if cleared {
                    Ok(ToolResult::ok("Human verification cleared, automation can continue"))
                } else {
                    Ok(ToolResult::err("Timeout waiting for human verification to be cleared"))
                }
            }
            _ => Ok(ToolResult::err(format!("Unknown wait_condition: {}", condition))),
        }
    }

    // ─── Scroll ───────────────────────────────────────────────────────────────

    async fn scroll(&self, input: &Value) -> Result<ToolResult> {
        let direction = input["scroll_direction"].as_str().unwrap_or("down");
        let amount = input["scroll_amount"].as_i64().unwrap_or(300);

        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        let js = match direction {
            "up"     => format!("window.scrollBy(0, -{})", amount),
            "down"   => format!("window.scrollBy(0, {})", amount),
            "left"   => format!("window.scrollBy(-{}, 0)", amount),
            "right"  => format!("window.scrollBy({}, 0)", amount),
            "top"    => "window.scrollTo(0, 0)".to_string(),
            "bottom" => "window.scrollTo(0, document.body.scrollHeight)".to_string(),
            _        => format!("window.scrollBy(0, {})", amount),
        };

        page.evaluate(js).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(ToolResult::ok(format!("Scrolled {} by {} px", direction, amount)))
    }

    // ─── Form controls ────────────────────────────────────────────────────────

    async fn select(&self, input: &Value) -> Result<ToolResult> {
        let selector = self.require_selector(input)?;
        let value = match input["text"].as_str() {
            Some(v) => v.to_string(),
            None => return Ok(ToolResult::err("select requires text (option value or label)")),
        };

        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        let js = format!(
            r#"
            const sel = document.querySelector({});
            if (!sel) throw new Error('Select element not found');
            const opts = Array.from(sel.options);
            const opt = opts.find(o => o.value === {} || o.text === {});
            if (opt) {{
                sel.value = opt.value;
                sel.dispatchEvent(new Event('change', {{bubbles: true}}));
                opt.value;
            }} else {{
                throw new Error('Option not found: ' + {});
            }}
            "#,
            serde_json::to_string(&selector).unwrap(),
            serde_json::to_string(&value).unwrap(),
            serde_json::to_string(&value).unwrap(),
            serde_json::to_string(&value).unwrap(),
        );
        let result = page.evaluate(js).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        let selected = result.into_value::<String>().unwrap_or_default();
        Ok(ToolResult::ok(format!("Selected '{}' in {}", selected, selector)))
    }

    async fn set_checked(&self, input: &Value, checked: bool) -> Result<ToolResult> {
        let selector = self.require_selector(input)?;
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        let js = format!(
            r#"
            const el = document.querySelector({});
            if (!el) throw new Error('Element not found');
            if (el.checked !== {}) {{
                el.click();
            }}
            el.checked
            "#,
            serde_json::to_string(&selector).unwrap(),
            checked
        );
        page.evaluate(js).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        let action = if checked { "Checked" } else { "Unchecked" };
        Ok(ToolResult::ok(format!("{}: {}", action, selector)))
    }

    // ─── Tab management ───────────────────────────────────────────────────────

    async fn list_tabs(&self) -> Result<ToolResult> {
        let mgr = self.manager.lock().await;
        let tabs = mgr.list_tabs();
        let active = mgr.active_tab.clone().unwrap_or_default();
        drop(mgr);

        if tabs.is_empty() {
            return Ok(ToolResult::ok("No open tabs"));
        }
        let list: Vec<String> = tabs.iter().map(|t| {
            if t == &active { format!("* {} (active)", t) } else { t.clone() }
        }).collect();
        Ok(ToolResult::ok(format!("Open tabs:\n{}", list.join("\n"))))
    }

    async fn new_tab(&self, input: &Value) -> Result<ToolResult> {
        let tab_id = input["tab_id"].as_str()
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("tab_{}", Uuid::new_v4().simple()))
            .to_string();
        let url = input["url"].as_str().unwrap_or("about:blank").to_string();

        let mut mgr = self.manager.lock().await;
        let page = mgr.create_page(&tab_id).await?;
        drop(mgr);

        if url != "about:blank" {
            let _ = page.goto(&url).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        Ok(ToolResult::ok(format!("New tab created: {}", tab_id)))
    }

    async fn close_tab(&self, input: &Value) -> Result<ToolResult> {
        let tab_id = match input["tab_id"].as_str() {
            Some(t) => t.to_string(),
            None => return Ok(ToolResult::err("close_tab requires tab_id")),
        };
        let mut mgr = self.manager.lock().await;
        mgr.close_tab(&tab_id).await?;
        Ok(ToolResult::ok(format!("Closed tab: {}", tab_id)))
    }

    async fn switch_tab(&self, input: &Value) -> Result<ToolResult> {
        let tab_id = match input["tab_id"].as_str() {
            Some(t) => t.to_string(),
            None => return Ok(ToolResult::err("switch_tab requires tab_id")),
        };
        let mut mgr = self.manager.lock().await;
        mgr.switch_tab(&tab_id)?;
        Ok(ToolResult::ok(format!("Switched to tab: {}", tab_id)))
    }

    // ─── Cookies ─────────────────────────────────────────────────────────────

    async fn get_cookies(&self, _input: &Value) -> Result<ToolResult> {
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        let cookies = page.get_cookies().await.map_err(|e| anyhow::anyhow!("{}", e))?;
        if cookies.is_empty() {
            return Ok(ToolResult::ok("No cookies"));
        }
        Ok(ToolResult::ok(
            serde_json::to_string_pretty(&cookies).unwrap_or_else(|_| format!("{:#?}", cookies))
        ))
    }

    async fn set_cookie(&self, input: &Value) -> Result<ToolResult> {
        let name = match input["cookie_name"].as_str() {
            Some(n) => n.to_string(),
            None => return Ok(ToolResult::err("set_cookie requires cookie_name")),
        };
        let value = input["cookie_value"].as_str().unwrap_or("").to_string();

        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);

        let current_url = page
            .url()
            .await
            .map_err(|e| anyhow::anyhow!("{}", e))?
            .unwrap_or_default();
        let cookie = CookieParam {
            name: name.clone(),
            value,
            url: if current_url.is_empty() { None } else { Some(current_url) },
            domain: None,
            path: Some("/".to_string()),
            secure: None,
            http_only: None,
            same_site: None,
            expires: None,
            priority: None,
            same_party: None,
            source_scheme: None,
            source_port: None,
            partition_key: None,
        };
        page.set_cookie(cookie).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(ToolResult::ok(format!("Cookie set: {}", name)))
    }

    async fn clear_cookies(&self, _input: &Value) -> Result<ToolResult> {
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);
        let cookies = page.get_cookies().await.map_err(|e| anyhow::anyhow!("{}", e))?;
        if cookies.is_empty() {
            return Ok(ToolResult::ok("No cookies to clear"));
        }
        let deletes: Vec<DeleteCookiesParams> = cookies
            .into_iter()
            .map(|c| DeleteCookiesParams {
                name: c.name,
                url: None,
                domain: Some(c.domain),
                path: Some(c.path),
                partition_key: None,
            })
            .collect();
        page.delete_cookies(deletes).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(ToolResult::ok("Cookies cleared via CDP"))
    }

    // ─── Page info ────────────────────────────────────────────────────────────

    async fn get_url(&self, _input: &Value) -> Result<ToolResult> {
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);
        let url = page.evaluate("window.location.href")
            .await.map_err(|e| anyhow::anyhow!("{}", e))?
            .into_value::<String>().unwrap_or_default();
        Ok(ToolResult::ok(url))
    }

    async fn get_title(&self, _input: &Value) -> Result<ToolResult> {
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);
        let title = page.evaluate("document.title")
            .await.map_err(|e| anyhow::anyhow!("{}", e))?
            .into_value::<String>().unwrap_or_default();
        Ok(ToolResult::ok(title))
    }

    async fn detect_challenge(&self, _input: &Value) -> Result<ToolResult> {
        let mut mgr = self.manager.lock().await;
        let page = mgr.active_page().await?;
        drop(mgr);
        match self.detect_challenge_hint(&page).await? {
            Some(reason) => Ok(ToolResult::ok(format!(
                "Detected possible human verification: {}\n请人工完成后调用 browser.wait_for(wait_condition='human_verification').",
                reason
            ))),
            None => Ok(ToolResult::ok("No obvious challenge detected")),
        }
    }

    // ─── Helpers ─────────────────────────────────────────────────────────────

    fn require_selector(&self, input: &Value) -> Result<String> {
        match input["selector"].as_str() {
            Some(s) => Ok(s.to_string()),
            None => Err(anyhow::anyhow!("This action requires a 'selector' parameter (CSS selector)")),
        }
    }

    async fn wait_until_navigation_ready(
        &self,
        page: &chromiumoxide::Page,
        timeout_ms: u64,
    ) -> Result<()> {
        let start = std::time::Instant::now();
        loop {
            let state = page
                .evaluate("document.readyState")
                .await
                .ok()
                .and_then(|v| v.into_value::<String>().ok())
                .unwrap_or_default();
            if state == "complete" || state == "interactive" {
                return Ok(());
            }
            if start.elapsed().as_millis() as u64 >= timeout_ms {
                return Err(anyhow::anyhow!("Timeout waiting for navigation readiness"));
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    }

    async fn wait_until_network_idle(
        &self,
        page: &chromiumoxide::Page,
        timeout_ms: u64,
    ) -> Result<()> {
        let start = std::time::Instant::now();
        let mut stable_rounds = 0u8;
        let mut last_count = -1i64;
        while start.elapsed().as_millis() as u64 < timeout_ms {
            let count = page
                .evaluate("performance.getEntriesByType('resource').length")
                .await
                .ok()
                .and_then(|v| v.into_value::<i64>().ok())
                .unwrap_or(-1);
            let ready = page
                .evaluate("document.readyState")
                .await
                .ok()
                .and_then(|v| v.into_value::<String>().ok())
                .unwrap_or_default();
            if count == last_count && (ready == "complete" || ready == "interactive") {
                stable_rounds += 1;
            } else {
                stable_rounds = 0;
            }
            if stable_rounds >= 3 {
                return Ok(());
            }
            last_count = count;
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
        Err(anyhow::anyhow!("Timeout waiting for network idle"))
    }

    async fn detect_challenge_hint(
        &self,
        page: &chromiumoxide::Page,
    ) -> Result<Option<String>> {
        let js = r#"
(() => {
  const text = (document.body?.innerText || '').toLowerCase();
  const title = (document.title || '').toLowerCase();
  const markers = [
    'captcha', 'recaptcha', 'hcaptcha',
    'verify you are human', 'verification required',
    'robot check', 'security check'
  ];
  const hasText = markers.some(k => text.includes(k) || title.includes(k));
  const hasIframe = !!document.querySelector("iframe[src*='captcha'], iframe[src*='recaptcha'], iframe[src*='hcaptcha']");
  const hasChallengeWidget = !!document.querySelector("[id*='captcha'], [class*='captcha'], .g-recaptcha, [data-sitekey]");
  if (hasText || hasIframe || hasChallengeWidget) {
    return {
      hasChallenge: true,
      reason: `text=${hasText}, iframe=${hasIframe}, widget=${hasChallengeWidget}`
    };
  }
  return { hasChallenge: false, reason: '' };
})()
"#;
        let obj = page.evaluate(js).await.map_err(|e| anyhow::anyhow!("{}", e))?;
        let val = obj.into_value::<Value>().unwrap_or(Value::Null);
        if val["hasChallenge"].as_bool().unwrap_or(false) {
            return Ok(Some(
                val["reason"]
                    .as_str()
                    .unwrap_or("captcha-like signals found")
                    .to_string(),
            ));
        }
        Ok(None)
    }

    async fn wait_until_no_challenge(
        &self,
        page: &chromiumoxide::Page,
        timeout_ms: u64,
    ) -> Result<bool> {
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() as u64 < timeout_ms {
            if self.detect_challenge_hint(page).await?.is_none() {
                return Ok(true);
            }
            tokio::time::sleep(Duration::from_millis(800)).await;
        }
        Ok(false)
    }
}

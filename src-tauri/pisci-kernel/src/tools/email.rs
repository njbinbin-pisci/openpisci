use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use lettre::message::{header::ContentType, Mailbox, MultiPart, SinglePart};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use serde_json::{json, Value};

pub struct EmailTool;

#[async_trait]
impl Tool for EmailTool {
    fn name(&self) -> &str {
        "email"
    }

    fn description(&self) -> &str {
        "Send and read emails. SMTP/IMAP credentials are taken from Settings — \
         do NOT pass passwords in tool input. \
         Actions: smtp_send (to, subject, body[, html_body]), \
         imap_fetch (folder, max_items), imap_search (query, folder, max_items)."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["smtp_send", "imap_fetch", "imap_search"],
                    "description": "Email action to perform"
                },
                // smtp_send params
                "to": {
                    "type": "string",
                    "description": "Recipient address, e.g. alice@example.com"
                },
                "subject": { "type": "string" },
                "body": {
                    "type": "string",
                    "description": "Plain-text body"
                },
                "html_body": {
                    "type": "string",
                    "description": "Optional HTML body (shown instead of plain-text in HTML-capable clients)"
                },
                // imap params
                "folder": {
                    "type": "string",
                    "description": "IMAP folder name (default: INBOX)"
                },
                "query": {
                    "type": "string",
                    "description": "IMAP search query (RFC 3501 syntax, e.g. UNSEEN, SUBJECT \"invoice\", FROM boss@company.com)"
                },
                "max_items": {
                    "type": "integer",
                    "description": "Maximum number of messages to return (default 10)"
                }
            },
            "required": ["action"]
        })
    }

    fn needs_confirmation(&self, input: &Value) -> bool {
        // Sending email always requires user confirmation
        input["action"].as_str() == Some("smtp_send")
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let s = &ctx.settings;

        if !s.email_enabled {
            return Ok(ToolResult::err(
                "Email tool is disabled. Enable it in Settings → Email.",
            ));
        }
        if s.smtp_host.is_empty() {
            return Ok(ToolResult::err(
                "SMTP not configured. Add smtp_host and credentials in Settings → Email.",
            ));
        }

        let action = input["action"].as_str().unwrap_or_default();
        match action {
            "smtp_send" => self.smtp_send(&input, ctx).await,
            "imap_fetch" => self.imap_fetch(&input, ctx).await,
            "imap_search" => self.imap_search(&input, ctx).await,
            _ => Ok(ToolResult::err(format!("Unknown email action: {}", action))),
        }
    }
}

impl EmailTool {
    fn header_value(headers: &[mailparse::MailHeader<'_>], key: &str) -> Option<String> {
        headers
            .iter()
            .find(|h| h.get_key_ref().eq_ignore_ascii_case(key))
            .map(|h| h.get_value())
    }

    async fn smtp_send(&self, input: &Value, ctx: &ToolContext) -> Result<ToolResult> {
        let s = &ctx.settings;

        let to = match input["to"].as_str() {
            Some(v) => v.to_string(),
            None => return Ok(ToolResult::err("smtp_send requires 'to'")),
        };
        let subject = input["subject"].as_str().unwrap_or("(no subject)");
        let body = input["body"].as_str().unwrap_or("");
        let html_body = input["html_body"].as_str().unwrap_or("");

        // Build From header: "Display Name <user@host>" or just "user@host"
        let from_str = if s.smtp_from_name.is_empty() {
            s.smtp_username.clone()
        } else {
            format!("{} <{}>", s.smtp_from_name, s.smtp_username)
        };

        let from_mb: Mailbox = from_str.parse()?;
        let to_mb: Mailbox = to.parse()?;

        let multipart = if !html_body.is_empty() {
            MultiPart::alternative()
                .singlepart(SinglePart::plain(body.to_string()))
                .singlepart(
                    SinglePart::builder()
                        .header(ContentType::TEXT_HTML)
                        .body(html_body.to_string()),
                )
        } else {
            MultiPart::mixed().singlepart(SinglePart::plain(body.to_string()))
        };

        let email = Message::builder()
            .from(from_mb)
            .to(to_mb)
            .subject(subject)
            .multipart(multipart)?;

        let creds = lettre::transport::smtp::authentication::Credentials::new(
            s.smtp_username.clone(),
            s.smtp_password.clone(),
        );
        let mailer = AsyncSmtpTransport::<Tokio1Executor>::relay(&s.smtp_host)?
            .port(s.smtp_port)
            .credentials(creds)
            .build();
        mailer.send(email).await?;
        Ok(ToolResult::ok(format!("Email sent to {}", to)))
    }

    async fn imap_fetch(&self, input: &Value, ctx: &ToolContext) -> Result<ToolResult> {
        let s = ctx.settings.clone();
        let imap_host = if s.imap_host.is_empty() {
            return Ok(ToolResult::err(
                "IMAP host not configured in Settings → Email.",
            ));
        } else {
            s.imap_host.clone()
        };
        let imap_port = s.imap_port;
        let username = s.smtp_username.clone(); // shared account
        let password = s.smtp_password.clone();
        let folder = input["folder"].as_str().unwrap_or("INBOX").to_string();
        let max_items = input["max_items"].as_u64().unwrap_or(10) as usize;

        let rows = tokio::task::spawn_blocking(move || -> Result<Vec<Value>> {
            let tls = native_tls::TlsConnector::builder().build()?;
            let client = imap::connect((imap_host.as_str(), imap_port), &imap_host, &tls)?;
            let mut session = client.login(username, password).map_err(|e| e.0)?;
            session.select(&folder)?;
            let msgs = session.fetch("1:*", "RFC822.HEADER")?;
            let mut out = Vec::new();
            for msg in msgs.iter().rev().take(max_items) {
                let mut subject = String::new();
                let mut from = String::new();
                let mut date = String::new();
                if let Some(header) = msg.header() {
                    let (headers, _) = mailparse::parse_headers(header)?;
                    if let Some(v) = Self::header_value(&headers, "Subject") {
                        subject = v;
                    }
                    if let Some(v) = Self::header_value(&headers, "From") {
                        from = v;
                    }
                    if let Some(v) = Self::header_value(&headers, "Date") {
                        date = v;
                    }
                }
                out.push(
                    json!({ "seq": msg.message, "subject": subject, "from": from, "date": date }),
                );
            }
            let _ = session.logout();
            Ok(out)
        })
        .await??;

        Ok(ToolResult::ok(
            serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string()),
        ))
    }

    async fn imap_search(&self, input: &Value, ctx: &ToolContext) -> Result<ToolResult> {
        let s = ctx.settings.clone();
        let imap_host = if s.imap_host.is_empty() {
            return Ok(ToolResult::err(
                "IMAP host not configured in Settings → Email.",
            ));
        } else {
            s.imap_host.clone()
        };
        let imap_port = s.imap_port;
        let username = s.smtp_username.clone();
        let password = s.smtp_password.clone();
        let folder = input["folder"].as_str().unwrap_or("INBOX").to_string();
        let query = input["query"].as_str().unwrap_or("ALL").to_string();
        let max_items = input["max_items"].as_u64().unwrap_or(10) as usize;

        let rows = tokio::task::spawn_blocking(move || -> Result<Vec<Value>> {
            let tls = native_tls::TlsConnector::builder().build()?;
            let client = imap::connect((imap_host.as_str(), imap_port), &imap_host, &tls)?;
            let mut session = client.login(username, password).map_err(|e| e.0)?;
            session.select(&folder)?;
            let ids = session.search(query)?;
            let mut out = Vec::new();
            let mut sorted_ids = ids.into_iter().collect::<Vec<u32>>();
            sorted_ids.sort_unstable_by(|a, b| b.cmp(a));
            for id in sorted_ids.into_iter().take(max_items) {
                let msgs = session.fetch(format!("{}", id), "RFC822.HEADER")?;
                for msg in msgs.iter() {
                    let mut subject = String::new();
                    let mut from = String::new();
                    if let Some(header) = msg.header() {
                        let (headers, _) = mailparse::parse_headers(header)?;
                        if let Some(v) = Self::header_value(&headers, "Subject") {
                            subject = v;
                        }
                        if let Some(v) = Self::header_value(&headers, "From") {
                            from = v;
                        }
                    }
                    out.push(json!({ "seq": msg.message, "subject": subject, "from": from }));
                }
            }
            let _ = session.logout();
            Ok(out)
        })
        .await??;

        Ok(ToolResult::ok(
            serde_json::to_string_pretty(&rows).unwrap_or_else(|_| "[]".to_string()),
        ))
    }
}

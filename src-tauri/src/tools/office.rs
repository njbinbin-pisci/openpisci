/// Office COM automation tool (Windows only).
/// Controls Word, Excel, and Outlook via PowerShell COM interop.
/// Uses PowerShell as the COM bridge to avoid complex Rust COM bindings.
use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

const OFFICE_TIMEOUT_SECS: u64 = 60;

pub struct OfficeTool;

#[async_trait]
impl Tool for OfficeTool {
    fn name(&self) -> &str { "office" }

    fn description(&self) -> &str {
        "Automate Microsoft Office applications (Word, Excel, Outlook) via COM. \
         Read/write Excel cells, create/edit Word documents, send/read Outlook emails. \
         Requires Office to be installed."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "app": {
                    "type": "string",
                    "enum": ["excel", "word", "outlook"],
                    "description": "Office application to control"
                },
                "action": {
                    "type": "string",
                    "enum": [
                        "open", "close", "save", "save_as",
                        "read_cell", "write_cell", "read_range", "write_range",
                        "get_sheet_names", "add_sheet", "get_cell_formula",
                        "read_document", "write_document", "append_text",
                        "read_emails", "send_email", "get_calendar"
                    ],
                    "description": "Action to perform"
                },
                "path": {
                    "type": "string",
                    "description": "File path to open/save"
                },
                "sheet": {
                    "type": "string",
                    "description": "Sheet name (for Excel, default: active sheet)"
                },
                "cell": {
                    "type": "string",
                    "description": "Cell reference (e.g. 'A1', 'B2') for Excel"
                },
                "range": {
                    "type": "string",
                    "description": "Cell range (e.g. 'A1:C10') for Excel"
                },
                "value": {
                    "type": "string",
                    "description": "Value to write"
                },
                "text": {
                    "type": "string",
                    "description": "Text content for Word document"
                },
                "to": {
                    "type": "string",
                    "description": "Email recipient(s) for Outlook"
                },
                "subject": {
                    "type": "string",
                    "description": "Email subject"
                },
                "body": {
                    "type": "string",
                    "description": "Email body"
                },
                "max_items": {
                    "type": "integer",
                    "description": "Maximum items to return (default: 10)"
                }
            },
            "required": ["app", "action"]
        })
    }

    fn needs_confirmation(&self, input: &Value) -> bool {
        matches!(
            input["action"].as_str(),
            Some("write_cell") | Some("write_range") | Some("write_document") |
            Some("append_text") | Some("send_email") | Some("save") | Some("save_as")
        )
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let app = match input["app"].as_str() {
            Some(a) => a,
            None => return Ok(ToolResult::err("Missing required parameter: app")),
        };
        let action = match input["action"].as_str() {
            Some(a) => a,
            None => return Ok(ToolResult::err("Missing required parameter: action")),
        };

        let ps_script = match (app, action) {
            // ── Excel ──────────────────────────────────────────────────────
            ("excel", "open") => {
                let path = input["path"].as_str().unwrap_or("");
                format!(r#"
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $true
$wb = $excel.Workbooks.Open("{}")
"Opened: " + $wb.Name
"#, path.replace('"', r#"\""#))
            }
            ("excel", "read_cell") => {
                let path = input["path"].as_str().unwrap_or("");
                let sheet = input["sheet"].as_str().unwrap_or("");
                let cell = input["cell"].as_str().unwrap_or("A1");
                format!(r#"
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $false
$wb = $excel.Workbooks.Open("{}")
$ws = if ("{}" -eq "") {{ $wb.ActiveSheet }} else {{ $wb.Sheets["{}"] }}
$val = $ws.Range("{}").Value2
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
"Cell {}: " + $val
"#, path.replace('"', r#"\""#), sheet, sheet, cell, cell)
            }
            ("excel", "write_cell") => {
                let path = input["path"].as_str().unwrap_or("");
                let sheet = input["sheet"].as_str().unwrap_or("");
                let cell = input["cell"].as_str().unwrap_or("A1");
                let value = input["value"].as_str().unwrap_or("");
                format!(r#"
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $false
$wb = $excel.Workbooks.Open("{}")
$ws = if ("{}" -eq "") {{ $wb.ActiveSheet }} else {{ $wb.Sheets["{}"] }}
$ws.Range("{}").Value2 = "{}"
$wb.Save()
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
"Written '{}' to cell {}"
"#, path.replace('"', r#"\""#), sheet, sheet, cell, value.replace('"', r#"\""#), value, cell)
            }
            ("excel", "read_range") => {
                let path = input["path"].as_str().unwrap_or("");
                let sheet = input["sheet"].as_str().unwrap_or("");
                let range = input["range"].as_str().unwrap_or("A1:A10");
                format!(r#"
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $false
$wb = $excel.Workbooks.Open("{}")
$ws = if ("{}" -eq "") {{ $wb.ActiveSheet }} else {{ $wb.Sheets["{}"] }}
$data = $ws.Range("{}").Value2
$result = @()
if ($data -is [System.Array]) {{
    for ($r = 1; $r -le $data.GetLength(0); $r++) {{
        $row = @()
        for ($c = 1; $c -le $data.GetLength(1); $c++) {{
            $row += $data[$r,$c]
        }}
        $result += ,$row
    }}
}} else {{
    $result = @(@($data))
}}
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
$result | ConvertTo-Json -Depth 3
"#, path.replace('"', r#"\""#), sheet, sheet, range)
            }
            ("excel", "get_sheet_names") => {
                let path = input["path"].as_str().unwrap_or("");
                format!(r#"
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $false
$wb = $excel.Workbooks.Open("{}")
$names = $wb.Sheets | ForEach-Object {{ $_.Name }}
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
$names | ConvertTo-Json
"#, path.replace('"', r#"\""#))
            }

            // ── Word ───────────────────────────────────────────────────────
            ("word", "read_document") => {
                let path = input["path"].as_str().unwrap_or("");
                format!(r#"
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$doc = $word.Documents.Open("{}")
$text = $doc.Content.Text
$doc.Close($false)
$word.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($word) | Out-Null
$text
"#, path.replace('"', r#"\""#))
            }
            ("word", "write_document") => {
                let path = input["path"].as_str().unwrap_or("");
                let text = input["text"].as_str().unwrap_or("");
                format!(r#"
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$doc = $word.Documents.Add()
$doc.Content.Text = "{}"
$doc.SaveAs2("{}")
$doc.Close()
$word.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($word) | Out-Null
"Document saved to {}"
"#, text.replace('"', r#"\""#), path.replace('"', r#"\""#), path)
            }
            ("word", "append_text") => {
                let path = input["path"].as_str().unwrap_or("");
                let text = input["text"].as_str().unwrap_or("");
                format!(r#"
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$doc = $word.Documents.Open("{}")
$range = $doc.Content
$range.Collapse([Microsoft.Office.Interop.Word.WdCollapseDirection]::wdCollapseEnd)
$range.InsertAfter("{}")
$doc.Save()
$doc.Close()
$word.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($word) | Out-Null
"Appended text to {}"
"#, path.replace('"', r#"\""#), text.replace('"', r#"\""#), path)
            }

            // ── Outlook ────────────────────────────────────────────────────
            ("outlook", "send_email") => {
                let to = input["to"].as_str().unwrap_or("");
                let subject = input["subject"].as_str().unwrap_or("");
                let body = input["body"].as_str().unwrap_or("");
                format!(r#"
$outlook = New-Object -ComObject Outlook.Application
$mail = $outlook.CreateItem(0)
$mail.To = "{}"
$mail.Subject = "{}"
$mail.Body = "{}"
$mail.Send()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($outlook) | Out-Null
"Email sent to {}"
"#, to, subject.replace('"', r#"\""#), body.replace('"', r#"\""#), to)
            }
            ("outlook", "read_emails") => {
                let max = input["max_items"].as_u64().unwrap_or(10);
                format!(r#"
$outlook = New-Object -ComObject Outlook.Application
$ns = $outlook.GetNamespace("MAPI")
$inbox = $ns.GetDefaultFolder(6)
$items = $inbox.Items
$items.Sort("[ReceivedTime]", $true)
$result = @()
$count = 0
foreach ($item in $items) {{
    if ($count -ge {}) {{ break }}
    $result += @{{
        Subject = $item.Subject
        From = $item.SenderName
        ReceivedTime = $item.ReceivedTime.ToString()
        Body = $item.Body.Substring(0, [Math]::Min(200, $item.Body.Length))
    }}
    $count++
}}
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($outlook) | Out-Null
$result | ConvertTo-Json -Depth 3
"#, max)
            }

            _ => return Ok(ToolResult::err(format!("Unknown action '{}' for app '{}'", action, app))),
        };

        self.run_ps_script(&ps_script, &ctx.workspace_root).await
    }
}

impl OfficeTool {
    async fn run_ps_script(&self, script: &str, cwd: &std::path::Path) -> Result<ToolResult> {
        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", script])
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let result = timeout(Duration::from_secs(OFFICE_TIMEOUT_SECS), cmd.output()).await;

        match result {
            Err(_) => Ok(ToolResult::err(format!("Office operation timed out after {}s", OFFICE_TIMEOUT_SECS))),
            Ok(Err(e)) => Ok(ToolResult::err(format!("Failed to run Office script: {}", e))),
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

                if !output.status.success() && stdout.is_empty() {
                    return Ok(ToolResult::err(format!("Office operation failed: {}", stderr)));
                }

                if !stderr.is_empty() && stdout.is_empty() {
                    return Ok(ToolResult::err(stderr));
                }

                Ok(ToolResult::ok(stdout))
            }
        }
    }
}

/// Office COM automation tool (Windows only).
/// Controls Excel, Word, PowerPoint, and Outlook via PowerShell COM interop.
///
/// Design principles:
/// - All string values are passed via PowerShell single-quoted variables (never interpolated),
///   which eliminates ALL escaping issues with $, ", ', backticks, etc.
/// - A single PowerShell process handles the full operation (open → modify → save → quit).
/// - Batch write operations accept JSON arrays for efficient multi-cell/multi-slide writes.
use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

const OFFICE_TIMEOUT_SECS: u64 = 120;

pub struct OfficeTool;

#[async_trait]
impl Tool for OfficeTool {
    fn name(&self) -> &str {
        "office"
    }

    fn description(&self) -> &str {
        "Automate Microsoft Office (Excel, Word, PowerPoint, Outlook) via COM.\n\
         All values are passed safely — no escaping needed for $, quotes, or special characters.\n\
         \n\
         **Excel** (app=\"excel\"):\n\
           create, open, close, save, save_as,\n\
           write_cells (batch write data+formulas), read_range,\n\
           get_sheet_names, add_sheet, add_chart, auto_fit, run_macro\n\
         \n\
         **Word** (app=\"word\"):\n\
           create, open, close, save, save_as,\n\
           read_document, write_document (replace all content),\n\
           add_paragraph (append styled paragraph),\n\
           add_table (insert table from 2D array),\n\
           add_picture (insert image from file path),\n\
           set_header_footer, find_replace\n\
         \n\
         **PowerPoint** (app=\"powerpoint\"):\n\
           create, open, close, save, save_as,\n\
           add_slide (append slide with title+content),\n\
           add_slides (batch: array of {title,content,layout} objects),\n\
           set_slide_text (edit existing slide text),\n\
           add_image (insert image onto slide),\n\
           get_slide_count, export_pdf\n\
         \n\
         **Outlook** (app=\"outlook\"):\n\
           send_email, read_emails, get_calendar\n\
         \n\
         For complex Excel tasks (regression, charts), use write_cells with a cells array,\n\
         then add_chart. For PowerPoint decks, use add_slides with a slides array.\n\
         \n\
         **IMPORTANT for add_chart**: always pass chart_type explicitly.\n\
         折线图=line, 柱状图=column, 条形图=bar, 饼图=pie, 散点图=scatter, 面积图=area.\n\
         Never omit chart_type — the default is 'line' but always state it clearly."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "app": {
                    "type": "string",
                    "enum": ["excel", "word", "powerpoint", "outlook"],
                    "description": "Office application"
                },
                "action": {
                    "type": "string",
                    "description": "Action to perform. See tool description for full list per app."
                },
                "path": {
                    "type": "string",
                    "description": "Absolute file path (REQUIRED for all actions except 'create' where it is the save path). Must always be provided."
                },
                "sheet": {
                    "type": "string",
                    "description": "[Excel] Sheet name. Default: active sheet."
                },
                "cell": {
                    "type": "string",
                    "description": "[Excel] Cell reference like 'A1'."
                },
                "range": {
                    "type": "string",
                    "description": "[Excel] Cell range like 'A1:C10'."
                },
                "value": {
                    "type": "string",
                    "description": "Value, formula, or text to write."
                },
                "cells": {
                    "type": "array",
                    "description": "[Excel write_cells] Array of {cell, value} objects. Formulas start with '='. NOTE: 'path' is required for write_cells.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "cell": { "type": "string" },
                            "value": { "type": "string" }
                        }
                    }
                },
                "chart_type": {
                    "type": "string",
                    "enum": ["line", "bar", "column", "pie", "scatter", "area"],
                    "description": "[Excel add_chart] Chart type. IMPORTANT: always specify this explicitly — do NOT omit it or guess. Use 'line' for trends/time-series (折线图), 'column' for category comparisons (柱状图), 'bar' for horizontal bars (条形图), 'pie' ONLY for part-of-whole proportions with a single data series (饼图), 'scatter' for XY correlation data (散点图), 'area' for cumulative trends (面积图). When the user asks for 折线图/line chart, you MUST pass chart_type='line'."
                },
                "chart_title": {
                    "type": "string",
                    "description": "[Excel add_chart] Chart title"
                },
                "new_path": {
                    "type": "string",
                    "description": "New file path for save_as"
                },
                "text": {
                    "type": "string",
                    "description": "[Word] Text content for paragraph/document"
                },
                "style": {
                    "type": "string",
                    "description": "[Word add_paragraph] Paragraph style: 'Normal', 'Heading 1'..'Heading 4', 'List Bullet', 'List Number', 'Quote'. Default: Normal"
                },
                "rows": {
                    "type": "array",
                    "description": "[Word add_table] 2D array of strings. First row is header. Example: [[\"Name\",\"Age\"],[\"Alice\",\"30\"]]",
                    "items": { "type": "array", "items": { "type": "string" } }
                },
                "image_path": {
                    "type": "string",
                    "description": "[Word add_picture / PowerPoint add_image] Absolute path to image file"
                },
                "header": {
                    "type": "string",
                    "description": "[Word set_header_footer] Header text"
                },
                "footer": {
                    "type": "string",
                    "description": "[Word set_header_footer] Footer text"
                },
                "find": {
                    "type": "string",
                    "description": "[Word find_replace] Text to find"
                },
                "replace": {
                    "type": "string",
                    "description": "[Word find_replace] Replacement text"
                },
                "title": {
                    "type": "string",
                    "description": "[PowerPoint] Slide title"
                },
                "content": {
                    "type": "string",
                    "description": "[PowerPoint] Slide body content (bullet points, use \\n to separate bullets)"
                },
                "layout": {
                    "type": "integer",
                    "description": "[PowerPoint add_slide] Slide layout index (1=title, 2=title+content, 11=blank). Default: 2"
                },
                "slide_index": {
                    "type": "integer",
                    "description": "[PowerPoint set_slide_text/add_image] 1-based slide index"
                },
                "slides": {
                    "type": "array",
                    "description": "[PowerPoint add_slides] Array of {title, content, layout} objects for batch slide creation",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" },
                            "content": { "type": "string" },
                            "layout": { "type": "integer" }
                        }
                    }
                },
                "to": { "type": "string", "description": "[Outlook] Email recipient(s)" },
                "subject": { "type": "string", "description": "[Outlook] Email subject" },
                "body": { "type": "string", "description": "[Outlook] Email body" },
                "max_items": { "type": "integer", "description": "[Outlook] Max items to return (default: 10)" },
                "visible": {
                    "type": "boolean",
                    "description": "Show Office window (default: false)"
                }
            },
            "required": ["app", "action"]
        })
    }

    fn needs_confirmation(&self, input: &Value) -> bool {
        matches!(
            input["action"].as_str(),
            Some("write_cells")
                | Some("set_formula")
                | Some("write_document")
                | Some("add_paragraph")
                | Some("add_table")
                | Some("add_picture")
                | Some("set_header_footer")
                | Some("find_replace")
                | Some("add_slide")
                | Some("add_slides")
                | Some("set_slide_text")
                | Some("add_image")
                | Some("send_email")
                | Some("save")
                | Some("save_as")
                | Some("run_macro")
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
        let path = input["path"].as_str().unwrap_or("");
        tracing::info!("office tool: app={} action={} path={}", app, action, path);

        let script = match self.build_script(app, action, &input) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "office build_script error: app={} action={} err={}",
                    app,
                    action,
                    e
                );
                return Ok(ToolResult::err(e));
            }
        };

        let result = self.run_ps_script(&script, &ctx.workspace_root).await;
        match &result {
            Ok(r) if r.is_error => tracing::warn!(
                "office tool error: app={} action={} result={}",
                app,
                action,
                r.content
            ),
            Ok(r) => tracing::info!(
                "office tool ok: app={} action={} result={}",
                app,
                action,
                &r.content.chars().take(120).collect::<String>()
            ),
            Err(e) => tracing::warn!(
                "office tool failed: app={} action={} err={}",
                app,
                action,
                e
            ),
        }
        result
    }
}

impl OfficeTool {
    fn build_script(&self, app: &str, action: &str, input: &Value) -> Result<String, String> {
        match (app, action) {

            // ════════════════════════════════════════════════════════════════
            // EXCEL
            // ════════════════════════════════════════════════════════════════

            ("excel", "create") => {
                let path = input["path"].as_str().unwrap_or("C:\\Users\\Public\\workbook.xlsx");
                let visible = input["visible"].as_bool().unwrap_or(false);
                Ok(format!(r#"
$path = {path_var}
$excel = New-Object -ComObject Excel.Application
$excel.Visible = ${vis}
$excel.DisplayAlerts = $false
$wb = $excel.Workbooks.Add()
$wb.SaveAs($path, 51)
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($wb) | Out-Null
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
"Created: $path"
"#, path_var = ps_str(path), vis = visible))
            }

            ("excel", "open") => {
                // open: inspect file info only — opens, reads metadata, then closes cleanly.
                // Do NOT use open to "keep Excel open" — each action is self-contained.
                let path = input["path"].as_str().unwrap_or("");
                let visible = input["visible"].as_bool().unwrap_or(false);
                Ok(format!(r#"
$path = {path_var}
$excel = New-Object -ComObject Excel.Application
$excel.Visible = ${vis}
$excel.DisplayAlerts = $false
$wb = $excel.Workbooks.Open($path)
$info = "File: " + $wb.Name + " | Sheets: " + $wb.Sheets.Count + " | Path: " + $wb.FullName
$wb.Close($false)
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
$info
"#, path_var = ps_str(path), vis = visible))
            }

            ("excel", "write_cells") => {
                let path = input["path"].as_str().unwrap_or("");
                if path.is_empty() { return Err("write_cells requires 'path'".into()); }
                let sheet = input["sheet"].as_str().unwrap_or("");
                let _visible = input["visible"].as_bool().unwrap_or(false);
                let cells = match input["cells"].as_array() {
                    Some(c) if !c.is_empty() => c,
                    _ => return Err("write_cells requires non-empty 'cells' array".into()),
                };
                let mut var_decls = String::new();
                let mut write_stmts = String::new();
                for (i, entry) in cells.iter().enumerate() {
                    let cell = entry["cell"].as_str().unwrap_or("A1");
                    let val = entry["value"].as_str().unwrap_or("");
                    var_decls.push_str(&format!("$v{i} = {}\n", ps_str(val)));
                    if val.starts_with('=') {
                        write_stmts.push_str(&format!("$ws.Range(\"{cell}\").Formula = $v{i}\n"));
                    } else {
                        write_stmts.push_str(&format!("$ws.Range(\"{cell}\").Value2 = $v{i}\n"));
                    }
                }
                Ok(format!(r#"
$path = {path_var}
{var_decls}
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $false
$excel.DisplayAlerts = $false
$existed = Test-Path $path
if ($existed) {{ $wb = $excel.Workbooks.Open($path) }} else {{ $wb = $excel.Workbooks.Add() }}
$ws = if ({sheet_check}) {{ $wb.ActiveSheet }} else {{ $wb.Sheets[{sheet_var}] }}
{write_stmts}
if ($existed) {{ $wb.Save() }} else {{ $wb.SaveAs($path, 51) }}
$wb.Close($false)
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($wb) | Out-Null
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
"Written {count} cells to $path"
"#,
                    path_var = ps_str(path), var_decls = var_decls,
                    sheet_check = sheet_check(sheet), sheet_var = ps_str(sheet),
                    write_stmts = write_stmts, count = cells.len()))
            }

            ("excel", "set_formula") => {
                let path = input["path"].as_str().unwrap_or("");
                let sheet = input["sheet"].as_str().unwrap_or("");
                let cell = input["cell"].as_str().unwrap_or("A1");
                let value = input["value"].as_str().unwrap_or("");
                let write_stmt = if value.starts_with('=') {
                    format!("$ws.Range(\"{cell}\").Formula = $val")
                } else {
                    format!("$ws.Range(\"{cell}\").Value2 = $val")
                };
                Ok(format!(r#"
$path = {path_var}
$val = {val_var}
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $false
$excel.DisplayAlerts = $false
$existed = Test-Path $path
if ($existed) {{ $wb = $excel.Workbooks.Open($path) }} else {{ $wb = $excel.Workbooks.Add() }}
$ws = if ({sheet_check}) {{ $wb.ActiveSheet }} else {{ $wb.Sheets[{sheet_var}] }}
{write_stmt}
if ($existed) {{ $wb.Save() }} else {{ $wb.SaveAs($path, 51) }}
$wb.Close($false)
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($wb) | Out-Null
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
"Set {cell} in $path"
"#,
                    path_var = ps_str(path), val_var = ps_str(value),
                    sheet_check = sheet_check(sheet), sheet_var = ps_str(sheet),
                    write_stmt = write_stmt, cell = cell))
            }

            ("excel", "read_range") => {
                let path = input["path"].as_str().unwrap_or("");
                let sheet = input["sheet"].as_str().unwrap_or("");
                let range = input["range"].as_str().unwrap_or("A1:A10");
                Ok(format!(r#"
$path = {path_var}
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $false
$excel.DisplayAlerts = $false
$wb = $excel.Workbooks.Open($path)
$ws = if ({sheet_check}) {{ $wb.ActiveSheet }} else {{ $wb.Sheets[{sheet_var}] }}
$data = $ws.Range("{range}").Value2
$result = @()
if ($data -is [System.Array]) {{
    for ($r = 1; $r -le $data.GetLength(0); $r++) {{
        $row = @()
        for ($c = 1; $c -le $data.GetLength(1); $c++) {{ $row += $data[$r,$c] }}
        $result += ,$row
    }}
}} else {{ $result = @(@($data)) }}
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($wb) | Out-Null
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
$result | ConvertTo-Json -Depth 4
"#, path_var = ps_str(path), sheet_check = sheet_check(sheet), sheet_var = ps_str(sheet), range = range))
            }

            ("excel", "add_chart") => {
                let path = input["path"].as_str().unwrap_or("");
                let sheet = input["sheet"].as_str().unwrap_or("");
                let range = input["range"].as_str().unwrap_or("A1:B10");
                let chart_type = input["chart_type"].as_str().unwrap_or("line");
                let chart_title = input["chart_title"].as_str().unwrap_or("Chart");
                let xl_type: i32 = match chart_type {
                    "line" => -4102, "bar" => 2, "column" => 51,
                    "pie" => 5, "scatter" => -4169, "area" => 1, _ => -4169,
                };
                Ok(format!(r#"
$path = {path_var}
$title = {title_var}
$xlType = {xl_type}
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $false
$excel.DisplayAlerts = $false
$wb = $excel.Workbooks.Open($path)
$ws = if ({sheet_check}) {{ $wb.ActiveSheet }} else {{ $wb.Sheets[{sheet_var}] }}
$chartObj = $ws.ChartObjects().Add(300, 20, 480, 320)
$chart = $chartObj.Chart
$chart.ChartType = $xlType
$chart.SetSourceData($ws.Range("{range}"))
$chart.ChartType = $xlType
$chart.HasTitle = $true
$chart.ChartTitle.Text = $title
$wb.Save()
$wb.Close($false)
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($wb) | Out-Null
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
"Chart (type=$xlType) added to $path"
"#,
                    path_var = ps_str(path), title_var = ps_str(chart_title),
                    sheet_check = sheet_check(sheet), sheet_var = ps_str(sheet),
                    range = range, xl_type = xl_type))
            }

            ("excel", "auto_fit") => {
                let path = input["path"].as_str().unwrap_or("");
                let sheet = input["sheet"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $false
$excel.DisplayAlerts = $false
$wb = $excel.Workbooks.Open($path)
$ws = if ({sheet_check}) {{ $wb.ActiveSheet }} else {{ $wb.Sheets[{sheet_var}] }}
$ws.Columns.AutoFit() | Out-Null
$wb.Save()
$wb.Close($false)
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($wb) | Out-Null
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
"AutoFit applied to $path"
"#, path_var = ps_str(path), sheet_check = sheet_check(sheet), sheet_var = ps_str(sheet)))
            }

            ("excel", "get_sheet_names") => {
                let path = input["path"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $false
$excel.DisplayAlerts = $false
$wb = $excel.Workbooks.Open($path)
$names = $wb.Sheets | ForEach-Object {{ $_.Name }}
$wb.Close($false)
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($wb) | Out-Null
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
$names | ConvertTo-Json
"#, path_var = ps_str(path)))
            }

            ("excel", "add_sheet") => {
                let path = input["path"].as_str().unwrap_or("");
                let sheet = input["sheet"].as_str().unwrap_or("Sheet2");
                Ok(format!(r#"
$path = {path_var}
$sheetName = {sheet_var}
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $false
$excel.DisplayAlerts = $false
$wb = $excel.Workbooks.Open($path)
$ws = $wb.Sheets.Add()
$ws.Name = $sheetName
$wb.Save()
$wb.Close($false)
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($wb) | Out-Null
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
"Added sheet: $sheetName"
"#, path_var = ps_str(path), sheet_var = ps_str(sheet)))
            }

            ("excel", "save") => {
                let path = input["path"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $false
$excel.DisplayAlerts = $false
$wb = $excel.Workbooks.Open($path)
$wb.Save()
$wb.Close($false)
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($wb) | Out-Null
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
"Saved: $path"
"#, path_var = ps_str(path)))
            }

            ("excel", "save_as") => {
                let path = input["path"].as_str().unwrap_or("");
                let new_path = input["new_path"].as_str().or_else(|| input["value"].as_str()).unwrap_or("");
                if new_path.is_empty() { return Err("save_as requires 'new_path'".into()); }
                Ok(format!(r#"
$path = {path_var}
$newPath = {new_path_var}
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $false; $excel.DisplayAlerts = $false
$wb = $excel.Workbooks.Open($path)
$wb.SaveAs($newPath, 51)
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($wb) | Out-Null
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
"Saved as: $newPath"
"#, path_var = ps_str(path), new_path_var = ps_str(new_path)))
            }

            ("excel", "close") => {
                let path = input["path"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $false; $excel.DisplayAlerts = $false
$wb = $excel.Workbooks.Open($path)
$wb.Close($false)
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
"Closed: $path"
"#, path_var = ps_str(path)))
            }

            ("excel", "run_macro") => {
                let path = input["path"].as_str().unwrap_or("");
                let macro_name = input["value"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$macro = {macro_var}
$excel = New-Object -ComObject Excel.Application
$excel.Visible = $false
$excel.DisplayAlerts = $false
$wb = $excel.Workbooks.Open($path)
$excel.Run($macro)
$wb.Save()
$wb.Close($false)
$excel.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($wb) | Out-Null
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($excel) | Out-Null
"Macro '$macro' executed"
"#, path_var = ps_str(path), macro_var = ps_str(macro_name)))
            }

            // ════════════════════════════════════════════════════════════════
            // WORD
            // ════════════════════════════════════════════════════════════════

            ("word", "create") => {
                let path = input["path"].as_str().unwrap_or("C:\\Users\\Public\\document.docx");
                Ok(format!(r#"
$path = {path_var}
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$doc = $word.Documents.Add()
$doc.SaveAs2($path)
$doc.Close()
$word.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($word) | Out-Null
"Created: $path"
"#, path_var = ps_str(path)))
            }

            ("word", "open") => {
                let path = input["path"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$doc = $word.Documents.Open($path, $false, $true)
$info = "File: " + $doc.Name + " | Pages: " + $doc.ComputeStatistics(2) + " | Words: " + $doc.ComputeStatistics(0)
$doc.Close($false)
$word.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($word) | Out-Null
$info
"#, path_var = ps_str(path)))
            }

            ("word", "read_document") => {
                let path = input["path"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$doc = $word.Documents.Open($path)
$text = $doc.Content.Text
$doc.Close($false)
$word.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($word) | Out-Null
$text
"#, path_var = ps_str(path)))
            }

            ("word", "write_document") => {
                let path = input["path"].as_str().unwrap_or("");
                let text = input["text"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$content = {text_var}
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$doc = $word.Documents.Add()
$doc.Content.Text = $content
$doc.SaveAs2($path)
$doc.Close()
$word.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($word) | Out-Null
"Document saved to $path"
"#, path_var = ps_str(path), text_var = ps_str(text)))
            }

            // Add a paragraph with optional style (Heading 1..4, List Bullet, Normal, etc.)
            ("word", "add_paragraph") => {
                let path = input["path"].as_str().unwrap_or("");
                let text = input["text"].as_str().unwrap_or("");
                let style = input["style"].as_str().unwrap_or("Normal");
                Ok(format!(r#"
$path = {path_var}
$content = {text_var}
$styleName = {style_var}
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$doc = $word.Documents.Open($path)
$range = $doc.Content
$range.Collapse(0)
$para = $doc.Paragraphs.Add($range)
$para.Range.Text = $content
$para.Style = $doc.Styles[$styleName]
$para.Range.InsertParagraphAfter()
$doc.Save()
$doc.Close()
$word.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($word) | Out-Null
"Paragraph added with style '$styleName'"
"#, path_var = ps_str(path), text_var = ps_str(text), style_var = ps_str(style)))
            }

            // Append plain text (legacy compat)
            ("word", "append_text") => {
                let path = input["path"].as_str().unwrap_or("");
                let text = input["text"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$content = {text_var}
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$doc = $word.Documents.Open($path)
$range = $doc.Content
$range.Collapse(0)
$range.InsertAfter($content)
$doc.Save()
$doc.Close()
$word.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($word) | Out-Null
"Appended text to $path"
"#, path_var = ps_str(path), text_var = ps_str(text)))
            }

            // Insert a table from a 2D array: rows=[[header1,header2],[val1,val2],...]
            ("word", "add_table") => {
                let path = input["path"].as_str().unwrap_or("");
                let rows = match input["rows"].as_array() {
                    Some(r) if !r.is_empty() => r,
                    _ => return Err("add_table requires non-empty 'rows' array (2D array of strings)".into()),
                };
                let num_rows = rows.len();
                let num_cols = rows[0].as_array().map(|r| r.len()).unwrap_or(1);
                // Build PS array literal: @(@('r0c0','r0c1'),@('r1c0','r1c1'),...)
                let mut ps_rows = String::new();
                for row in rows {
                    let cols: Vec<String> = row.as_array()
                        .map(|cols| cols.iter().map(|v| ps_str(v.as_str().unwrap_or(""))).collect())
                        .unwrap_or_default();
                    ps_rows.push_str(&format!("@({}),\n", cols.join(",")));
                }
                Ok(format!(r#"
$path = {path_var}
$tableData = @(
{ps_rows}
)
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$doc = $word.Documents.Open($path)
$range = $doc.Content
$range.Collapse(0)
$table = $doc.Tables.Add($range, {num_rows}, {num_cols})
$table.Style = 'Table Grid'
for ($r = 0; $r -lt $tableData.Count; $r++) {{
    for ($c = 0; $c -lt $tableData[$r].Count; $c++) {{
        $table.Cell($r+1, $c+1).Range.Text = $tableData[$r][$c]
    }}
}}
# Bold header row
for ($c = 1; $c -le {num_cols}; $c++) {{
    $table.Cell(1, $c).Range.Bold = $true
}}
$doc.Save()
$doc.Close()
$word.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($word) | Out-Null
"Table {num_rows}x{num_cols} added to $path"
"#, path_var = ps_str(path), ps_rows = ps_rows, num_rows = num_rows, num_cols = num_cols))
            }

            // Insert an image at end of document
            ("word", "add_picture") => {
                let path = input["path"].as_str().unwrap_or("");
                let image_path = input["image_path"].as_str().unwrap_or("");
                if image_path.is_empty() { return Err("add_picture requires 'image_path'".into()); }
                Ok(format!(r#"
$path = {path_var}
$imgPath = {img_var}
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$doc = $word.Documents.Open($path)
$range = $doc.Content
$range.Collapse(0)
$doc.InlineShapes.AddPicture($imgPath, $false, $true, $range) | Out-Null
$doc.Save()
$doc.Close()
$word.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($word) | Out-Null
"Picture inserted into $path"
"#, path_var = ps_str(path), img_var = ps_str(image_path)))
            }

            // Set header and/or footer text
            ("word", "set_header_footer") => {
                let path = input["path"].as_str().unwrap_or("");
                let header = input["header"].as_str().unwrap_or("");
                let footer = input["footer"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$headerText = {header_var}
$footerText = {footer_var}
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$doc = $word.Documents.Open($path)
$section = $doc.Sections(1)
if ($headerText -ne '') {{
    $section.Headers(1).Range.Text = $headerText
}}
if ($footerText -ne '') {{
    $section.Footers(1).Range.Text = $footerText
}}
$doc.Save()
$doc.Close()
$word.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($word) | Out-Null
"Header/footer set in $path"
"#, path_var = ps_str(path), header_var = ps_str(header), footer_var = ps_str(footer)))
            }

            // Find and replace text throughout document
            ("word", "find_replace") => {
                let path = input["path"].as_str().unwrap_or("");
                let find = input["find"].as_str().unwrap_or("");
                let replace = input["replace"].as_str().unwrap_or("");
                if find.is_empty() { return Err("find_replace requires 'find'".into()); }
                Ok(format!(r#"
$path = {path_var}
$findText = {find_var}
$replaceText = {replace_var}
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$doc = $word.Documents.Open($path)
$find = $doc.Content.Find
$find.ClearFormatting()
$find.Replacement.ClearFormatting()
$find.Execute($findText, $false, $false, $false, $false, $false, $true, 1, $true, $replaceText, 2) | Out-Null
$doc.Save()
$doc.Close()
$word.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($word) | Out-Null
"Find/replace completed in $path"
"#, path_var = ps_str(path), find_var = ps_str(find), replace_var = ps_str(replace)))
            }

            ("word", "save") => {
                let path = input["path"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$doc = $word.Documents.Open($path)
$doc.Save()
$doc.Close()
$word.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($word) | Out-Null
"Saved: $path"
"#, path_var = ps_str(path)))
            }

            ("word", "save_as") => {
                let path = input["path"].as_str().unwrap_or("");
                let new_path = input["new_path"].as_str().or_else(|| input["value"].as_str()).unwrap_or("");
                if new_path.is_empty() { return Err("save_as requires 'new_path'".into()); }
                Ok(format!(r#"
$path = {path_var}
$newPath = {new_path_var}
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$doc = $word.Documents.Open($path)
$doc.SaveAs2($newPath)
$doc.Close()
$word.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($word) | Out-Null
"Saved as: $newPath"
"#, path_var = ps_str(path), new_path_var = ps_str(new_path)))
            }

            ("word", "close") => {
                let path = input["path"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$word = New-Object -ComObject Word.Application
$word.Visible = $false
$doc = $word.Documents.Open($path)
$doc.Close($false)
$word.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($word) | Out-Null
"Closed: $path"
"#, path_var = ps_str(path)))
            }

            // ════════════════════════════════════════════════════════════════
            // POWERPOINT
            // ════════════════════════════════════════════════════════════════

            ("powerpoint", "create") => {
                let path = input["path"].as_str().unwrap_or("C:\\Users\\Public\\presentation.pptx");
                Ok(format!(r#"
$path = {path_var}
$ppt = New-Object -ComObject PowerPoint.Application
$ppt.Visible = [Microsoft.Office.Core.MsoTriState]::msoFalse
$pres = $ppt.Presentations.Add($false)
$pres.SaveAs($path, 24)
$pres.Close()
$ppt.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($ppt) | Out-Null
"Created: $path"
"#, path_var = ps_str(path)))
            }

            ("powerpoint", "open") => {
                // open: inspect file info only — reads metadata then closes cleanly.
                let path = input["path"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$ppt = New-Object -ComObject PowerPoint.Application
$ppt.Visible = [Microsoft.Office.Core.MsoTriState]::msoFalse
$pres = $ppt.Presentations.Open($path, $true, $false, $false)
$info = "File: " + $pres.Name + " | Slides: " + $pres.Slides.Count
$pres.Close()
$ppt.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($ppt) | Out-Null
$info
"#, path_var = ps_str(path)))
            }

            // Add a single slide with title + content
            ("powerpoint", "add_slide") => {
                let path = input["path"].as_str().unwrap_or("");
                let title = input["title"].as_str().unwrap_or("");
                let content = input["content"].as_str().unwrap_or("");
                let layout = input["layout"].as_i64().unwrap_or(2) as i32;
                Ok(format!(r#"
$path = {path_var}
$slideTitle = {title_var}
$slideContent = {content_var}
$ppt = New-Object -ComObject PowerPoint.Application
$ppt.Visible = [Microsoft.Office.Core.MsoTriState]::msoFalse
$pres = $ppt.Presentations.Open($path, $false, $false, $false)
$layout = $pres.SlideMaster.CustomLayouts.Item({layout})
$slide = $pres.Slides.AddSlide($pres.Slides.Count + 1, $layout)
if ($slide.Shapes.Count -ge 1 -and $slideTitle -ne '') {{
    $slide.Shapes(1).TextFrame.TextRange.Text = $slideTitle
}}
if ($slide.Shapes.Count -ge 2 -and $slideContent -ne '') {{
    $slide.Shapes(2).TextFrame.TextRange.Text = $slideContent
}}
$pres.Save()
$pres.Close()
$ppt.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($ppt) | Out-Null
"Slide added: $slideTitle"
"#, path_var = ps_str(path), title_var = ps_str(title), content_var = ps_str(content), layout = layout))
            }

            // Batch add slides: slides=[{title,content,layout},...]
            ("powerpoint", "add_slides") => {
                let path = input["path"].as_str().unwrap_or("");
                if path.is_empty() { return Err("add_slides requires 'path'".into()); }
                let slides = match input["slides"].as_array() {
                    Some(s) if !s.is_empty() => s,
                    _ => return Err("add_slides requires non-empty 'slides' array".into()),
                };
                let mut slide_stmts = String::new();
                for (i, slide) in slides.iter().enumerate() {
                    let title = slide["title"].as_str().unwrap_or("");
                    let content = slide["content"].as_str().unwrap_or("");
                    let layout = slide["layout"].as_i64().unwrap_or(2);
                    slide_stmts.push_str(&format!(r#"
$t{i} = {title_var}
$c{i} = {content_var}
$layout{i} = $pres.SlideMaster.CustomLayouts.Item({layout})
$slide{i} = $pres.Slides.AddSlide($pres.Slides.Count + 1, $layout{i})
if ($slide{i}.Shapes.Count -ge 1 -and $t{i} -ne '') {{ $slide{i}.Shapes(1).TextFrame.TextRange.Text = $t{i} }}
if ($slide{i}.Shapes.Count -ge 2 -and $c{i} -ne '') {{ $slide{i}.Shapes(2).TextFrame.TextRange.Text = $c{i} }}
"#,
                        i = i, title_var = ps_str(title), content_var = ps_str(content), layout = layout));
                }
                Ok(format!(r#"
$path = {path_var}
$ppt = New-Object -ComObject PowerPoint.Application
$ppt.Visible = [Microsoft.Office.Core.MsoTriState]::msoFalse
$existed = Test-Path $path
if ($existed) {{
    $pres = $ppt.Presentations.Open($path, $false, $false, $false)
}} else {{
    $pres = $ppt.Presentations.Add($false)
}}
{slide_stmts}
if ($existed) {{ $pres.Save() }} else {{ $pres.SaveAs($path, 24) }}
$pres.Close()
$ppt.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($ppt) | Out-Null
"Added {count} slides to $path"
"#, path_var = ps_str(path), slide_stmts = slide_stmts, count = slides.len()))
            }

            // Edit text of an existing slide (1-based index)
            ("powerpoint", "set_slide_text") => {
                let path = input["path"].as_str().unwrap_or("");
                let idx = input["slide_index"].as_i64().unwrap_or(1);
                let title = input["title"].as_str().unwrap_or("");
                let content = input["content"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$slideTitle = {title_var}
$slideContent = {content_var}
$ppt = New-Object -ComObject PowerPoint.Application
$ppt.Visible = [Microsoft.Office.Core.MsoTriState]::msoFalse
$pres = $ppt.Presentations.Open($path, $false, $false, $false)
$slide = $pres.Slides({idx})
if ($slide.Shapes.Count -ge 1 -and $slideTitle -ne '') {{ $slide.Shapes(1).TextFrame.TextRange.Text = $slideTitle }}
if ($slide.Shapes.Count -ge 2 -and $slideContent -ne '') {{ $slide.Shapes(2).TextFrame.TextRange.Text = $slideContent }}
$pres.Save()
$pres.Close()
$ppt.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($ppt) | Out-Null
"Slide {idx} updated"
"#, path_var = ps_str(path), title_var = ps_str(title), content_var = ps_str(content), idx = idx))
            }

            // Insert image onto a slide
            ("powerpoint", "add_image") => {
                let path = input["path"].as_str().unwrap_or("");
                let image_path = input["image_path"].as_str().unwrap_or("");
                if image_path.is_empty() { return Err("add_image requires 'image_path'".into()); }
                let idx = input["slide_index"].as_i64().unwrap_or(1);
                Ok(format!(r#"
$path = {path_var}
$imgPath = {img_var}
$ppt = New-Object -ComObject PowerPoint.Application
$ppt.Visible = [Microsoft.Office.Core.MsoTriState]::msoFalse
$pres = $ppt.Presentations.Open($path, $false, $false, $false)
$slide = $pres.Slides({idx})
$slide.Shapes.AddPicture($imgPath, $false, $true, 50, 150, 600, 350) | Out-Null
$pres.Save()
$pres.Close()
$ppt.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($ppt) | Out-Null
"Image added to slide {idx}"
"#, path_var = ps_str(path), img_var = ps_str(image_path), idx = idx))
            }

            ("powerpoint", "get_slide_count") => {
                let path = input["path"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$ppt = New-Object -ComObject PowerPoint.Application
$ppt.Visible = [Microsoft.Office.Core.MsoTriState]::msoFalse
$pres = $ppt.Presentations.Open($path, $true, $false, $false)
$count = $pres.Slides.Count
$pres.Close()
$ppt.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($ppt) | Out-Null
"Slide count: $count"
"#, path_var = ps_str(path)))
            }

            // Export presentation to PDF
            ("powerpoint", "export_pdf") => {
                let path = input["path"].as_str().unwrap_or("");
                let new_path = input["new_path"].as_str().or_else(|| input["value"].as_str()).unwrap_or("");
                if new_path.is_empty() { return Err("export_pdf requires 'new_path' for the output PDF path".into()); }
                Ok(format!(r#"
$path = {path_var}
$pdfPath = {pdf_var}
$ppt = New-Object -ComObject PowerPoint.Application
$ppt.Visible = [Microsoft.Office.Core.MsoTriState]::msoFalse
$pres = $ppt.Presentations.Open($path, $false, $false, $false)
$pres.ExportAsFixedFormat($pdfPath, 2)
$pres.Close()
$ppt.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($ppt) | Out-Null
"Exported to PDF: $pdfPath"
"#, path_var = ps_str(path), pdf_var = ps_str(new_path)))
            }

            ("powerpoint", "save") => {
                let path = input["path"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$ppt = New-Object -ComObject PowerPoint.Application
$ppt.Visible = [Microsoft.Office.Core.MsoTriState]::msoFalse
$pres = $ppt.Presentations.Open($path, $false, $false, $false)
$pres.Save()
$pres.Close()
$ppt.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($ppt) | Out-Null
"Saved: $path"
"#, path_var = ps_str(path)))
            }

            ("powerpoint", "save_as") => {
                let path = input["path"].as_str().unwrap_or("");
                let new_path = input["new_path"].as_str().or_else(|| input["value"].as_str()).unwrap_or("");
                if new_path.is_empty() { return Err("save_as requires 'new_path'".into()); }
                Ok(format!(r#"
$path = {path_var}
$newPath = {new_path_var}
$ppt = New-Object -ComObject PowerPoint.Application
$ppt.Visible = [Microsoft.Office.Core.MsoTriState]::msoFalse
$pres = $ppt.Presentations.Open($path, $false, $false, $false)
$pres.SaveAs($newPath, 24)
$pres.Close()
$ppt.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($ppt) | Out-Null
"Saved as: $newPath"
"#, path_var = ps_str(path), new_path_var = ps_str(new_path)))
            }

            ("powerpoint", "close") => {
                let path = input["path"].as_str().unwrap_or("");
                Ok(format!(r#"
$path = {path_var}
$ppt = New-Object -ComObject PowerPoint.Application
$ppt.Visible = [Microsoft.Office.Core.MsoTriState]::msoFalse
$pres = $ppt.Presentations.Open($path, $false, $false, $false)
$pres.Close()
$ppt.Quit()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($ppt) | Out-Null
"Closed: $path"
"#, path_var = ps_str(path)))
            }

            // ════════════════════════════════════════════════════════════════
            // OUTLOOK
            // ════════════════════════════════════════════════════════════════

            ("outlook", "send_email") => {
                let to = input["to"].as_str().unwrap_or("");
                let subject = input["subject"].as_str().unwrap_or("");
                let body = input["body"].as_str().unwrap_or("");
                Ok(format!(r#"
$to = {to_var}
$subject = {subject_var}
$body = {body_var}
$outlook = New-Object -ComObject Outlook.Application
$mail = $outlook.CreateItem(0)
$mail.To = $to
$mail.Subject = $subject
$mail.Body = $body
$mail.Send()
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($outlook) | Out-Null
"Email sent to $to"
"#, to_var = ps_str(to), subject_var = ps_str(subject), body_var = ps_str(body)))
            }

            ("outlook", "read_emails") => {
                let max = input["max_items"].as_u64().unwrap_or(10);
                Ok(format!(r#"
$outlook = New-Object -ComObject Outlook.Application
$ns = $outlook.GetNamespace("MAPI")
$inbox = $ns.GetDefaultFolder(6)
$items = $inbox.Items
$items.Sort("[ReceivedTime]", $true)
$result = @()
$count = 0
foreach ($item in $items) {{
    if ($count -ge {max}) {{ break }}
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
"#, max = max))
            }

            ("outlook", "get_calendar") => {
                let max = input["max_items"].as_u64().unwrap_or(10);
                Ok(format!(r#"
$outlook = New-Object -ComObject Outlook.Application
$ns = $outlook.GetNamespace("MAPI")
$calendar = $ns.GetDefaultFolder(9)
$items = $calendar.Items
$items.IncludeRecurrences = $true
$items.Sort("[Start]")
$start = [DateTime]::Now.ToString("MM/dd/yyyy HH:mm")
$end = [DateTime]::Now.AddDays(30).ToString("MM/dd/yyyy HH:mm")
$filter = "[Start] >= '$start' AND [End] <= '$end'"
$filtered = $items.Restrict($filter)
$result = @()
$count = 0
foreach ($item in $filtered) {{
    if ($count -ge {max}) {{ break }}
    $result += @{{
        Subject = $item.Subject
        Start = $item.Start.ToString()
        End = $item.End.ToString()
        Location = $item.Location
    }}
    $count++
}}
[System.Runtime.Interopservices.Marshal]::ReleaseComObject($outlook) | Out-Null
$result | ConvertTo-Json -Depth 3
"#, max = max))
            }

            _ => Err(format!(
                "Unknown action '{}' for app '{}'. \
                 Excel: create/open/close/save/save_as/write_cells/set_formula/read_range/get_sheet_names/add_sheet/add_chart/auto_fit/run_macro. \
                 Word: create/open/close/save/save_as/read_document/write_document/add_paragraph/append_text/add_table/add_picture/set_header_footer/find_replace. \
                 PowerPoint: create/open/close/save/save_as/add_slide/add_slides/set_slide_text/add_image/get_slide_count/export_pdf. \
                 Outlook: send_email/read_emails/get_calendar.",
                action, app
            )),
        }
    }

    async fn run_ps_script(&self, script: &str, cwd: &std::path::Path) -> Result<ToolResult> {
        let utf8_prefix = "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;\
                           $OutputEncoding=[System.Text.Encoding]::UTF8;\
                           chcp 65001 | Out-Null; ";
        let full_script = format!("{}{}", utf8_prefix, script);
        let safe_cwd = if cwd.exists() {
            cwd.to_path_buf()
        } else {
            std::path::PathBuf::from("C:\\")
        };

        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-NonInteractive", "-Command", &full_script])
            .current_dir(&safe_cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(target_os = "windows")]
        cmd.creation_flags(0x0800_0000);

        tracing::debug!(
            "office PS script (first 200 chars): {}",
            &script.chars().take(200).collect::<String>()
        );
        match timeout(Duration::from_secs(OFFICE_TIMEOUT_SECS), cmd.output()).await {
            Err(_) => {
                tracing::warn!("office PS script timed out after {}s", OFFICE_TIMEOUT_SECS);
                Ok(ToolResult::err(format!(
                    "Office operation timed out after {}s",
                    OFFICE_TIMEOUT_SECS
                )))
            }
            Ok(Err(e)) => {
                tracing::warn!("office PS script spawn error: {}", e);
                Ok(ToolResult::err(format!(
                    "Failed to run Office script: {}",
                    e
                )))
            }
            Ok(Ok(output)) => {
                let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                tracing::debug!(
                    "office PS stdout: {} | stderr: {}",
                    &stdout.chars().take(200).collect::<String>(),
                    &stderr.chars().take(200).collect::<String>()
                );
                if !output.status.success() && stdout.is_empty() {
                    return Ok(ToolResult::err(format!(
                        "Office operation failed:\n{}",
                        stderr
                    )));
                }
                if !stderr.is_empty() && stdout.is_empty() {
                    return Ok(ToolResult::err(stderr));
                }
                let mut result = stdout;
                if !stderr.is_empty() {
                    result.push_str(&format!("\n[warnings: {}]", stderr));
                }
                Ok(ToolResult::ok(result))
            }
        }
    }
}

/// Encode a Rust &str as a PowerShell single-quoted string literal.
/// Single-quoted PS strings have NO variable expansion and NO escape sequences —
/// the only special sequence is '' (two single quotes) = one literal quote.
/// This is the safest way to pass arbitrary values (formulas, paths, text) to PowerShell.
fn ps_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Generate the PowerShell condition for selecting a worksheet.
/// Returns "$true" (use ActiveSheet) when sheet name is empty,
/// "$false" (use named sheet) when a sheet name is provided.
fn sheet_check(sheet: &str) -> String {
    if sheet.is_empty() {
        "$true".into()
    } else {
        "$false".into()
    }
}

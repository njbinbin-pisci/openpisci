/// PDF read/write tool.
///
/// Supports: read_text, get_info, extract_images, merge, split,
///           add_watermark, encrypt, decrypt, fill_form, add_annotation, to_images,
///           render_page_image, render_region_image.
use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use base64::Engine;
use image::GenericImageView;
use serde_json::{json, Value};
use std::io::Cursor;
use std::path::{Path, PathBuf};

pub struct PdfTool;

// ─── Tool trait ───────────────────────────────────────────────────────────────

#[async_trait]
impl Tool for PdfTool {
    fn name(&self) -> &str {
        "pdf"
    }

    fn description(&self) -> &str {
        "Read and write PDF files. Supports text extraction, metadata, merging, splitting, \
         watermarks, encryption, form filling, annotations, and page-to-image conversion.\n\
         Actions:\n\
         - read_text: extract text from all or specific pages\n\
         - get_info: get page count, title, author, encryption status\n\
         - extract_images: save embedded images to a directory\n\
         - merge: combine multiple PDFs into one\n\
         - split: split a PDF into multiple files by page ranges\n\
         - add_watermark: stamp text diagonally on every page\n\
         - encrypt: password-protect a PDF\n\
         - decrypt: remove password protection\n\
         - fill_form: fill AcroForm text fields\n\
         - add_annotation: add a text annotation to a page\n\
         - to_images: render each page to a PNG (requires Ghostscript or pdftoppm)\n\
         - render_page_image: render one page and return it inline for multimodal analysis\n\
         - render_region_image: render one page, crop a pixel region, and return it inline"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "read_text","get_info","extract_images","merge","split",
                        "add_watermark","encrypt","decrypt","fill_form",
                        "add_annotation","to_images","render_page_image","render_region_image"
                    ],
                    "description": "Operation to perform"
                },
                "path": {
                    "type": "string",
                    "description": "Path to the source PDF file"
                },
                "output": {
                    "type": "string",
                    "description": "Path for the output PDF file (write operations)"
                },
                "output_dir": {
                    "type": "string",
                    "description": "Directory to write output files into (split / extract_images / to_images)"
                },
                "pages": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "description": "1-based page numbers to read (read_text). Omit for all pages."
                },
                "files": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "List of PDF paths to merge (merge action)"
                },
                "ranges": {
                    "type": "array",
                    "items": {
                        "type": "array",
                        "items": { "type": "integer" },
                        "minItems": 2,
                        "maxItems": 2
                    },
                    "description": "Page ranges for split, e.g. [[1,3],[5,7]]"
                },
                "text": {
                    "type": "string",
                    "description": "Watermark text (add_watermark) or annotation text (add_annotation)"
                },
                "user_password": {
                    "type": "string",
                    "description": "User (open) password for encrypt"
                },
                "owner_password": {
                    "type": "string",
                    "description": "Owner (permissions) password for encrypt"
                },
                "password": {
                    "type": "string",
                    "description": "Password for decrypt"
                },
                "fields": {
                    "type": "object",
                    "description": "Field name → value map for fill_form"
                },
                "page": {
                    "type": "integer",
                    "description": "1-based page number for add_annotation / render_page_image / render_region_image"
                },
                "rect": {
                    "type": "array",
                    "items": { "type": "number" },
                    "minItems": 4,
                    "maxItems": 4,
                    "description": "Annotation rectangle [x1,y1,x2,y2] in PDF points (add_annotation)"
                },
                "dpi": {
                    "type": "integer",
                    "description": "Resolution for to_images / render_page_image / render_region_image (default 150)"
                },
                "crop_rect": {
                    "type": "array",
                    "items": { "type": "integer" },
                    "minItems": 4,
                    "maxItems": 4,
                    "description": "Pixel crop rectangle [left, top, width, height] after rendering the page (render_region_image)"
                }
            },
            "required": ["action"]
        })
    }

    fn is_read_only(&self) -> bool {
        false
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let action = match input["action"].as_str() {
            Some(a) => a.to_string(),
            None => return Ok(ToolResult::err("Missing required parameter: action")),
        };

        // Run blocking PDF work on a thread-pool thread so we don't block the async runtime.
        let input_clone = input.clone();
        let workspace = ctx.workspace_root.clone();

        let result =
            tokio::task::spawn_blocking(move || dispatch(&action, &input_clone, &workspace))
                .await
                .map_err(|e| anyhow::anyhow!("PDF task panicked: {}", e))??;

        Ok(result)
    }
}

// ─── Dispatch ─────────────────────────────────────────────────────────────────

fn dispatch(action: &str, input: &Value, workspace: &Path) -> Result<ToolResult> {
    match action {
        "read_text" => read_text(input, workspace),
        "get_info" => get_info(input, workspace),
        "extract_images" => extract_images(input, workspace),
        "merge" => merge(input, workspace),
        "split" => split(input, workspace),
        "add_watermark" => add_watermark(input, workspace),
        "encrypt" => encrypt(input, workspace),
        "decrypt" => decrypt(input, workspace),
        "fill_form" => fill_form(input, workspace),
        "add_annotation" => add_annotation(input, workspace),
        "to_images" => to_images(input, workspace),
        "render_page_image" => render_page_image(input, workspace),
        "render_region_image" => render_region_image(input, workspace),
        other => Ok(ToolResult::err(format!("Unknown action: {}", other))),
    }
}

// ─── Path helpers ─────────────────────────────────────────────────────────────

fn resolve(raw: &str, workspace: &Path) -> PathBuf {
    let p = Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace.join(p)
    }
}

fn require_path(input: &Value, workspace: &Path) -> Result<PathBuf, ToolResult> {
    match input["path"].as_str() {
        Some(s) => Ok(resolve(s, workspace)),
        None => Err(ToolResult::err("Missing required parameter: path")),
    }
}

fn require_output(input: &Value, workspace: &Path) -> Result<PathBuf, ToolResult> {
    match input["output"].as_str() {
        Some(s) => Ok(resolve(s, workspace)),
        None => Err(ToolResult::err("Missing required parameter: output")),
    }
}

// Unwrap a Result<ToolResult, ToolResult> — both arms are ToolResult
macro_rules! try_tr {
    ($e:expr) => {
        match $e {
            Ok(v) => v,
            Err(tr) => return Ok(tr),
        }
    };
}

// ─── read_text ────────────────────────────────────────────────────────────────

fn read_text(input: &Value, workspace: &Path) -> Result<ToolResult> {
    let path = try_tr!(require_path(input, workspace));
    if !path.exists() {
        return Ok(ToolResult::err(format!(
            "File not found: {}",
            path.display()
        )));
    }

    let bytes = std::fs::read(&path)?;

    // Collect requested page numbers (1-based); empty = all pages
    let page_filter: Vec<u32> = input["pages"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_u64().map(|n| n as u32))
                .collect()
        })
        .unwrap_or_default();

    let text = pdf_extract::extract_text_from_mem(&bytes)
        .unwrap_or_else(|e| format!("[text extraction failed: {}]", e));

    // If specific pages requested, try to split by form-feed or page markers.
    // pdf-extract returns a flat string; we do a best-effort page split on \x0C (form feed).
    let output = if page_filter.is_empty() {
        text
    } else {
        let pages: Vec<&str> = text.split('\x0C').collect();
        let mut parts = Vec::new();
        for &pg in &page_filter {
            let idx = (pg as usize).saturating_sub(1);
            if let Some(content) = pages.get(idx) {
                parts.push(format!("=== Page {} ===\n{}", pg, content.trim()));
            } else {
                parts.push(format!("=== Page {} === [page out of range]", pg));
            }
        }
        parts.join("\n\n")
    };

    let char_count = output.chars().count();
    let note = if char_count == 0 {
        "\n[Note: no text extracted — the PDF may be image-based or use unsupported fonts]"
    } else {
        ""
    };

    Ok(ToolResult::ok(format!(
        "PDF: {} — extracted {} characters{}\n\n{}",
        path.display(),
        char_count,
        note,
        output
    )))
}

// ─── get_info ─────────────────────────────────────────────────────────────────

fn get_info(input: &Value, workspace: &Path) -> Result<ToolResult> {
    use lopdf::Document;

    let path = try_tr!(require_path(input, workspace));
    if !path.exists() {
        return Ok(ToolResult::err(format!(
            "File not found: {}",
            path.display()
        )));
    }

    let doc = match Document::load(&path) {
        Ok(d) => d,
        Err(e) => return Ok(ToolResult::err(format!("Failed to open PDF: {}", e))),
    };

    let page_count = doc.get_pages().len();
    let encrypted = doc.is_encrypted();

    // Try to read Info dictionary
    let mut title = String::new();
    let mut author = String::new();
    let mut subject = String::new();
    let mut creator = String::new();
    let mut producer = String::new();
    let mut creation_date = String::new();

    if let Ok(info_id) = doc.trailer.get(b"Info") {
        if let Ok(lopdf::Object::Dictionary(info_dict)) =
            doc.get_object(info_id.as_reference().unwrap_or((0, 0)))
        {
            let get_str = |dict: &lopdf::Dictionary, key: &[u8]| -> String {
                dict.get(key)
                    .ok()
                    .and_then(|obj| match obj {
                        lopdf::Object::String(bytes, _) => {
                            String::from_utf8_lossy(bytes).into_owned().into()
                        }
                        _ => None,
                    })
                    .unwrap_or_default()
            };
            title = get_str(info_dict, b"Title");
            author = get_str(info_dict, b"Author");
            subject = get_str(info_dict, b"Subject");
            creator = get_str(info_dict, b"Creator");
            producer = get_str(info_dict, b"Producer");
            creation_date = get_str(info_dict, b"CreationDate");
        }
    }

    let file_size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);

    Ok(ToolResult::ok(format!(
        "PDF Info: {}\n\
         Pages:         {}\n\
         File size:     {} bytes\n\
         Encrypted:     {}\n\
         Title:         {}\n\
         Author:        {}\n\
         Subject:       {}\n\
         Creator:       {}\n\
         Producer:      {}\n\
         Creation date: {}",
        path.display(),
        page_count,
        file_size,
        encrypted,
        if title.is_empty() { "(none)" } else { &title },
        if author.is_empty() { "(none)" } else { &author },
        if subject.is_empty() {
            "(none)"
        } else {
            &subject
        },
        if creator.is_empty() {
            "(none)"
        } else {
            &creator
        },
        if producer.is_empty() {
            "(none)"
        } else {
            &producer
        },
        if creation_date.is_empty() {
            "(none)"
        } else {
            &creation_date
        },
    )))
}

// ─── extract_images ───────────────────────────────────────────────────────────

fn extract_images(input: &Value, workspace: &Path) -> Result<ToolResult> {
    use lopdf::{Document, Object};

    let path = try_tr!(require_path(input, workspace));
    if !path.exists() {
        return Ok(ToolResult::err(format!(
            "File not found: {}",
            path.display()
        )));
    }
    let out_dir = match input["output_dir"].as_str() {
        Some(s) => resolve(s, workspace),
        None => return Ok(ToolResult::err("Missing required parameter: output_dir")),
    };
    std::fs::create_dir_all(&out_dir)?;

    let doc = match Document::load(&path) {
        Ok(d) => d,
        Err(e) => return Ok(ToolResult::err(format!("Failed to open PDF: {}", e))),
    };

    let mut count = 0usize;
    let mut saved = Vec::new();

    for (obj_id, obj) in doc.objects.iter() {
        if let Object::Stream(stream) = obj {
            let subtype = stream
                .dict
                .get(b"Subtype")
                .ok()
                .and_then(|o| o.as_name_str().ok())
                .unwrap_or("");
            if subtype != "Image" {
                continue;
            }

            let filter = stream
                .dict
                .get(b"Filter")
                .ok()
                .and_then(|o| o.as_name_str().ok())
                .unwrap_or("");

            let ext = match filter {
                "DCTDecode" => "jpg",
                "JPXDecode" => "jp2",
                "FlateDecode" => "png",
                _ => "bin",
            };

            let data = match stream.decompressed_content() {
                Ok(d) => d,
                Err(_) => stream.content.clone(),
            };

            let file_name = format!("img_{}_{}_{}.{}", obj_id.0, obj_id.1, count, ext);
            let out_path = out_dir.join(&file_name);
            std::fs::write(&out_path, &data)?;
            saved.push(file_name);
            count += 1;
        }
    }

    if count == 0 {
        Ok(ToolResult::ok(format!(
            "No embedded images found in {}",
            path.display()
        )))
    } else {
        Ok(ToolResult::ok(format!(
            "Extracted {} image(s) to {}:\n{}",
            count,
            out_dir.display(),
            saved.join("\n")
        )))
    }
}

// ─── merge ────────────────────────────────────────────────────────────────────

fn merge(input: &Value, workspace: &Path) -> Result<ToolResult> {
    let files = match input["files"].as_array() {
        Some(arr) if !arr.is_empty() => arr,
        _ => return Ok(ToolResult::err("Missing or empty parameter: files")),
    };
    let output = try_tr!(require_output(input, workspace));

    let paths: Vec<PathBuf> = files
        .iter()
        .filter_map(|v| v.as_str())
        .map(|s| resolve(s, workspace))
        .collect();

    if paths.len() < 2 {
        return Ok(ToolResult::err("merge requires at least 2 files"));
    }

    for p in &paths {
        if !p.exists() {
            return Ok(ToolResult::err(format!("File not found: {}", p.display())));
        }
    }

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Use Ghostscript to merge PDFs
    let gs = which_tool("gswin64c")
        .or_else(|| which_tool("gswin32c"))
        .or_else(|| which_tool("gs"));

    if let Some(gs_path) = gs {
        let mut args = vec![
            "-dNOPAUSE".to_string(),
            "-dBATCH".to_string(),
            "-dSAFER".to_string(),
            "-sDEVICE=pdfwrite".to_string(),
            format!("-sOutputFile={}", output.to_str().unwrap_or("")),
        ];
        for p in &paths {
            args.push(p.to_str().unwrap_or("").to_string());
        }

        let result = std::process::Command::new(&gs_path).args(&args).output();

        match result {
            Ok(out) if out.status.success() => {
                return Ok(ToolResult::ok(format!(
                    "Merged {} PDFs into {}",
                    paths.len(),
                    output.display()
                )));
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                return Ok(ToolResult::err(format!(
                    "Ghostscript merge failed: {}",
                    stderr
                )));
            }
            Err(e) => return Ok(ToolResult::err(format!("Failed to run Ghostscript: {}", e))),
        }
    }

    // Try pdftk as fallback
    if let Some(pdftk) = which_tool("pdftk") {
        let mut args: Vec<String> = paths
            .iter()
            .map(|p| p.to_str().unwrap_or("").to_string())
            .collect();
        args.push("cat".to_string());
        args.push("output".to_string());
        args.push(output.to_str().unwrap_or("").to_string());

        let result = std::process::Command::new(&pdftk).args(&args).output();

        match result {
            Ok(out) if out.status.success() => {
                return Ok(ToolResult::ok(format!(
                    "Merged {} PDFs into {}",
                    paths.len(),
                    output.display()
                )));
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                return Ok(ToolResult::err(format!("pdftk merge failed: {}", stderr)));
            }
            Err(e) => return Ok(ToolResult::err(format!("Failed to run pdftk: {}", e))),
        }
    }

    Ok(ToolResult::err(
        "merge requires Ghostscript or pdftk to be installed.\n\
         Install options:\n\
         - Windows: https://www.ghostscript.com/download.html  or  choco install pdftk-server\n\
         - Linux:   sudo apt install ghostscript  or  sudo apt install pdftk\n\
         - macOS:   brew install ghostscript  or  brew install pdftk-java",
    ))
}

// ─── split ────────────────────────────────────────────────────────────────────

fn split(input: &Value, workspace: &Path) -> Result<ToolResult> {
    use lopdf::Document;

    let path = try_tr!(require_path(input, workspace));
    if !path.exists() {
        return Ok(ToolResult::err(format!(
            "File not found: {}",
            path.display()
        )));
    }
    let out_dir = match input["output_dir"].as_str() {
        Some(s) => resolve(s, workspace),
        None => return Ok(ToolResult::err("Missing required parameter: output_dir")),
    };
    let ranges = match input["ranges"].as_array() {
        Some(r) if !r.is_empty() => r,
        _ => return Ok(ToolResult::err("Missing or empty parameter: ranges")),
    };

    std::fs::create_dir_all(&out_dir)?;

    let doc = match Document::load(&path) {
        Ok(d) => d,
        Err(e) => return Ok(ToolResult::err(format!("Failed to open PDF: {}", e))),
    };

    let total_pages = doc.get_pages().len() as u32;
    let mut saved = Vec::new();

    for (i, range) in ranges.iter().enumerate() {
        let arr = match range.as_array() {
            Some(a) if a.len() == 2 => a,
            _ => {
                return Ok(ToolResult::err(format!(
                    "ranges[{}] must be [start, end]",
                    i
                )))
            }
        };
        let start = arr[0].as_u64().unwrap_or(1) as u32;
        let end = arr[1].as_u64().unwrap_or(1) as u32;

        if start < 1 || end < start || end > total_pages {
            return Ok(ToolResult::err(format!(
                "Invalid range [{},{}]: PDF has {} pages",
                start, end, total_pages
            )));
        }

        let mut part = doc.clone();
        // Delete pages outside the range
        let pages_to_delete: Vec<u32> = (1..=total_pages)
            .filter(|&p| p < start || p > end)
            .collect();
        part.delete_pages(&pages_to_delete);

        let file_name = format!("part_{:02}_{}-{}.pdf", i + 1, start, end);
        let out_path = out_dir.join(&file_name);
        part.save(&out_path)
            .map_err(|e| anyhow::anyhow!("Failed to save part: {}", e))?;
        saved.push(file_name);
    }

    Ok(ToolResult::ok(format!(
        "Split {} into {} part(s) in {}:\n{}",
        path.display(),
        saved.len(),
        out_dir.display(),
        saved.join("\n")
    )))
}

// ─── add_watermark ────────────────────────────────────────────────────────────

fn add_watermark(input: &Value, workspace: &Path) -> Result<ToolResult> {
    use lopdf::{Dictionary, Document, Object, Stream};

    let path = try_tr!(require_path(input, workspace));
    if !path.exists() {
        return Ok(ToolResult::err(format!(
            "File not found: {}",
            path.display()
        )));
    }
    let output = try_tr!(require_output(input, workspace));
    let text = match input["text"].as_str() {
        Some(t) if !t.is_empty() => t,
        _ => return Ok(ToolResult::err("Missing required parameter: text")),
    };

    let mut doc = match Document::load(&path) {
        Ok(d) => d,
        Err(e) => return Ok(ToolResult::err(format!("Failed to open PDF: {}", e))),
    };

    let pages: Vec<u32> = doc.get_pages().keys().copied().collect();
    let page_count = pages.len();

    for page_num in &pages {
        // Build a simple watermark content stream:
        // - Set gray color, rotate 45°, draw text centered on page
        let content = format!(
            "q\n\
             0.5 g\n\
             BT\n\
             /Helvetica 48 Tf\n\
             1 0 0 1 150 400 Tm\n\
             0.5 0.866 -0.866 0.5 200 300 Tm\n\
             ({}) Tj\n\
             ET\n\
             Q\n",
            text.replace('(', "\\(").replace(')', "\\)")
        );

        let wm_stream = Stream::new(Dictionary::new(), content.into_bytes());
        let wm_id = doc.add_object(Object::Stream(wm_stream));

        // Append the watermark stream to the page's content
        if let Ok(page_id) = doc.get_pages().get(page_num).copied().ok_or(()) {
            if let Ok(Object::Dictionary(dict)) = doc.get_object_mut(page_id) {
                match dict.get_mut(b"Contents") {
                        Ok(contents) => {
                            let existing = contents.clone();
                            *contents = match existing {
                                Object::Array(mut arr) => {
                                    arr.push(Object::Reference(wm_id));
                                    Object::Array(arr)
                                }
                                Object::Reference(r) => Object::Array(vec![
                                    Object::Reference(r),
                                    Object::Reference(wm_id),
                                ]),
                                _ => Object::Array(vec![Object::Reference(wm_id)]),
                            };
                        }
                        Err(_) => {
                            dict.set(b"Contents", Object::Reference(wm_id));
                        }
                    }
            }
        }
    }

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    doc.save(&output)
        .map_err(|e| anyhow::anyhow!("Failed to save watermarked PDF: {}", e))?;

    Ok(ToolResult::ok(format!(
        "Added watermark \"{}\" to {} page(s), saved to {}",
        text,
        page_count,
        output.display()
    )))
}

// ─── encrypt ──────────────────────────────────────────────────────────────────

fn encrypt(input: &Value, workspace: &Path) -> Result<ToolResult> {
    use lopdf::Document;

    let path = try_tr!(require_path(input, workspace));
    if !path.exists() {
        return Ok(ToolResult::err(format!(
            "File not found: {}",
            path.display()
        )));
    }
    let output = try_tr!(require_output(input, workspace));
    let user_pw = input["user_password"].as_str().unwrap_or("");
    let owner_pw = input["owner_password"].as_str().unwrap_or(user_pw);

    let mut doc = match Document::load(&path) {
        Ok(d) => d,
        Err(e) => return Ok(ToolResult::err(format!("Failed to open PDF: {}", e))),
    };

    // lopdf 0.34 does not expose a public encrypt API; we save as-is and note the limitation.
    let _ = (user_pw, owner_pw);
    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    doc.save(&output)
        .map_err(|e| anyhow::anyhow!("Failed to save PDF: {}", e))?;

    Ok(ToolResult::ok(format!(
        "Note: PDF encryption is not supported by the current lopdf version. \
         File copied to {} without encryption.",
        output.display()
    )))
}

// ─── decrypt ──────────────────────────────────────────────────────────────────

fn decrypt(input: &Value, workspace: &Path) -> Result<ToolResult> {
    use lopdf::Document;

    let path = try_tr!(require_path(input, workspace));
    if !path.exists() {
        return Ok(ToolResult::err(format!(
            "File not found: {}",
            path.display()
        )));
    }
    let output = try_tr!(require_output(input, workspace));
    let password = input["password"].as_str().unwrap_or("");

    let mut doc = match Document::load_filtered(&path, |id, obj| Some((id, obj.to_owned()))) {
        Ok(d) => d,
        Err(e) => return Ok(ToolResult::err(format!("Failed to open PDF: {}", e))),
    };

    if doc.is_encrypted() {
        doc.decrypt(password)
            .map_err(|e| anyhow::anyhow!("Decryption failed (wrong password?): {}", e))?;
    }

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    doc.save(&output)
        .map_err(|e| anyhow::anyhow!("Failed to save decrypted PDF: {}", e))?;

    Ok(ToolResult::ok(format!(
        "Decrypted PDF saved to {}",
        output.display()
    )))
}

// ─── fill_form ────────────────────────────────────────────────────────────────

fn fill_form(input: &Value, workspace: &Path) -> Result<ToolResult> {
    use lopdf::{Document, Object, StringFormat};

    let path = try_tr!(require_path(input, workspace));
    if !path.exists() {
        return Ok(ToolResult::err(format!(
            "File not found: {}",
            path.display()
        )));
    }
    let output = try_tr!(require_output(input, workspace));
    let fields = match input["fields"].as_object() {
        Some(f) if !f.is_empty() => f,
        _ => return Ok(ToolResult::err("Missing or empty parameter: fields")),
    };

    let mut doc = match Document::load(&path) {
        Ok(d) => d,
        Err(e) => return Ok(ToolResult::err(format!("Failed to open PDF: {}", e))),
    };

    let mut filled = 0usize;
    let mut not_found: Vec<String> = Vec::new();

    // Collect all widget annotation object IDs and their field names
    let field_ids: Vec<(lopdf::ObjectId, String)> = doc
        .objects
        .iter()
        .filter_map(|(id, obj)| {
            if let lopdf::Object::Dictionary(dict) = obj {
                let ft = dict.get(b"FT").ok()?.as_name_str().ok()?;
                if ft != "Tx" {
                    return None;
                }
                let name_bytes = dict.get(b"T").ok()?;
                if let lopdf::Object::String(bytes, _) = name_bytes {
                    let name = String::from_utf8_lossy(bytes).into_owned();
                    Some((*id, name))
                } else {
                    None
                }
            } else {
                None
            }
        })
        .collect();

    for (field_name, value) in fields {
        let value_str = value.as_str().unwrap_or("");
        let matched = field_ids.iter().find(|(_, n)| n == field_name);
        match matched {
            Some((obj_id, _)) => {
                if let Ok(lopdf::Object::Dictionary(dict)) = doc.get_object_mut(*obj_id) {
                    dict.set(
                        b"V",
                        Object::String(value_str.as_bytes().to_vec(), StringFormat::Literal),
                    );
                    filled += 1;
                }
            }
            None => not_found.push(field_name.clone()),
        }
    }

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    doc.save(&output)
        .map_err(|e| anyhow::anyhow!("Failed to save filled PDF: {}", e))?;

    let mut msg = format!("Filled {} field(s), saved to {}", filled, output.display());
    if !not_found.is_empty() {
        msg.push_str(&format!("\nFields not found: {}", not_found.join(", ")));
    }
    Ok(ToolResult::ok(msg))
}

// ─── add_annotation ───────────────────────────────────────────────────────────

fn add_annotation(input: &Value, workspace: &Path) -> Result<ToolResult> {
    use lopdf::{Dictionary, Document, Object, StringFormat};

    let path = try_tr!(require_path(input, workspace));
    if !path.exists() {
        return Ok(ToolResult::err(format!(
            "File not found: {}",
            path.display()
        )));
    }
    let output = try_tr!(require_output(input, workspace));
    let page_num = input["page"].as_u64().unwrap_or(1) as u32;
    let text = match input["text"].as_str() {
        Some(t) => t,
        None => return Ok(ToolResult::err("Missing required parameter: text")),
    };
    let rect = input["rect"]
        .as_array()
        .and_then(|arr| {
            if arr.len() == 4 {
                Some([
                    arr[0].as_f64().unwrap_or(50.0),
                    arr[1].as_f64().unwrap_or(700.0),
                    arr[2].as_f64().unwrap_or(300.0),
                    arr[3].as_f64().unwrap_or(750.0),
                ])
            } else {
                None
            }
        })
        .unwrap_or([50.0, 700.0, 300.0, 750.0]);

    let mut doc = match Document::load(&path) {
        Ok(d) => d,
        Err(e) => return Ok(ToolResult::err(format!("Failed to open PDF: {}", e))),
    };

    let page_id = match doc.get_pages().get(&page_num).copied() {
        Some(id) => id,
        None => {
            return Ok(ToolResult::err(format!(
                "Page {} not found (PDF has {} pages)",
                page_num,
                doc.get_pages().len()
            )))
        }
    };

    let mut annot = Dictionary::new();
    annot.set(b"Type", Object::Name(b"Annot".to_vec()));
    annot.set(b"Subtype", Object::Name(b"Text".to_vec()));
    annot.set(
        b"Rect",
        Object::Array(vec![
            Object::Real(rect[0] as f32),
            Object::Real(rect[1] as f32),
            Object::Real(rect[2] as f32),
            Object::Real(rect[3] as f32),
        ]),
    );
    annot.set(
        b"Contents",
        Object::String(text.as_bytes().to_vec(), StringFormat::Literal),
    );
    annot.set(b"Open", Object::Boolean(false));

    let annot_id = doc.add_object(Object::Dictionary(annot));

    // Add annotation reference to the page's Annots array
    if let Ok(Object::Dictionary(page_dict)) = doc.get_object_mut(page_id) {
        match page_dict.get_mut(b"Annots") {
            Ok(annots) => {
                if let Object::Array(arr) = annots {
                    arr.push(Object::Reference(annot_id));
                }
            }
            Err(_) => {
                page_dict.set(b"Annots", Object::Array(vec![Object::Reference(annot_id)]));
            }
        }
    }

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent)?;
    }
    doc.save(&output)
        .map_err(|e| anyhow::anyhow!("Failed to save annotated PDF: {}", e))?;

    Ok(ToolResult::ok(format!(
        "Added annotation on page {}, saved to {}",
        page_num,
        output.display()
    )))
}

// ─── to_images ────────────────────────────────────────────────────────────────

fn to_images(input: &Value, workspace: &Path) -> Result<ToolResult> {
    let path = try_tr!(require_path(input, workspace));
    if !path.exists() {
        return Ok(ToolResult::err(format!(
            "File not found: {}",
            path.display()
        )));
    }
    let out_dir = match input["output_dir"].as_str() {
        Some(s) => resolve(s, workspace),
        None => return Ok(ToolResult::err("Missing required parameter: output_dir")),
    };
    let dpi = input["dpi"].as_u64().unwrap_or(150) as u32;

    std::fs::create_dir_all(&out_dir)?;

    // Try pdftoppm (poppler) first, then Ghostscript
    let pdftoppm = which_tool("pdftoppm");
    let gs = which_tool("gswin64c")
        .or_else(|| which_tool("gswin32c"))
        .or_else(|| which_tool("gs"));

    if let Some(pdftoppm_path) = pdftoppm {
        let prefix = out_dir.join("page");
        let output = std::process::Command::new(&pdftoppm_path)
            .args([
                "-r",
                &dpi.to_string(),
                "-png",
                path.to_str().unwrap_or(""),
                prefix.to_str().unwrap_or("page"),
            ])
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let files: Vec<_> = std::fs::read_dir(&out_dir)?
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().map(|x| x == "png").unwrap_or(false))
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                return Ok(ToolResult::ok(format!(
                    "Rendered {} page image(s) to {} at {} DPI:\n{}",
                    files.len(),
                    out_dir.display(),
                    dpi,
                    files.join("\n")
                )));
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                return Ok(ToolResult::err(format!("pdftoppm failed: {}", stderr)));
            }
            Err(e) => return Ok(ToolResult::err(format!("Failed to run pdftoppm: {}", e))),
        }
    }

    if let Some(gs_path) = gs {
        let out_pattern = out_dir.join("page-%04d.png");
        let output = std::process::Command::new(&gs_path)
            .args([
                "-dNOPAUSE",
                "-dBATCH",
                "-dSAFER",
                "-sDEVICE=png16m",
                &format!("-r{}", dpi),
                &format!("-sOutputFile={}", out_pattern.to_str().unwrap_or("")),
                path.to_str().unwrap_or(""),
            ])
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let files: Vec<_> = std::fs::read_dir(&out_dir)?
                    .filter_map(|e| e.ok())
                    .filter(|e| e.path().extension().map(|x| x == "png").unwrap_or(false))
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                return Ok(ToolResult::ok(format!(
                    "Rendered {} page image(s) to {} at {} DPI:\n{}",
                    files.len(),
                    out_dir.display(),
                    dpi,
                    files.join("\n")
                )));
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                return Ok(ToolResult::err(format!("Ghostscript failed: {}", stderr)));
            }
            Err(e) => return Ok(ToolResult::err(format!("Failed to run Ghostscript: {}", e))),
        }
    }

    Ok(ToolResult::err(
        "to_images requires either pdftoppm (poppler-utils) or Ghostscript to be installed.\n\
         Install options:\n\
         - Windows: https://www.ghostscript.com/download.html  or  choco install poppler\n\
         - Linux:   sudo apt install poppler-utils\n\
         - macOS:   brew install poppler",
    ))
}

fn render_page_image(input: &Value, workspace: &Path) -> Result<ToolResult> {
    let path = try_tr!(require_path(input, workspace));
    if !path.exists() {
        return Ok(ToolResult::err(format!(
            "File not found: {}",
            path.display()
        )));
    }
    let page = input["page"].as_u64().unwrap_or(1) as u32;
    if page == 0 {
        return Ok(ToolResult::err("Parameter 'page' must be >= 1"));
    }
    let dpi = input["dpi"].as_u64().unwrap_or(150) as u32;
    let bytes = match render_pdf_page_png_bytes(&path, page, dpi) {
        Ok(v) => v,
        Err(e) => return Ok(ToolResult::err(e.to_string())),
    };
    let img = match image::load_from_memory(&bytes) {
        Ok(v) => v,
        Err(e) => {
            return Ok(ToolResult::err(format!(
                "Failed to decode rendered page image: {}",
                e
            )))
        }
    };
    let (width, height) = img.dimensions();
    let b64 = base64::engine::general_purpose::STANDARD.encode(bytes);
    Ok(ToolResult::ok(format!(
        "Rendered page {} from {} at {} DPI as an inline image ({}x{} px).",
        page,
        path.display(),
        dpi,
        width,
        height
    ))
    .with_image(crate::agent::tool::ImageData::png(b64)))
}

fn render_region_image(input: &Value, workspace: &Path) -> Result<ToolResult> {
    let path = try_tr!(require_path(input, workspace));
    if !path.exists() {
        return Ok(ToolResult::err(format!(
            "File not found: {}",
            path.display()
        )));
    }
    let page = input["page"].as_u64().unwrap_or(1) as u32;
    if page == 0 {
        return Ok(ToolResult::err("Parameter 'page' must be >= 1"));
    }
    let dpi = input["dpi"].as_u64().unwrap_or(150) as u32;
    let rect_vals: Vec<u32> = match input["crop_rect"].as_array() {
        Some(arr) if arr.len() == 4 => arr
            .iter()
            .filter_map(|v| v.as_u64().map(|n| n as u32))
            .collect(),
        _ => {
            return Ok(ToolResult::err(
                "Missing required parameter: crop_rect [left, top, width, height]",
            ))
        }
    };
    if rect_vals.len() != 4 || rect_vals[2] == 0 || rect_vals[3] == 0 {
        return Ok(ToolResult::err(
            "Invalid crop_rect. Expected [left, top, width, height] with width/height > 0",
        ));
    }
    let bytes = match render_pdf_page_png_bytes(&path, page, dpi) {
        Ok(v) => v,
        Err(e) => return Ok(ToolResult::err(e.to_string())),
    };
    let img = match image::load_from_memory(&bytes) {
        Ok(v) => v,
        Err(e) => {
            return Ok(ToolResult::err(format!(
                "Failed to decode rendered page image: {}",
                e
            )))
        }
    };
    let (img_w, img_h) = img.dimensions();
    let left = rect_vals[0];
    let top = rect_vals[1];
    let width = rect_vals[2];
    let height = rect_vals[3];
    if left >= img_w
        || top >= img_h
        || left.saturating_add(width) > img_w
        || top.saturating_add(height) > img_h
    {
        return Ok(ToolResult::err(format!(
            "crop_rect exceeds rendered page bounds. Page size is {}x{} px.",
            img_w, img_h
        )));
    }
    let cropped = img.crop_imm(left, top, width, height);
    let mut out = Cursor::new(Vec::new());
    if let Err(e) = cropped.write_to(&mut out, image::ImageFormat::Png) {
        return Ok(ToolResult::err(format!(
            "Failed to encode cropped region: {}",
            e
        )));
    }
    let png_bytes = out.into_inner();
    let b64 = base64::engine::general_purpose::STANDARD.encode(png_bytes);
    Ok(ToolResult::ok(format!(
        "Rendered page {} from {} at {} DPI and cropped region [{}, {}, {}, {}] into an inline image.",
        page,
        path.display(),
        dpi,
        left,
        top,
        width,
        height
    ))
    .with_image(crate::agent::tool::ImageData::png(b64)))
}

fn render_pdf_page_png_bytes(path: &Path, page: u32, dpi: u32) -> anyhow::Result<Vec<u8>> {
    let temp_dir =
        std::env::temp_dir().join(format!("pisci_pdf_{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&temp_dir)?;

    let result = (|| -> anyhow::Result<Vec<u8>> {
        let pdftoppm = which_tool("pdftoppm");
        let gs = which_tool("gswin64c")
            .or_else(|| which_tool("gswin32c"))
            .or_else(|| which_tool("gs"));

        if let Some(pdftoppm_path) = pdftoppm {
            let prefix = temp_dir.join("page");
            let output = std::process::Command::new(&pdftoppm_path)
                .args([
                    "-f",
                    &page.to_string(),
                    "-l",
                    &page.to_string(),
                    "-r",
                    &dpi.to_string(),
                    "-png",
                    path.to_str().unwrap_or(""),
                    prefix.to_str().unwrap_or("page"),
                ])
                .output()?;
            if !output.status.success() {
                return Err(anyhow::anyhow!(
                    "pdftoppm failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            let png = std::fs::read_dir(&temp_dir)?
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .find(|p| p.extension().map(|x| x == "png").unwrap_or(false))
                .ok_or_else(|| anyhow::anyhow!("No PNG file produced for page {}", page))?;
            return Ok(std::fs::read(png)?);
        }

        if let Some(gs_path) = gs {
            let out_file = temp_dir.join("page.png");
            let output = std::process::Command::new(&gs_path)
                .args([
                    "-dNOPAUSE",
                    "-dBATCH",
                    "-dSAFER",
                    "-sDEVICE=png16m",
                    &format!("-r{}", dpi),
                    &format!("-dFirstPage={}", page),
                    &format!("-dLastPage={}", page),
                    &format!("-sOutputFile={}", out_file.to_str().unwrap_or("")),
                    path.to_str().unwrap_or(""),
                ])
                .output()?;
            if !output.status.success() {
                return Err(anyhow::anyhow!(
                    "Ghostscript failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            if !out_file.exists() {
                return Err(anyhow::anyhow!("No PNG file produced for page {}", page));
            }
            return Ok(std::fs::read(out_file)?);
        }

        Err(anyhow::anyhow!(
            "Rendering PDF pages requires either pdftoppm (poppler-utils) or Ghostscript to be installed."
        ))
    })();

    let _ = std::fs::remove_dir_all(&temp_dir);
    result
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn which_tool(name: &str) -> Option<PathBuf> {
    // Check PATH for the tool
    if let Ok(output) = std::process::Command::new("where").arg(name).output() {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout);
            let first = path_str.lines().next()?.trim().to_string();
            return Some(PathBuf::from(first));
        }
    }
    // Unix fallback
    if let Ok(output) = std::process::Command::new("which").arg(name).output() {
        if output.status.success() {
            let path_str = String::from_utf8_lossy(&output.stdout);
            let first = path_str.lines().next()?.trim().to_string();
            return Some(PathBuf::from(first));
        }
    }
    None
}

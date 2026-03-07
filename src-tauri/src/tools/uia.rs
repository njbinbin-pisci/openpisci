/// Windows UI Automation tool (Windows only)
/// Supports 25+ actions for element interaction, keyboard control, and window management.
use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct UiaTool;

#[async_trait]
impl Tool for UiaTool {
    fn name(&self) -> &str { "uia" }

    fn description(&self) -> &str {
        "Control Windows desktop applications via UI Automation (UIA). \
         Supports finding controls, clicking, typing, keyboard shortcuts, scrolling, drag-drop, window management. \
         \
         Recommended workflow: \
         1. list_windows — find the target window title \
         2. find (with window_title) — locate a specific control by name/type \
         3. click / type / send_hotkey — interact with the control \
         \
         When find fails (element not found): \
         → Use screen_capture to take a screenshot and visually locate the element \
         → Then use click with x/y coordinates instead of name-based find \
         → Or use smart_find which tries fuzzy matching \
         \
         Tips: \
         - Always specify window_title to narrow the search scope \
         - Use annotate_elements to get a labeled map of all controls in a window \
         - For text input fields, use 'type' action (not 'click' then 'type') \
         - send_hotkey supports: ctrl+c, ctrl+v, alt+f4, win+d, ctrl+shift+esc, etc."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "list_windows", "find", "get_children", "get_rect", "get_value",
                        "click", "double_click", "right_click", "hover",
                        "type", "send_hotkey", "send_keys",
                        "get_text", "scroll", "drag_drop",
                        "expand", "collapse", "select", "check", "uncheck",
                        "wait_for_element",
                        "activate_window", "minimize", "maximize", "restore",
                        "close_window", "move_window", "resize_window", "get_window_rect",
                        "smart_find", "annotate_elements"
                    ],
                    "description": "Action to perform"
                },
                "name": {
                    "type": "string",
                    "description": "Control name to search for"
                },
                "class_name": {
                    "type": "string",
                    "description": "Control class name"
                },
                "automation_id": {
                    "type": "string",
                    "description": "Control automation ID"
                },
                "control_type": {
                    "type": "string",
                    "description": "Control type filter (e.g. Button, Edit, ListItem, CheckBox, ComboBox, TreeItem)"
                },
                "window_title": {
                    "type": "string",
                    "description": "Limit search to children of a specific window by title"
                },
                "text": {
                    "type": "string",
                    "description": "Text to type (for 'type' action) or option to select (for 'select' action)"
                },
                "hotkey": {
                    "type": "string",
                    "description": "Hotkey combination for 'send_hotkey' (e.g. 'ctrl+c', 'alt+f4', 'win+d')"
                },
                "keys": {
                    "type": "string",
                    "description": "Key sequence for 'send_keys' (e.g. '{Enter}', '{Tab}', '{Escape}')"
                },
                "x": {
                    "type": "integer",
                    "description": "X coordinate (for click by coords, drag start, or window move)"
                },
                "y": {
                    "type": "integer",
                    "description": "Y coordinate (for click by coords, drag start, or window move)"
                },
                "x2": {
                    "type": "integer",
                    "description": "Target X coordinate (for drag_drop end, or window resize width)"
                },
                "y2": {
                    "type": "integer",
                    "description": "Target Y coordinate (for drag_drop end, or window resize height)"
                },
                "direction": {
                    "type": "string",
                    "enum": ["up", "down", "left", "right"],
                    "description": "Scroll direction"
                },
                "amount": {
                    "type": "integer",
                    "description": "Scroll amount (number of ticks, default 3)"
                },
                "timeout_ms": {
                    "type": "integer",
                    "description": "Timeout in milliseconds for wait_for_element (default 10000)"
                },
                "depth": {
                    "type": "integer",
                    "description": "Depth for get_children traversal (default 2, max 5)"
                },
                "description": {
                    "type": "string",
                    "description": "Natural language description of the element to find (for smart_find)"
                },
                "max_elements": {
                    "type": "integer",
                    "description": "Maximum number of elements to annotate (for annotate_elements, default 30)"
                },
                "logical_coords": {
                    "type": "boolean",
                    "description": "If true, x/y/x2/y2 are treated as logical (DPI-unaware) pixel coordinates from a screenshot and will be automatically multiplied by the DPI scale factor to get physical screen coordinates. Use this when coordinates come from screen_capture image analysis."
                }
            },
            "required": ["action"]
        })
    }

    async fn call(&self, input: Value, ctx: &ToolContext) -> Result<ToolResult> {
        let action = match input["action"].as_str() {
            Some(a) => a,
            None => return Ok(ToolResult::err("Missing required parameter: action")),
        };

        match action {
            // Discovery
            "list_windows"    => self.list_windows(),
            "find"            => self.find_element(&input),
            "get_children"    => self.get_children(&input),
            "get_rect"        => self.get_rect(&input),
            "get_value"       => self.get_value(&input),
            "get_text"        => self.get_text(&input),
            // Mouse actions
            "click"           => self.click_element(&input),
            "double_click"    => self.double_click_element(&input),
            "right_click"     => self.right_click_element(&input),
            "hover"           => self.hover_element(&input),
            "scroll"          => self.scroll_element(&input),
            "drag_drop"       => self.drag_drop(&input),
            // Keyboard actions
            "type"            => self.type_text(&input),
            "send_hotkey"     => self.send_hotkey(&input),
            "send_keys"       => self.send_keys_action(&input),
            // State actions
            "expand"          => self.expand_element(&input),
            "collapse"        => self.collapse_element(&input),
            "select"          => self.select_item(&input),
            "check"           => self.set_check(&input, true),
            "uncheck"         => self.set_check(&input, false),
            // Wait
            "wait_for_element" => self.wait_for_element(&input),
            // Window management
            "activate_window" => self.activate_window(&input),
            "minimize"        => self.window_state(&input, "minimize"),
            "maximize"        => self.window_state(&input, "maximize"),
            "restore"         => self.window_state(&input, "restore"),
            "close_window"    => self.close_window(&input),
            "move_window"     => self.move_window(&input),
            "resize_window"   => self.resize_window(&input),
            "get_window_rect" => self.get_window_rect(&input),
            // Hybrid vision automation
            "smart_find"        => self.smart_find(&input, ctx).await,
            "annotate_elements" => self.annotate_elements(&input),
            _ => Ok(ToolResult::err(format!("Unknown action: {}", action))),
        }
    }
}

// ─── Helper: build a matcher from common search params ───────────────────────

impl UiaTool {
    fn build_matcher(
        &self,
        automation: &uiautomation::UIAutomation,
        root: uiautomation::UIElement,
        input: &Value,
        timeout_ms: u64,
    ) -> uiautomation::UIMatcher {
        let mut matcher = automation.create_matcher().from(root).timeout(timeout_ms);
        if let Some(name) = input["name"].as_str() {
            matcher = matcher.name(name);
        }
        // automation_id is a unique control identifier (e.g. "btnOK"), distinct from name (display text)
        // UIMatcher has no built-in automation_id filter, so use filter_fn
        if let Some(aid) = input["automation_id"].as_str().map(|s| s.to_string()) {
            matcher = matcher.filter_fn(Box::new(move |el: &uiautomation::UIElement| {
                Ok(el.get_automation_id().unwrap_or_default() == aid)
            }));
        }
        if let Some(class) = input["class_name"].as_str() {
            matcher = matcher.classname(class);
        }
        // control_type filter: map common names to Win32 classnames
        if let Some(ct) = input["control_type"].as_str() {
            let classname = match ct.to_lowercase().as_str() {
                "button" => "Button",
                "edit" | "textbox" => "Edit",
                "combobox" => "ComboBox",
                "listbox" => "ListBox",
                "listview" => "SysListView32",
                "treeview" => "SysTreeView32",
                "toolbar" => "ToolbarWindow32",
                "statusbar" => "msctls_statusbar32",
                "tabcontrol" => "SysTabControl32",
                _ => ct,
            };
            matcher = matcher.classname(classname);
        }
        matcher
    }

    /// Get root element, optionally scoped to a window by title
    fn get_search_root(
        &self,
        automation: &uiautomation::UIAutomation,
        input: &Value,
    ) -> Result<uiautomation::UIElement> {
        let root = automation.get_root_element().map_err(|e| anyhow::anyhow!("{}", e))?;
        if let Some(title) = input["window_title"].as_str() {
            let walker = automation.get_control_view_walker().map_err(|e| anyhow::anyhow!("{}", e))?;
            if let Ok(child) = walker.get_first_child(&root) {
                let mut current = child;
                loop {
                    let name = current.get_name().unwrap_or_default();
                    if name.contains(title) {
                        return Ok(current);
                    }
                    match walker.get_next_sibling(&current) {
                        Ok(next) => current = next,
                        Err(_) => break,
                    }
                }
            }
            return Err(anyhow::anyhow!("Window with title containing '{}' not found", title));
        }
        Ok(root)
    }

    // ─── Discovery ───────────────────────────────────────────────────────────

    fn list_windows(&self) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = automation.get_root_element().map_err(|e| anyhow::anyhow!("{}", e))?;
        let walker = automation.get_control_view_walker().map_err(|e| anyhow::anyhow!("{}", e))?;

        let mut windows = Vec::new();
        if let Ok(child) = walker.get_first_child(&root) {
            let mut current = child;
            loop {
                let name = current.get_name().unwrap_or_default();
                let class = current.get_classname().unwrap_or_default();
                if !name.is_empty() {
                    windows.push(format!("Name: '{}', Class: '{}'", name, class));
                }
                match walker.get_next_sibling(&current) {
                    Ok(next) => current = next,
                    Err(_) => break,
                }
            }
        }

        Ok(ToolResult::ok(format!(
            "Found {} windows:\n{}",
            windows.len(),
            windows.join("\n")
        )))
    }

    fn find_element(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);

        match matcher.find_first() {
            Ok(element) => {
                let name = element.get_name().unwrap_or_default();
                let class = element.get_classname().unwrap_or_default();
                Ok(ToolResult::ok(format!(
                    "Found element: Name='{}', Class='{}'",
                    name, class
                )))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found: {}", e))),
        }
    }

    fn get_children(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;
        let depth = input["depth"].as_u64().unwrap_or(2).min(5) as usize;

        let start = if input["name"].is_null() && input["class_name"].is_null() && input["automation_id"].is_null() {
            root
        } else {
            let matcher = self.build_matcher(&automation, root, input, 5000);
            match matcher.find_first() {
                Ok(el) => el,
                Err(e) => return Ok(ToolResult::err(format!("Parent element not found: {}", e))),
            }
        };

        let walker = automation.get_control_view_walker().map_err(|e| anyhow::anyhow!("{}", e))?;
        let mut results = Vec::new();
        self.collect_children(&walker, &start, 0, depth, &mut results);

        Ok(ToolResult::ok(format!(
            "Children ({} elements):\n{}",
            results.len(),
            results.join("\n")
        )))
    }

    fn collect_children(
        &self,
        walker: &uiautomation::UITreeWalker,
        element: &uiautomation::UIElement,
        current_depth: usize,
        max_depth: usize,
        results: &mut Vec<String>,
    ) {
        if current_depth >= max_depth { return; }
        if let Ok(child) = walker.get_first_child(element) {
            let mut current = child;
            loop {
                let name = current.get_name().unwrap_or_default();
                let class = current.get_classname().unwrap_or_default();
                let indent = "  ".repeat(current_depth);
                results.push(format!(
                    "{}Name='{}', Class='{}'",
                    indent, name, class
                ));
                self.collect_children(walker, &current, current_depth + 1, max_depth, results);
                match walker.get_next_sibling(&current) {
                    Ok(next) => current = next,
                    Err(_) => break,
                }
            }
        }
    }

    fn get_rect(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);

        match matcher.find_first() {
            Ok(element) => {
                let rect = element.get_bounding_rectangle().map_err(|e| anyhow::anyhow!("{}", e))?;
                let left = rect.get_left();
                let top = rect.get_top();
                let right = rect.get_right();
                let bottom = rect.get_bottom();
                Ok(ToolResult::ok(format!(
                    "Rect: left={}, top={}, right={}, bottom={}, width={}, height={}",
                    left, top, right, bottom,
                    right - left,
                    bottom - top
                )))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found: {}", e))),
        }
    }

    fn get_value(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        use uiautomation::patterns::{UIValuePattern, UITextPattern};
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);

        match matcher.find_first() {
            Ok(element) => {
                let name = element.get_name().unwrap_or_default();

                // Try ValuePattern first (standard edit controls, combo boxes)
                if let Ok(pattern) = element.get_pattern::<UIValuePattern>() {
                    if let Ok(val) = pattern.get_value() {
                        if !val.is_empty() {
                            return Ok(ToolResult::ok(format!(
                                "Element: '{}'\nValue (ValuePattern): {}",
                                name, val
                            )));
                        }
                    }
                }

                // Try TextPattern (RichEdit, document controls)
                if let Ok(pattern) = element.get_pattern::<UITextPattern>() {
                    if let Ok(doc_range) = pattern.get_document_range() {
                        if let Ok(text) = doc_range.get_text(1_000_000) {
                            if !text.is_empty() {
                                return Ok(ToolResult::ok(format!(
                                    "Element: '{}'\nValue (TextPattern): {}",
                                    name, text
                                )));
                            }
                        }
                    }
                }

                // Win32 fallback: WM_GETTEXT for classic Edit / RichEdit controls
                if let Ok(handle) = element.get_native_window_handle() {
                    use windows::Win32::Foundation::{HWND, WPARAM, LPARAM};
                    use windows::Win32::UI::WindowsAndMessaging::{
                        SendMessageW, WM_GETTEXT, WM_GETTEXTLENGTH,
                    };
                    let hwnd_isize: isize = handle.into();
                    let hwnd = HWND(hwnd_isize as *mut core::ffi::c_void);
                    let len = unsafe {
                        SendMessageW(hwnd, WM_GETTEXTLENGTH, WPARAM(0), LPARAM(0))
                    };
                    if len.0 > 0 {
                        let buf_len = (len.0 as usize) + 1;
                        let mut buf: Vec<u16> = vec![0u16; buf_len];
                        let copied = unsafe {
                            SendMessageW(
                                hwnd,
                                WM_GETTEXT,
                                WPARAM(buf_len),
                                LPARAM(buf.as_mut_ptr() as isize),
                            )
                        };
                        if copied.0 > 0 {
                            let text = String::from_utf16_lossy(&buf[..copied.0 as usize]);
                            if !text.is_empty() {
                                return Ok(ToolResult::ok(format!(
                                    "Element: '{}'\nValue (WM_GETTEXT): {}",
                                    name, text
                                )));
                            }
                        }
                    }
                }

                // Final fallback: element name only
                Ok(ToolResult::ok(format!(
                    "Element: '{}'\nNote: ValuePattern not supported by this control. Name returned as fallback.",
                    name
                )))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found: {}", e))),
        }
    }

    fn get_text(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        use uiautomation::patterns::{UIValuePattern, UITextPattern};
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);

        match matcher.find_first() {
            Ok(element) => {
                let name = element.get_name().unwrap_or_default();

                // Try TextPattern first (rich text, document controls).
                // Use a large positive max_length instead of -1 to avoid crate-specific quirks
                // where -1 may be interpreted as "0 characters" on some controls.
                if let Ok(pattern) = element.get_pattern::<UITextPattern>() {
                    if let Ok(doc_range) = pattern.get_document_range() {
                        if let Ok(text) = doc_range.get_text(1_000_000) {
                            if !text.is_empty() {
                                return Ok(ToolResult::ok(format!(
                                    "Element: '{}'\nText (TextPattern): {}",
                                    name, text
                                )));
                            }
                        }
                    }
                }

                // Try ValuePattern as fallback (standard edit controls, combo boxes)
                if let Ok(pattern) = element.get_pattern::<UIValuePattern>() {
                    if let Ok(val) = pattern.get_value() {
                        if !val.is_empty() {
                            return Ok(ToolResult::ok(format!(
                                "Element: '{}'\nText (ValuePattern): {}",
                                name, val
                            )));
                        }
                    }
                }

                // Win32 fallback: use WM_GETTEXT for Edit / RichEdit controls that don't
                // expose TextPattern or ValuePattern via UIA (e.g. classic Notepad RichEdit).
                if let Ok(handle) = element.get_native_window_handle() {
                    use windows::Win32::Foundation::{HWND, WPARAM, LPARAM};
                    use windows::Win32::UI::WindowsAndMessaging::{
                        SendMessageW, WM_GETTEXT, WM_GETTEXTLENGTH,
                    };
                    let hwnd_isize: isize = handle.into();
                    let hwnd = HWND(hwnd_isize as *mut core::ffi::c_void);
                    let len = unsafe {
                        SendMessageW(hwnd, WM_GETTEXTLENGTH, WPARAM(0), LPARAM(0))
                    };
                    if len.0 > 0 {
                        let buf_len = (len.0 as usize) + 1;
                        let mut buf: Vec<u16> = vec![0u16; buf_len];
                        let copied = unsafe {
                            SendMessageW(
                                hwnd,
                                WM_GETTEXT,
                                WPARAM(buf_len),
                                LPARAM(buf.as_mut_ptr() as isize),
                            )
                        };
                        if copied.0 > 0 {
                            let text = String::from_utf16_lossy(&buf[..copied.0 as usize]);
                            if !text.is_empty() {
                                return Ok(ToolResult::ok(format!(
                                    "Element: '{}'\nText (WM_GETTEXT): {}",
                                    name, text
                                )));
                            }
                        }
                    }
                }

                // Final fallback: element name only
                Ok(ToolResult::ok(format!(
                    "Element: '{}'\nNote: Could not read text content (TextPattern/ValuePattern/WM_GETTEXT all returned empty).",
                    name
                )))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found: {}", e))),
        }
    }

    // ─── Mouse Actions ────────────────────────────────────────────────────────

    fn click_element(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        use uiautomation::inputs::Mouse;
        use uiautomation::types::Point;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;

        if let (Some(x), Some(y)) = (input["x"].as_i64(), input["y"].as_i64()) {
            let (px, py) = if input["logical_coords"].as_bool().unwrap_or(false) {
                let scale = crate::tools::dpi::get_dpi_scale();
                ((x as f64 * scale) as i32, (y as f64 * scale) as i32)
            } else {
                (x as i32, y as i32)
            };
            Mouse::new().click(&Point::new(px, py))
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            return Ok(ToolResult::ok(format!("Clicked at ({}, {})", px, py)));
        }

        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);
        match matcher.find_first() {
            Ok(element) => {
                // First try native invoke click; if that fails, retry with center-point mouse click.
                match element.click() {
                    Ok(_) => Ok(ToolResult::ok(format!("Clicked: '{}'", element.get_name().unwrap_or_default()))),
                    Err(_) => {
                        let rect = element.get_bounding_rectangle().map_err(|e| anyhow::anyhow!("{}", e))?;
                        let cx = (rect.get_left() + rect.get_right()) / 2;
                        let cy = (rect.get_top() + rect.get_bottom()) / 2;
                        Mouse::new().click(&Point::new(cx, cy))
                            .map_err(|e| anyhow::anyhow!("{}", e))?;
                        Ok(ToolResult::ok(format!(
                            "Clicked with coordinate fallback: '{}' at ({}, {})",
                            element.get_name().unwrap_or_default(),
                            cx,
                            cy
                        )))
                    }
                }
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found for click: {}", e))),
        }
    }

    fn double_click_element(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        use uiautomation::inputs::Mouse;
        use uiautomation::types::Point;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;

        if let (Some(x), Some(y)) = (input["x"].as_i64(), input["y"].as_i64()) {
            Mouse::new().double_click(&Point::new(x as i32, y as i32))
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            return Ok(ToolResult::ok(format!("Double-clicked at ({}, {})", x, y)));
        }

        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);
        match matcher.find_first() {
            Ok(element) => {
                let rect = element.get_bounding_rectangle().map_err(|e| anyhow::anyhow!("{}", e))?;
                let cx = (rect.get_left() + rect.get_right()) / 2;
                let cy = (rect.get_top() + rect.get_bottom()) / 2;
                Mouse::new().double_click(&Point::new(cx, cy))
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(ToolResult::ok(format!("Double-clicked: '{}'", element.get_name().unwrap_or_default())))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found for double_click: {}", e))),
        }
    }

    fn right_click_element(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        use uiautomation::inputs::Mouse;
        use uiautomation::types::Point;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;

        if let (Some(x), Some(y)) = (input["x"].as_i64(), input["y"].as_i64()) {
            Mouse::new().right_click(&Point::new(x as i32, y as i32))
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            return Ok(ToolResult::ok(format!("Right-clicked at ({}, {})", x, y)));
        }

        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);
        match matcher.find_first() {
            Ok(element) => {
                let rect = element.get_bounding_rectangle().map_err(|e| anyhow::anyhow!("{}", e))?;
                let cx = (rect.get_left() + rect.get_right()) / 2;
                let cy = (rect.get_top() + rect.get_bottom()) / 2;
                Mouse::new().right_click(&Point::new(cx, cy))
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(ToolResult::ok(format!("Right-clicked: '{}'", element.get_name().unwrap_or_default())))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found for right_click: {}", e))),
        }
    }


    fn hover_element(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;

        if let (Some(x), Some(y)) = (input["x"].as_i64(), input["y"].as_i64()) {
            use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
            unsafe { SetCursorPos(x as i32, y as i32).map_err(|e| anyhow::anyhow!("{}", e))?; }
            return Ok(ToolResult::ok(format!("Hovered at ({}, {})", x, y)));
        }

        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);
        match matcher.find_first() {
            Ok(element) => {
                let rect = element.get_bounding_rectangle().map_err(|e| anyhow::anyhow!("{}", e))?;
                let cx = (rect.get_left() + rect.get_right()) / 2;
                let cy = (rect.get_top() + rect.get_bottom()) / 2;
                use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
                unsafe { let _ = SetCursorPos(cx, cy); }
                Ok(ToolResult::ok(format!("Hovered: '{}'", element.get_name().unwrap_or_default())))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found for hover: {}", e))),
        }
    }

    fn scroll_element(&self, input: &Value) -> Result<ToolResult> {
        use windows::Win32::UI::WindowsAndMessaging::SetCursorPos;
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEINPUT, MOUSE_EVENT_FLAGS,
            MOUSEEVENTF_WHEEL, MOUSEEVENTF_HWHEEL,
        };

        let direction = input["direction"].as_str().unwrap_or("down");
        let amount = input["amount"].as_i64().unwrap_or(3) as i32;

        if let (Some(x), Some(y)) = (input["x"].as_i64(), input["y"].as_i64()) {
            unsafe { let _ = SetCursorPos(x as i32, y as i32); }
        }

        let (flags, wheel_data): (MOUSE_EVENT_FLAGS, i32) = match direction {
            "up"    => (MOUSEEVENTF_WHEEL,  120 * amount),
            "down"  => (MOUSEEVENTF_WHEEL,  -120 * amount),
            "left"  => (MOUSEEVENTF_HWHEEL, -120 * amount),
            "right" => (MOUSEEVENTF_HWHEEL,  120 * amount),
            _       => (MOUSEEVENTF_WHEEL,  -120 * amount),
        };

        let input_ev = [INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx: 0, dy: 0,
                    mouseData: wheel_data as u32,
                    dwFlags: flags,
                    time: 0, dwExtraInfo: 0,
                },
            },
        }];
        unsafe { SendInput(&input_ev, std::mem::size_of::<INPUT>() as i32); }
        Ok(ToolResult::ok(format!("Scrolled {} by {} ticks", direction, amount)))
    }

    fn drag_drop(&self, input: &Value) -> Result<ToolResult> {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEINPUT,
            MOUSEEVENTF_MOVE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_ABSOLUTE,
            MOUSEEVENTF_VIRTUALDESK,
        };
        #[allow(unused_imports)]
        use windows::Win32::UI::WindowsAndMessaging::GetSystemMetrics;

        let (x1_raw, y1_raw) = match (input["x"].as_i64(), input["y"].as_i64()) {
            (Some(x), Some(y)) => (x as i32, y as i32),
            _ => return Ok(ToolResult::err("drag_drop requires x, y (start) and x2, y2 (end)")),
        };
        let (x2_raw, y2_raw) = match (input["x2"].as_i64(), input["y2"].as_i64()) {
            (Some(x), Some(y)) => (x as i32, y as i32),
            _ => return Ok(ToolResult::err("drag_drop requires x2, y2 (end coordinates)")),
        };

        // If logical_coords=true, the caller is providing coordinates from a screenshot
        // (logical/DPI-unaware pixels). Multiply by DPI scale to get physical screen coords.
        let (x1, y1, x2, y2) = if input["logical_coords"].as_bool().unwrap_or(false) {
            let scale = crate::tools::dpi::get_dpi_scale();
            let coords = (
                (x1_raw as f64 * scale) as i32,
                (y1_raw as f64 * scale) as i32,
                (x2_raw as f64 * scale) as i32,
                (y2_raw as f64 * scale) as i32,
            );
            tracing::info!(
                "drag_drop: logical({},{})→({},{}) × dpi={:.2} → physical({},{})→({},{})",
                x1_raw, y1_raw, x2_raw, y2_raw, scale,
                coords.0, coords.1, coords.2, coords.3
            );
            coords
        } else {
            tracing::info!(
                "drag_drop: physical({},{})→({},{})",
                x1_raw, y1_raw, x2_raw, y2_raw
            );
            (x1_raw, y1_raw, x2_raw, y2_raw)
        };

        // MOUSEEVENTF_ABSOLUTE (without VIRTUALDESK) maps 0-65535 onto the primary
        // monitor in logical pixels. GetDeviceCaps(HORZRES/VERTRES) always returns
        // logical pixels regardless of DPI awareness mode — same coordinate space
        // as screen_capture grid labels.
        let (sw, sh) = unsafe {
            use windows::Win32::Graphics::Gdi::{GetDC, ReleaseDC, GetDeviceCaps, HORZRES, VERTRES};
            use windows::Win32::UI::WindowsAndMessaging::GetDesktopWindow;
            let hwnd = GetDesktopWindow();
            let hdc = GetDC(hwnd);
            let w = GetDeviceCaps(hdc, HORZRES);
            let h = GetDeviceCaps(hdc, VERTRES);
            ReleaseDC(hwnd, hdc);
            (w, h)
        };

        tracing::info!(
            "drag_drop: logical_screen={}x{} input=({},{})→({},{})",
            sw, sh, x1, y1, x2, y2
        );

        let to_abs = |px: i32, py: i32| -> (i32, i32) {
            let ax = (px * 65535 + sw / 2) / sw.max(1);
            let ay = (py * 65535 + sh / 2) / sh.max(1);
            (ax, ay)
        };

        let (ax1, ay1) = to_abs(x1, y1);
        let (ax2, ay2) = to_abs(x2, y2);
        tracing::info!(
            "drag_drop: abs=({},{})→({},{})",
            ax1, ay1, ax2, ay2
        );

        // No VIRTUALDESK: maps 0-65535 to primary monitor logical pixels (matches GetDeviceCaps HORZRES)
        let flags_move = MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE;
        let flags_down = MOUSEEVENTF_LEFTDOWN | MOUSEEVENTF_ABSOLUTE;
        let flags_up   = MOUSEEVENTF_LEFTUP   | MOUSEEVENTF_ABSOLUTE;

        let make_mouse_input = |dx: i32, dy: i32, flags| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dx, dy,
                    mouseData: 0,
                    dwFlags: flags,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        };

        unsafe {
            // Move to start position
            let ev = [make_mouse_input(ax1, ay1, flags_move)];
            SendInput(&ev, std::mem::size_of::<INPUT>() as i32);
            std::thread::sleep(std::time::Duration::from_millis(80));

            // Press left button at start
            let ev = [make_mouse_input(ax1, ay1, flags_down)];
            SendInput(&ev, std::mem::size_of::<INPUT>() as i32);
            std::thread::sleep(std::time::Duration::from_millis(80));

            // Smoothly move to end position in steps (helps with apps that need
            // intermediate mouse-move events to track drag, e.g. ball drag tests)
            let steps = 20i32;
            for i in 1..=steps {
                let ix = ax1 + (ax2 - ax1) * i / steps;
                let iy = ay1 + (ay2 - ay1) * i / steps;
                let ev = [make_mouse_input(ix, iy, flags_move)];
                SendInput(&ev, std::mem::size_of::<INPUT>() as i32);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }

            // Release left button at end
            let ev = [make_mouse_input(ax2, ay2, flags_up)];
            SendInput(&ev, std::mem::size_of::<INPUT>() as i32);
        }

        Ok(ToolResult::ok(format!("Dragged from ({},{}) to ({},{})", x1, y1, x2, y2)))
    }

    // ─── Keyboard Actions ─────────────────────────────────────────────────────

    fn type_text(&self, input: &Value) -> Result<ToolResult> {
        let text = match input["text"].as_str() {
            Some(t) => t,
            None => return Ok(ToolResult::err("Missing parameter: text")),
        };

        use uiautomation::UIAutomation;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;
        let mut matcher = automation.create_matcher().from(root).timeout(3000);
        if let Some(name) = input["name"].as_str() { matcher = matcher.name(name); }
        if let Some(class) = input["class_name"].as_str() { matcher = matcher.classname(class); }

        match matcher.find_first() {
            Ok(element) => {
                element.send_keys(text, 10).map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(ToolResult::ok("Typed text into element"))
            }
            Err(_) => {
                let root2 = automation.get_root_element().map_err(|e| anyhow::anyhow!("{}", e))?;
                root2.send_keys(text, 10).map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(ToolResult::ok("Typed text to focused element"))
            }
        }
    }

    fn send_hotkey(&self, input: &Value) -> Result<ToolResult> {
        let hotkey = match input["hotkey"].as_str() {
            Some(h) => h.to_lowercase(),
            None => return Ok(ToolResult::err("Missing parameter: hotkey (e.g. 'ctrl+c')")),
        };

        use windows::Win32::UI::Input::KeyboardAndMouse::{
            keybd_event, KEYEVENTF_KEYUP,
            VK_CONTROL, VK_MENU, VK_SHIFT, VK_LWIN, VK_RETURN, VK_ESCAPE, VK_TAB,
            VK_F1, VK_F2, VK_F3, VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_F10, VK_F11, VK_F12,
            VK_DELETE, VK_BACK, VK_HOME, VK_END, VK_PRIOR, VK_NEXT, VK_LEFT, VK_RIGHT, VK_UP, VK_DOWN,
        };

        let parts: Vec<&str> = hotkey.split('+').collect();
        let mut vkeys: Vec<u8> = Vec::new();

        for part in &parts {
            let vk: u8 = match part.trim() {
                "ctrl" | "control" => VK_CONTROL.0 as u8,
                "alt"              => VK_MENU.0 as u8,
                "shift"            => VK_SHIFT.0 as u8,
                "win" | "windows"  => VK_LWIN.0 as u8,
                "enter" | "return" => VK_RETURN.0 as u8,
                "esc" | "escape"   => VK_ESCAPE.0 as u8,
                "tab"              => VK_TAB.0 as u8,
                "delete" | "del"   => VK_DELETE.0 as u8,
                "backspace"        => VK_BACK.0 as u8,
                "home"             => VK_HOME.0 as u8,
                "end"              => VK_END.0 as u8,
                "pageup"           => VK_PRIOR.0 as u8,
                "pagedown"         => VK_NEXT.0 as u8,
                "left"             => VK_LEFT.0 as u8,
                "right"            => VK_RIGHT.0 as u8,
                "up"               => VK_UP.0 as u8,
                "down"             => VK_DOWN.0 as u8,
                "f1"  => VK_F1.0 as u8,  "f2"  => VK_F2.0 as u8,  "f3"  => VK_F3.0 as u8,  "f4"  => VK_F4.0 as u8,
                "f5"  => VK_F5.0 as u8,  "f6"  => VK_F6.0 as u8,  "f7"  => VK_F7.0 as u8,  "f8"  => VK_F8.0 as u8,
                "f9"  => VK_F9.0 as u8,  "f10" => VK_F10.0 as u8, "f11" => VK_F11.0 as u8, "f12" => VK_F12.0 as u8,
                s if s.len() == 1 => s.chars().next().unwrap().to_ascii_uppercase() as u8,
                _ => continue,
            };
            vkeys.push(vk);
        }

        if vkeys.is_empty() {
            return Ok(ToolResult::err(format!("Could not parse hotkey: {}", hotkey)));
        }

        unsafe {
            // Press all keys
            for &vk in &vkeys {
                keybd_event(vk, 0, windows::Win32::UI::Input::KeyboardAndMouse::KEYBD_EVENT_FLAGS(0), 0);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            // Release all keys in reverse
            for &vk in vkeys.iter().rev() {
                keybd_event(vk, 0, KEYEVENTF_KEYUP, 0);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
        }
        Ok(ToolResult::ok(format!("Sent hotkey: {}", hotkey)))
    }

    fn send_keys_action(&self, input: &Value) -> Result<ToolResult> {
        let keys = match input["keys"].as_str() {
            Some(k) => k,
            None => return Ok(ToolResult::err("Missing parameter: keys")),
        };

        use uiautomation::UIAutomation;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = automation.get_root_element().map_err(|e| anyhow::anyhow!("{}", e))?;
        // send_keys to focused element (root acts as global keyboard input)
        root.send_keys(keys, 50).map_err(|e| anyhow::anyhow!("{}", e))?;
        Ok(ToolResult::ok(format!("Sent keys: {}", keys)))
    }

    // ─── State Actions ────────────────────────────────────────────────────────

    fn expand_element(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        use uiautomation::patterns::UIExpandCollapsePattern;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);
        match matcher.find_first() {
            Ok(element) => {
                let name = element.get_name().unwrap_or_default();
                // Try ExpandCollapsePattern first (correct semantic)
                if let Ok(pattern) = element.get_pattern::<UIExpandCollapsePattern>() {
                    pattern.expand().map_err(|e| anyhow::anyhow!("{}", e))?;
                    return Ok(ToolResult::ok(format!("Expanded '{}' via ExpandCollapsePattern", name)));
                }
                // Fallback: click (toggles state, may not guarantee expanded)
                element.click().map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(ToolResult::ok(format!("Expanded (clicked fallback): '{}'", name)))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found: {}", e))),
        }
    }

    fn collapse_element(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        use uiautomation::patterns::UIExpandCollapsePattern;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);
        match matcher.find_first() {
            Ok(element) => {
                let name = element.get_name().unwrap_or_default();
                // Try ExpandCollapsePattern first (correct semantic)
                if let Ok(pattern) = element.get_pattern::<UIExpandCollapsePattern>() {
                    pattern.collapse().map_err(|e| anyhow::anyhow!("{}", e))?;
                    return Ok(ToolResult::ok(format!("Collapsed '{}' via ExpandCollapsePattern", name)));
                }
                // Fallback: click
                element.click().map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(ToolResult::ok(format!("Collapsed (clicked fallback): '{}'", name)))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found: {}", e))),
        }
    }

    fn select_item(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);
        match matcher.find_first() {
            Ok(element) => {
                element.click().map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(ToolResult::ok(format!("Selected (clicked): '{}'", element.get_name().unwrap_or_default())))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found: {}", e))),
        }
    }

    fn set_check(&self, input: &Value, checked: bool) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);
        match matcher.find_first() {
            Ok(element) => {
                // Click to toggle checkbox state
                element.click().map_err(|e| anyhow::anyhow!("{}", e))?;
                let action = if checked { "Checked" } else { "Unchecked" };
                Ok(ToolResult::ok(format!("{} (clicked): '{}'", action, element.get_name().unwrap_or_default())))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found: {}", e))),
        }
    }

    // ─── Wait ─────────────────────────────────────────────────────────────────

    fn wait_for_element(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        let timeout_ms = input["timeout_ms"].as_u64().unwrap_or(10000);
        let poll_ms = 500u64;
        let start = std::time::Instant::now();

        loop {
            let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
            let root = match self.get_search_root(&automation, input) {
                Ok(r) => r,
                Err(_) => {
                    if start.elapsed().as_millis() as u64 >= timeout_ms {
                        return Ok(ToolResult::err("Timeout: element not found"));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(poll_ms));
                    continue;
                }
            };
            let matcher = self.build_matcher(&automation, root, input, 1000);
            match matcher.find_first() {
                Ok(element) => {
                    let name = element.get_name().unwrap_or_default();
                    let elapsed = start.elapsed().as_millis();
                    return Ok(ToolResult::ok(format!(
                        "Element found after {}ms: Name='{}'", elapsed, name
                    )));
                }
                Err(_) => {
                    if start.elapsed().as_millis() as u64 >= timeout_ms {
                        return Ok(ToolResult::err(format!(
                            "Timeout after {}ms: element not found", timeout_ms
                        )));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(poll_ms));
                }
            }
        }
    }

    // ─── Window Management ────────────────────────────────────────────────────

    fn find_window_hwnd(&self, input: &Value) -> Result<windows::Win32::Foundation::HWND> {
        use windows::Win32::UI::WindowsAndMessaging::{
            FindWindowW, GetForegroundWindow, EnumWindows, GetWindowTextW, IsWindowVisible,
        };
        use windows::Win32::Foundation::{HWND, BOOL, LPARAM};
        use windows::core::PCWSTR;

        let title = match input["name"].as_str().or(input["window_title"].as_str()) {
            Some(t) => t,
            None => return Ok(unsafe { GetForegroundWindow() }),
        };

        // First try exact match (fast path)
        let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        if let Ok(hwnd) = unsafe { FindWindowW(PCWSTR::null(), PCWSTR(wide.as_ptr())) } {
            return Ok(hwnd);
        }

        // Fallback: partial match via EnumWindows (consistent with get_search_root)
        let title_lower = title.to_lowercase();
        let result = std::sync::Arc::new(std::sync::Mutex::new(None::<HWND>));
        let result_clone = result.clone();
        let title_clone = title_lower.clone();

        unsafe extern "system" fn enum_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
            let data = &*(lparam.0 as *const (String, std::sync::Arc<std::sync::Mutex<Option<HWND>>>));
            let (title_lower, result) = data;
            if IsWindowVisible(hwnd).as_bool() {
                let mut buf = [0u16; 512];
                let len = GetWindowTextW(hwnd, &mut buf);
                if len > 0 {
                    let window_title = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
                    if window_title.contains(title_lower.as_str()) {
                        *result.lock().unwrap() = Some(hwnd);
                        return BOOL(0); // stop enumeration
                    }
                }
            }
            BOOL(1) // continue
        }

        let data = (title_clone, result_clone);
        let _ = unsafe { EnumWindows(Some(enum_callback), LPARAM(&data as *const _ as isize)) };

        let found = *result.lock().unwrap();
        match found {
            Some(hwnd) => Ok(hwnd),
            None => Err(anyhow::anyhow!("Window '{}' not found (tried exact and partial match)", title)),
        }
    }

    fn activate_window(&self, input: &Value) -> Result<ToolResult> {
        use windows::Win32::UI::WindowsAndMessaging::{
            SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
            GetForegroundWindow, IsIconic, IsWindowVisible,
            GetWindowThreadProcessId, BringWindowToTop,
        };
        use windows::Win32::UI::Input::KeyboardAndMouse::SetActiveWindow;
        use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};

        let hwnd = self.find_window_hwnd(input)?;

        unsafe {
            // 1. Restore if minimized
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
                std::thread::sleep(std::time::Duration::from_millis(200));
            } else {
                let _ = ShowWindow(hwnd, SW_SHOW);
            }

            // 2. Already foreground — nothing to do
            if GetForegroundWindow() == hwnd {
                return Ok(ToolResult::ok("Window already in foreground"));
            }

            // 3. AttachThreadInput: the most reliable way to steal foreground
            //    without sending global keyboard events (which would disturb
            //    other windows like the IDE).
            let fg_hwnd = GetForegroundWindow();
            let fg_thread = GetWindowThreadProcessId(fg_hwnd, None);
            let our_thread = GetCurrentThreadId();
            let attached = fg_thread != 0 && fg_thread != our_thread;
            if attached {
                let _ = AttachThreadInput(our_thread, fg_thread, true);
            }

            let _ = BringWindowToTop(hwnd);
            let _ = SetActiveWindow(hwnd);
            let _ = SetForegroundWindow(hwnd);

            if attached {
                let _ = AttachThreadInput(our_thread, fg_thread, false);
            }

            // Give the OS time to process the foreground change.
            std::thread::sleep(std::time::Duration::from_millis(300));

            if GetForegroundWindow() != hwnd && IsWindowVisible(hwnd).as_bool() {
                return Ok(ToolResult::ok(
                    "Window activated (best-effort; foreground lock may be held by another process)"
                ));
            }
        }
        Ok(ToolResult::ok("Window activated and in foreground"))
    }

    fn window_state(&self, input: &Value, state: &str) -> Result<ToolResult> {
        use windows::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_MINIMIZE, SW_MAXIMIZE, SW_RESTORE};
        let hwnd = self.find_window_hwnd(input)?;
        let cmd = match state {
            "minimize" => SW_MINIMIZE,
            "maximize" => SW_MAXIMIZE,
            "restore"  => SW_RESTORE,
            _          => SW_RESTORE,
        };
        unsafe { let _ = ShowWindow(hwnd, cmd); }
        Ok(ToolResult::ok(format!("Window {}", state)))
    }

    fn close_window(&self, input: &Value) -> Result<ToolResult> {
        use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
        use windows::Win32::Foundation::{WPARAM, LPARAM};
        let hwnd = self.find_window_hwnd(input)?;
        unsafe {
            PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0))
                .map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        Ok(ToolResult::ok("Window close message sent"))
    }

    fn move_window(&self, input: &Value) -> Result<ToolResult> {
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, HWND_TOP, SWP_NOSIZE, SWP_NOZORDER};
        let hwnd = self.find_window_hwnd(input)?;
        let x = input["x"].as_i64().unwrap_or(0) as i32;
        let y = input["y"].as_i64().unwrap_or(0) as i32;
        unsafe {
            SetWindowPos(hwnd, HWND_TOP, x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        Ok(ToolResult::ok(format!("Window moved to ({}, {})", x, y)))
    }

    fn resize_window(&self, input: &Value) -> Result<ToolResult> {
        use windows::Win32::UI::WindowsAndMessaging::{SetWindowPos, HWND_TOP, SWP_NOMOVE, SWP_NOZORDER};
        let hwnd = self.find_window_hwnd(input)?;
        let w = input["x2"].as_i64().unwrap_or(800) as i32;
        let h = input["y2"].as_i64().unwrap_or(600) as i32;
        unsafe {
            SetWindowPos(hwnd, HWND_TOP, 0, 0, w, h, SWP_NOMOVE | SWP_NOZORDER)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        Ok(ToolResult::ok(format!("Window resized to {}x{}", w, h)))
    }

    fn get_window_rect(&self, input: &Value) -> Result<ToolResult> {
        use windows::Win32::UI::WindowsAndMessaging::GetWindowRect;
        use windows::Win32::Foundation::RECT;
        let hwnd = self.find_window_hwnd(input)?;
        let mut rect = RECT::default();
        unsafe {
            GetWindowRect(hwnd, &mut rect).map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        Ok(ToolResult::ok(format!(
            "Window rect: left={}, top={}, right={}, bottom={}, width={}, height={}",
            rect.left, rect.top, rect.right, rect.bottom,
            rect.right - rect.left, rect.bottom - rect.top
        )))
    }

    // ─── Hybrid Vision Automation ────────────────────────────────────────────

    async fn smart_find(&self, input: &Value, _ctx: &ToolContext) -> Result<ToolResult> {
        use uiautomation::UIAutomation;

        let description = input["description"].as_str().unwrap_or("");

        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;

        let mut matcher = automation.create_matcher().from(root.clone()).timeout(3000);
        if let Some(name) = input["name"].as_str() {
            matcher = matcher.name(name);
        } else if !description.is_empty() {
            matcher = matcher.name(description);
        }
        if let Some(class) = input["class_name"].as_str() {
            matcher = matcher.classname(class);
        }
        if let Some(ct) = input["control_type"].as_str() {
            matcher = matcher.classname(ct);
        }

        match matcher.find_first() {
            Ok(element) => {
                let name = element.get_name().unwrap_or_default();
                let class = element.get_classname().unwrap_or_default();
                let rect = element.get_bounding_rectangle().map_err(|e| anyhow::anyhow!("{}", e))?;
                let cx = (rect.get_left() + rect.get_right()) / 2;
                let cy = (rect.get_top() + rect.get_bottom()) / 2;
                Ok(ToolResult::ok(format!(
                    "Found via UIA: Name='{}', Class='{}', Center=({}, {}), Rect=[{},{},{},{}]",
                    name, class, cx, cy,
                    rect.get_left(), rect.get_top(), rect.get_right(), rect.get_bottom()
                )))
            }
            Err(_) => {
                let screen_tool = super::screen::ScreenTool;
                let capture_input = serde_json::json!({
                    "action": "capture",
                    "format": "jpeg",
                    "quality": 75
                });
                match screen_tool.capture_full(&capture_input) {
                    Ok(result) => {
                        let msg = format!(
                            "UIA could not find element matching '{}'. Screenshot captured for Vision AI analysis. \
                             Please analyze the screenshot to locate the element and provide coordinates.",
                            description
                        );
                        if let Some(img) = result.image {
                            Ok(ToolResult::ok(msg).with_image(img))
                        } else {
                            Ok(ToolResult::ok(msg))
                        }
                    }
                    Err(e) => Ok(ToolResult::err(format!(
                        "UIA search failed and screenshot also failed: {}", e
                    )))
                }
            }
        }
    }

    fn annotate_elements(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;

        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;
        let max_elements = input["max_elements"].as_u64().unwrap_or(30) as usize;

        let walker = automation.get_control_view_walker().map_err(|e| anyhow::anyhow!("{}", e))?;
        let mut elements: Vec<(String, String, i32, i32, i32, i32)> = Vec::new();
        self.collect_interactive_elements(&walker, &root, 0, 4, &mut elements, max_elements);

        if elements.is_empty() {
            return Ok(ToolResult::err("No interactive elements found in the target window"));
        }

        let screen_tool = super::screen::ScreenTool;
        let capture_input = serde_json::json!({ "action": "capture", "format": "png" });
        let capture_result = screen_tool.capture_full(&capture_input)?;

        let mut map_text = String::from("Annotated elements:\n");
        for (i, (name, class, left, top, right, bottom)) in elements.iter().enumerate() {
            let cx = (left + right) / 2;
            let cy = (top + bottom) / 2;
            map_text.push_str(&format!(
                "[{}] Name='{}', Class='{}', Center=({},{}), Rect=[{},{},{},{}]\n",
                i + 1, name, class, cx, cy, left, top, right, bottom
            ));
        }

        if let Some(img) = capture_result.image {
            Ok(ToolResult::ok(map_text).with_image(img))
        } else {
            Ok(ToolResult::ok(map_text))
        }
    }

    fn collect_interactive_elements(
        &self,
        walker: &uiautomation::UITreeWalker,
        element: &uiautomation::UIElement,
        depth: usize,
        max_depth: usize,
        results: &mut Vec<(String, String, i32, i32, i32, i32)>,
        max_elements: usize,
    ) {
        if depth >= max_depth || results.len() >= max_elements { return; }
        if let Ok(child) = walker.get_first_child(element) {
            let mut current = child;
            loop {
                if results.len() >= max_elements { break; }
                let name = current.get_name().unwrap_or_default();
                let class = current.get_classname().unwrap_or_default();

                let is_interactive = matches!(
                    class.as_str(),
                    "Button" | "Edit" | "ComboBox" | "CheckBox" | "RadioButton" |
                    "ListItem" | "MenuItem" | "TabItem" | "Hyperlink" | "TreeItem" |
                    "Slider" | "Spinner" | "ToggleButton" | "SplitButton"
                ) || class.contains("Button") || class.contains("Edit") || class.contains("TextBox");

                if is_interactive {
                    if let Ok(rect) = current.get_bounding_rectangle() {
                        let l = rect.get_left();
                        let t = rect.get_top();
                        let r = rect.get_right();
                        let b = rect.get_bottom();
                        if r > l && b > t && (r - l) > 2 && (b - t) > 2 {
                            results.push((name, class, l, t, r, b));
                        }
                    }
                }

                self.collect_interactive_elements(walker, &current, depth + 1, max_depth, results, max_elements);
                match walker.get_next_sibling(&current) {
                    Ok(next) => current = next,
                    Err(_) => break,
                }
            }
        }
    }
}

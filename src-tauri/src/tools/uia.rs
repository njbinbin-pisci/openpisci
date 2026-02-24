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
         Supports finding controls, clicking, typing, keyboard shortcuts, scrolling, \
         drag-drop, window management, and more. \
         Use screen_capture with Vision AI as fallback when UIA cannot find elements."
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
                        "close_window", "move_window", "resize_window", "get_window_rect"
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
                }
            },
            "required": ["action"]
        })
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolResult> {
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
            _ => Ok(ToolResult::err(format!("Unknown action: {}", action))),
        }
    }
}

// ─── Helper: build a matcher from common search params ───────────────────────

impl UiaTool {
    fn build_matcher<'a>(
        &self,
        automation: &'a uiautomation::UIAutomation,
        root: uiautomation::UIElement,
        input: &Value,
        timeout_ms: u64,
    ) -> uiautomation::UIMatcher<'a> {
        let mut matcher = automation.create_matcher().from(root).timeout(timeout_ms as i32);
        if let Some(name) = input["name"].as_str() {
            matcher = matcher.name(name);
        } else if let Some(aid_fallback) = input["automation_id"].as_str() {
            // Compatibility fallback: when automation_id APIs are limited,
            // treat automation_id as a name hint to improve matching.
            matcher = matcher.name(aid_fallback);
        }
        if let Some(class) = input["class_name"].as_str() {
            matcher = matcher.classname(class);
        } else if let Some(control_type_fallback) = input["control_type"].as_str() {
            // Compatibility fallback: map control_type to classname filter.
            matcher = matcher.classname(control_type_fallback);
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
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);

        match matcher.find_first() {
            Ok(element) => {
                // Get element name/value
                let name = element.get_name().unwrap_or_default();
                Ok(ToolResult::ok(format!("Value: {}", name)))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found: {}", e))),
        }
    }

    fn get_text(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);

        match matcher.find_first() {
            Ok(element) => {
                let text = element.get_name().unwrap_or_default();
                Ok(ToolResult::ok(format!("Text: {}", text)))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found: {}", e))),
        }
    }

    // ─── Mouse Actions ────────────────────────────────────────────────────────

    fn click_element(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        use uiautomation::inputs::MouseButton;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;

        if let (Some(x), Some(y)) = (input["x"].as_i64(), input["y"].as_i64()) {
            automation.send_mouse_input(x as i32, y as i32, MouseButton::Left)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            return Ok(ToolResult::ok(format!("Clicked at ({}, {})", x, y)));
        }

        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);
        match matcher.find_first() {
            Ok(element) => {
                element.click().map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(ToolResult::ok(format!("Clicked: '{}'", element.get_name().unwrap_or_default())))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found for click: {}", e))),
        }
    }

    fn double_click_element(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        use uiautomation::inputs::MouseButton;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;

        if let (Some(x), Some(y)) = (input["x"].as_i64(), input["y"].as_i64()) {
            automation.send_mouse_input(x as i32, y as i32, MouseButton::Left)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            std::thread::sleep(std::time::Duration::from_millis(50));
            automation.send_mouse_input(x as i32, y as i32, MouseButton::Left)
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
                automation.send_mouse_input(cx, cy, MouseButton::Left)
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                std::thread::sleep(std::time::Duration::from_millis(50));
                automation.send_mouse_input(cx, cy, MouseButton::Left)
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(ToolResult::ok(format!("Double-clicked: '{}'", element.get_name().unwrap_or_default())))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found for double_click: {}", e))),
        }
    }

    fn right_click_element(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        use uiautomation::inputs::MouseButton;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;

        if let (Some(x), Some(y)) = (input["x"].as_i64(), input["y"].as_i64()) {
            automation.send_mouse_input(x as i32, y as i32, MouseButton::Right)
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
                automation.send_mouse_input(cx, cy, MouseButton::Right)
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(ToolResult::ok(format!("Right-clicked: '{}'", element.get_name().unwrap_or_default())))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found for right_click: {}", e))),
        }
    }


    fn hover_element(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        use uiautomation::inputs::MouseButton;

        if let (Some(x), Some(y)) = (input["x"].as_i64(), input["y"].as_i64()) {
            // Move mouse without clicking by using a tiny move
            let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
            // Use send_mouse_input to move to position (we'll use a workaround)
            // Move to position by clicking and immediately releasing won't work for hover
            // Instead, use SetCursorPos
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
        use windows::Win32::UI::WindowsAndMessaging::{mouse_event, SetCursorPos};
        // MOUSEEVENTF_WHEEL = 0x0800, MOUSEEVENTF_HWHEEL = 0x1000
        const MOUSEEVENTF_WHEEL: u32 = 0x0800;
        const MOUSEEVENTF_HWHEEL: u32 = 0x1000;

        let direction = input["direction"].as_str().unwrap_or("down");
        let amount = input["amount"].as_i64().unwrap_or(3) as i32;

        // If coordinates provided, move mouse there first
        if let (Some(x), Some(y)) = (input["x"].as_i64(), input["y"].as_i64()) {
            unsafe { let _ = SetCursorPos(x as i32, y as i32); }
        }

        let (flags, delta) = match direction {
            "up"    => (MOUSEEVENTF_WHEEL, (120 * amount) as u32),
            "down"  => (MOUSEEVENTF_WHEEL, (-120i32 * amount) as u32),
            "left"  => (MOUSEEVENTF_HWHEEL, (-120i32 * amount) as u32),
            "right" => (MOUSEEVENTF_HWHEEL, (120 * amount) as u32),
            _       => (MOUSEEVENTF_WHEEL, (-120i32 * amount) as u32),
        };

        unsafe {
            mouse_event(flags, 0, 0, delta, 0);
        }
        Ok(ToolResult::ok(format!("Scrolled {} by {} ticks", direction, amount)))
    }

    fn drag_drop(&self, input: &Value) -> Result<ToolResult> {
        use windows::Win32::UI::WindowsAndMessaging::{mouse_event, SetCursorPos};
        // MOUSEEVENTF_LEFTDOWN = 0x0002, MOUSEEVENTF_LEFTUP = 0x0004
        const MOUSEEVENTF_LEFTDOWN: u32 = 0x0002;
        const MOUSEEVENTF_LEFTUP: u32 = 0x0004;

        let (x1, y1) = match (input["x"].as_i64(), input["y"].as_i64()) {
            (Some(x), Some(y)) => (x as i32, y as i32),
            _ => return Ok(ToolResult::err("drag_drop requires x, y (start) and x2, y2 (end)")),
        };
        let (x2, y2) = match (input["x2"].as_i64(), input["y2"].as_i64()) {
            (Some(x), Some(y)) => (x as i32, y as i32),
            _ => return Ok(ToolResult::err("drag_drop requires x2, y2 (end coordinates)")),
        };

        unsafe {
            // Move to start position
            let _ = SetCursorPos(x1, y1);
            std::thread::sleep(std::time::Duration::from_millis(50));
            // Press left button
            mouse_event(MOUSEEVENTF_LEFTDOWN, x1 as u32, y1 as u32, 0, 0);
            std::thread::sleep(std::time::Duration::from_millis(100));
            // Move to end position
            let _ = SetCursorPos(x2, y2);
            std::thread::sleep(std::time::Duration::from_millis(100));
            // Release left button
            mouse_event(MOUSEEVENTF_LEFTUP, x2 as u32, y2 as u32, 0, 0);
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
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);
        match matcher.find_first() {
            Ok(element) => {
                // Try clicking to expand (most reliable fallback)
                element.click().map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(ToolResult::ok(format!("Expanded (clicked): '{}'", element.get_name().unwrap_or_default())))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found: {}", e))),
        }
    }

    fn collapse_element(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = self.get_search_root(&automation, input)?;
        let matcher = self.build_matcher(&automation, root, input, 5000);
        match matcher.find_first() {
            Ok(element) => {
                element.click().map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(ToolResult::ok(format!("Collapsed (clicked): '{}'", element.get_name().unwrap_or_default())))
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
        use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetForegroundWindow};
        use windows::core::PCWSTR;

        if let Some(title) = input["name"].as_str().or(input["window_title"].as_str()) {
            let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            let hwnd = unsafe { FindWindowW(PCWSTR::null(), PCWSTR(wide.as_ptr())) };
            if hwnd.0 as usize != 0 {
                return Ok(hwnd);
            }
            return Err(anyhow::anyhow!("Window '{}' not found", title));
        }
        Ok(unsafe { GetForegroundWindow() })
    }

    fn activate_window(&self, input: &Value) -> Result<ToolResult> {
        use windows::Win32::UI::WindowsAndMessaging::{SetForegroundWindow, ShowWindow, SW_RESTORE};
        let hwnd = self.find_window_hwnd(input)?;
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
        Ok(ToolResult::ok("Window activated"))
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
}

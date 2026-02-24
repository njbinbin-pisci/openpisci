/// Windows UI Automation tool (Windows only)
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
         Can find controls by name/class/automation ID, click them, and type text. \
         Use screen_capture with Vision AI as fallback when UIA cannot find elements."
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["find", "click", "type", "get_text", "list_windows"],
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
                "text": {
                    "type": "string",
                    "description": "Text to type (for 'type' action)"
                },
                "x": {
                    "type": "integer",
                    "description": "X coordinate for click (alternative to finding by name)"
                },
                "y": {
                    "type": "integer",
                    "description": "Y coordinate for click"
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
            "list_windows" => self.list_windows(),
            "find" => self.find_element(&input),
            "click" => self.click_element(&input),
            "type" => self.type_text(&input),
            "get_text" => self.get_text(&input),
            _ => Ok(ToolResult::err(format!("Unknown action: {}", action))),
        }
    }
}

impl UiaTool {
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
        let root = automation.get_root_element().map_err(|e| anyhow::anyhow!("{}", e))?;

        let mut matcher = automation.create_matcher().from(root).timeout(5000);

        if let Some(name) = input["name"].as_str() {
            matcher = matcher.name(name);
        }
        if let Some(class) = input["class_name"].as_str() {
            matcher = matcher.classname(class);
        }

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

    fn click_element(&self, input: &Value) -> Result<ToolResult> {
        // Click by coordinates if provided
        if let (Some(x), Some(y)) = (input["x"].as_i64(), input["y"].as_i64()) {
            use uiautomation::UIAutomation;
            let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
            automation.send_mouse_input(x as i32, y as i32, uiautomation::inputs::MouseButton::Left)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            return Ok(ToolResult::ok(format!("Clicked at ({}, {})", x, y)));
        }

        // Click by finding element
        use uiautomation::UIAutomation;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = automation.get_root_element().map_err(|e| anyhow::anyhow!("{}", e))?;
        let mut matcher = automation.create_matcher().from(root).timeout(5000);

        if let Some(name) = input["name"].as_str() {
            matcher = matcher.name(name);
        }

        match matcher.find_first() {
            Ok(element) => {
                element.click().map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(ToolResult::ok(format!(
                    "Clicked element: '{}'",
                    element.get_name().unwrap_or_default()
                )))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found for click: {}", e))),
        }
    }

    fn type_text(&self, input: &Value) -> Result<ToolResult> {
        let text = match input["text"].as_str() {
            Some(t) => t,
            None => return Ok(ToolResult::err("Missing parameter: text")),
        };

        use uiautomation::UIAutomation;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = automation.get_root_element().map_err(|e| anyhow::anyhow!("{}", e))?;

        // Find focused element or use name
        let mut matcher = automation.create_matcher().from(root).timeout(3000);
        if let Some(name) = input["name"].as_str() {
            matcher = matcher.name(name);
        }

        match matcher.find_first() {
            Ok(element) => {
                element.send_keys(text, 10).map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(ToolResult::ok(format!("Typed text into element")))
            }
            Err(_) => {
                // Fall back to sending keys to focused element
                root.send_keys(text, 10).map_err(|e| anyhow::anyhow!("{}", e))?;
                Ok(ToolResult::ok("Typed text to focused element"))
            }
        }
    }

    fn get_text(&self, input: &Value) -> Result<ToolResult> {
        use uiautomation::UIAutomation;
        let automation = UIAutomation::new().map_err(|e| anyhow::anyhow!("{}", e))?;
        let root = automation.get_root_element().map_err(|e| anyhow::anyhow!("{}", e))?;
        let mut matcher = automation.create_matcher().from(root).timeout(5000);

        if let Some(name) = input["name"].as_str() {
            matcher = matcher.name(name);
        }

        match matcher.find_first() {
            Ok(element) => {
                let text = element.get_name().unwrap_or_default();
                Ok(ToolResult::ok(format!("Text: {}", text)))
            }
            Err(e) => Ok(ToolResult::err(format!("Element not found: {}", e))),
        }
    }
}

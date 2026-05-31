//! Shared JSON-schema fragments for `chat_ui` / `chat_ui_patch` tool definitions.

use serde_json::{json, Value};

pub fn block_type_enum() -> Value {
    json!([
        "text",
        "divider",
        "section",
        "row",
        "column",
        "card",
        "image",
        "code_preview",
        "progress",
        "link_list",
        "text_input",
        "number_input",
        "slider",
        "switch",
        "date",
        "time",
        "datetime",
        "select",
        "radio",
        "checkbox",
        "tags",
        "koi_picker",
        "project_picker",
        "file_picker",
        "confirm",
        "actions"
    ])
}

pub fn block_item_schema() -> Value {
    json!({
        "type": "object",
        "required": ["type"],
        "properties": {
            "type": {
                "type": "string",
                "enum": block_type_enum()
            },
            "id": { "type": "string" },
            "label": { "type": "string" },
            "description": { "type": "string" },
            "required": { "type": "boolean" },
            "disabled": { "type": "boolean" },
            "value": {},
            "content": { "type": "string" },
            "url": { "type": "string" },
            "alt": { "type": "string" },
            "language": { "type": "string" },
            "accept": { "type": "string" },
            "multiple": { "type": "boolean" },
            "blocks": {
                "type": "array",
                "description": "Child blocks for row/column/card layout containers.",
                "items": { "type": "object" }
            },
            "options": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "value": { "type": "string" },
                        "label": { "type": "string" },
                        "description": { "type": "string" },
                        "href": { "type": "string" }
                    }
                }
            },
            "default": {},
            "placeholder": { "type": "string" },
            "allow_custom": { "type": "boolean" },
            "custom_label": { "type": "string" },
            "multiline": { "type": "boolean" },
            "rows": { "type": "integer" },
            "input_mode": {
                "type": "string",
                "enum": ["text", "email", "url", "password"]
            },
            "min_length": { "type": "integer" },
            "max_length": { "type": "integer" },
            "pattern": { "type": "string" },
            "show_when": {
                "type": "object",
                "properties": {
                    "field": { "type": "string" },
                    "equals": {},
                    "one_of": { "type": "array" },
                    "not_equals": {}
                }
            },
            "suggestions": {
                "type": "array",
                "items": { "type": "string" }
            },
            "allow_new": { "type": "boolean" },
            "min": {},
            "max": {},
            "step": { "type": "number" },
            "buttons": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["label"],
                    "properties": {
                        "id": { "type": "string" },
                        "label": { "type": "string" },
                        "value": {},
                        "style": {
                            "type": "string",
                            "enum": ["primary", "danger", "default"]
                        },
                        "emit": {
                            "type": "string",
                            "enum": ["submit", "action"]
                        }
                    }
                }
            }
        }
    })
}

pub fn ui_definition_schema() -> Value {
    json!({
        "type": "object",
        "required": ["blocks"],
        "properties": {
            "protocol_version": {
                "type": "string",
                "enum": ["1", "2"]
            },
            "mode": {
                "type": "string",
                "enum": ["form", "display", "wizard"]
            },
            "title": { "type": "string" },
            "description": { "type": "string" },
            "submit_label": { "type": "string" },
            "data": {
                "type": "object",
                "description": "v2 data model; echoed as __data_model__ on submit/action."
            },
            "blocks": {
                "type": "array",
                "items": block_item_schema()
            },
            "steps": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["blocks"],
                    "properties": {
                        "id": { "type": "string" },
                        "label": { "type": "string" },
                        "description": { "type": "string" },
                        "blocks": {
                            "type": "array",
                            "items": block_item_schema()
                        }
                    }
                }
            }
        }
    })
}

pub fn patch_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" },
            "description": { "type": "string" },
            "submit_label": { "type": "string" },
            "mode": { "type": "string", "enum": ["form", "display", "wizard"] },
            "data": { "type": "object" },
            "blocks": {
                "type": "array",
                "items": block_item_schema()
            },
            "steps": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "label": { "type": "string" },
                        "description": { "type": "string" },
                        "blocks": {
                            "type": "array",
                            "items": block_item_schema()
                        }
                    }
                }
            },
            "wizard_step": { "type": "integer" },
            "reopen_submit": {
                "type": "boolean",
                "description": "Enable submit on the card (use with chat_ui_listen)."
            }
        }
    })
}

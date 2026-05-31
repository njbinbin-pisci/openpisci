# Chat UI Protocol (v1)

Structured interaction between the **Agent** (`chat_ui` tool) and the **Desktop user**. The agent sends a `ui_definition`; the client renders a card, validates input, and returns `USER_INTERACTIVE_RESPONSE_JSON`.

## When to use `chat_ui`

| Use `chat_ui` | Use plain text instead |
|---------------|------------------------|
| Multiple fields in one step | Single yes/no |
| Constrained choices (enum, range, date) | Open-ended brainstorming |
| Koi / project pickers | Simple confirmation |
| Wizard-style steps with conditionals | |

The tool **blocks** until submit or timeout (5 minutes).

## `ui_definition` shape

```json
{
  "protocol_version": "1",
  "title": "Configure export",
  "description": "Optional markdown-friendly intro.",
  "submit_label": "Continue",
  "blocks": [ /* UiBlock[] */ ]
}
```

- `protocol_version`: must be `"1"` (omit only for legacy cards; client assumes v1).
- Every **value-bearing** block needs a unique `id` (stable key in the response JSON).
- Decorative blocks (`text`, `divider`, `section`) do not need `id`.

## Submit payload (authoritative)

```json
{
  "field_id": "user value",
  "tags": ["a", "b"],
  "enabled": true,
  "__action__": "submit",
  "__button__": { "id": "submit", "label": "Continue", "value": "submit" },
  "__meta__": {
    "protocol_version": "1",
    "request_id": "<tool_use_id>",
    "submitted_at": "2026-05-29T12:00:00.000Z"
  }
}
```

- Agent **must** treat field keys as authoritative over prior assumptions.
- `__action__` / `__button__` identify which button was pressed when using `actions` / `confirm`.
- Custom options: submit the **user-typed string**, not the sentinel `__custom__`.

## Block types

### Display (no `id`)

| type | fields | notes |
|------|--------|-------|
| `text` | `content` | Markdown-ish plain text |
| `divider` | — | Horizontal rule |
| `section` | `label`, `description?` | Section heading |

### Single value (`id` required)

| type | value type | key fields |
|------|------------|------------|
| `text_input` | `string` | `placeholder`, `default`, `min_length`, `max_length`, `pattern`, `multiline`, `rows`, `input_mode`: `text`\|`email`\|`url`\|`password` |
| `number_input` | `number` | `min`, `max`, `step`, `default` |
| `slider` | `number` | `min`, `max`, `step`, `default` |
| `switch` | `boolean` | `default` |
| `date` | `string` (ISO `YYYY-MM-DD`) | `min`, `max`, `default` |
| `time` | `string` (`HH:mm`) | `min`, `max`, `default` |
| `datetime` | `string` (ISO local) | `min`, `max`, `default` |
| `select` | `string` | `options[]`, `allow_custom`, `custom_label`, `placeholder`, `default` |
| `radio` | `string` | `options[]`, `allow_custom`, `custom_label`, `default` |
| `project_picker` | `string` | `allow_new`, `default` |

### Multi value (`id` required)

| type | value type | key fields |
|------|------------|------------|
| `checkbox` | `string[]` | `options[]`, `allow_custom`, `min` (min selections), `max` (max selections), `default` |
| `tags` | `string[]` | `options[]` (suggestions), `allow_custom`, `min`, `max`, `placeholder`, `default` |
| `koi_picker` | `string[]` | `suggestions[]`, `min`, `max`, `default` |

### Actions (no field value unless `id` set)

| type | notes |
|------|-------|
| `actions` | `buttons[]`: `{ id?, label, value?, style?: primary\|danger\|default }` |
| `confirm` | Shorthand single primary button; same as `actions` with one button |

If there are input blocks but no `actions`/`confirm`, the client shows a default **Submit** button.

## Options

```json
{ "value": "stable_id", "label": "Human label", "description": "Optional subtitle" }
```

- `value` must be stable (ASCII identifier); shown in submitted JSON.
- With `allow_custom: true`, user can pick **Other** and type a custom string (submitted as the field value or appended to the array for multi-select).

## Conditional visibility (`show_when`)

```json
"show_when": { "field": "mode", "equals": "advanced" }
"show_when": { "field": "mode", "one_of": ["a", "b"] }
"show_when": { "field": "enabled", "equals": true }
"show_when": { "field": "tags", "one_of": ["x"] }  // any overlap for arrays
```

## Validation (client-enforced)

| rule | applies to |
|------|------------|
| `required: true` | all value blocks |
| `min` / `max` | number, slider, checkbox/tags count, date/time bounds |
| `min_length` / `max_length` | text |
| `pattern` | text (JavaScript RegExp; invalid pattern ignored) |

Errors show inline; submit is blocked until valid.

## Agent authoring checklist

1. One card = one decision step; don’t overload 15 fields.
2. Always set `id` on inputs; use snake_case ids (`export_format`, not `Export Format`).
3. Provide `default` when a sensible pre-selection exists.
4. Use `options` with clear `label`; keep `value` machine-readable.
5. Use `allow_custom` when the list is indicative, not exhaustive.
6. Use `show_when` for branching instead of multiple sequential cards when possible.
7. End with `actions` (e.g. Confirm / Cancel) for destructive flows.
8. For dates use ISO dates; for times use 24h `HH:mm`.

## Example: export wizard fragment

```json
{
  "protocol_version": "1",
  "title": "Export settings",
  "blocks": [
    { "type": "section", "label": "Output" },
    {
      "type": "select",
      "id": "format",
      "label": "Format",
      "required": true,
      "default": "pdf",
      "options": [
        { "value": "pdf", "label": "PDF" },
        { "value": "docx", "label": "Word" }
      ],
      "allow_custom": true,
      "custom_label": "Other format"
    },
    {
      "type": "date",
      "id": "due_date",
      "label": "Due date",
      "min": "2026-01-01",
      "required": true
    },
    {
      "type": "checkbox",
      "id": "notify",
      "label": "Notify channels",
      "options": [
        { "value": "email", "label": "Email" },
        { "value": "im", "label": "IM" }
      ],
      "min": 1
    },
    {
      "type": "actions",
      "buttons": [
        { "id": "cancel", "label": "Cancel", "value": "cancel", "style": "default" },
        { "id": "ok", "label": "Export", "value": "export", "style": "primary" }
      ]
    }
  ]
}
```

## Protocol v2 (extends v1)

Set `protocol_version` to `"2"`. v1 cards remain valid (flat `blocks` only).

### Modes

| `mode` | Use |
|--------|-----|
| `form` | Default single-step form |
| `wizard` | Use `steps[]`; each step has its own `blocks`; optional footer `blocks` on root |
| `display` | Read-only emphasis (same blocks; agent should avoid required inputs) |

### Data model

- Optional top-level `data: { ... }` seeds field values (merged with block `default`).
- Submit/action payloads include `__data_model__` (full snapshot) plus field keys.

### Layout & display blocks

| type | notes |
|------|-------|
| `row` / `column` / `card` | Nested `blocks[]` |
| `image` | `url`, optional `alt` |
| `code_preview` | `content`, optional `language` |
| `progress` | `id`, numeric `value` via data model; `min`/`max` (default max=1) |
| `link_list` | `options[]`; sets field `id` on click |
| `file_picker` | Native dialog; value = path string |

### Non-terminal actions

Buttons may set `"emit": "action"` (vs `"submit"`):

1. User click → tool returns `USER_INTERACTIVE_RESPONSE_JSON` with `__action_type__: "action"`.
2. Agent may `chat_ui_patch` (update `data`, `blocks`, `progress`, `wizard_step`) and/or `chat_ui_listen` (re-open submit wait on same `request_id`).
3. User final submit → `__action_type__: "submit"`.

Patch tool: `chat_ui_patch { request_id, patch }`. Catalog: `docs/pisci.chat.catalog.json`.

### Wizard example

```json
{
  "protocol_version": "2",
  "mode": "wizard",
  "title": "Release",
  "steps": [
    { "id": "ver", "label": "Version", "blocks": [{ "type": "text_input", "id": "tag", "required": true }] },
    { "id": "notes", "label": "Notes", "blocks": [{ "type": "text_input", "id": "notes", "multiline": true, "rows": 5 }] }
  ],
  "blocks": [
    {
      "type": "actions",
      "buttons": [
        { "id": "preview", "label": "Preview", "value": "preview", "emit": "action" },
        { "id": "ok", "label": "Publish", "value": "publish", "style": "primary", "emit": "submit" }
      ]
    }
  ]
}
```

## Desktop implementation

- Renderer: `src/components/Chat/InteractiveCard.tsx`
- Types & validation: `src/components/Chat/interactiveUi/`
- Tools: `chat_ui`, `chat_ui_patch`, `chat_ui_listen` in `src-tauri/src/tools/`
- Catalog: `docs/pisci.chat.catalog.json`

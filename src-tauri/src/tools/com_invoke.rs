/// COM Invoke Tool — instantiate any COM/ActiveX object and call methods via PowerShell.
///
/// Why PowerShell instead of native Rust COM?
/// - Native Rust COM (windows-rs) requires knowing the interface at compile time.
/// - PowerShell's New-Object -ComObject is fully dynamic: any ProgID, any method, any property.
/// - 32-bit COM objects (WOW6432Node) require a 32-bit host process — we use SysWOW64\powershell.exe.
/// - This mirrors exactly what a human would do to automate legacy Windows software.
use crate::agent::tool::{Tool, ToolContext, ToolResult};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;
use tokio::time::timeout;

const COM_TIMEOUT_SECS: u64 = 60;

pub struct ComInvokeTool;

#[async_trait]
impl Tool for ComInvokeTool {
    fn name(&self) -> &str {
        "com_invoke"
    }

    fn description(&self) -> &str {
        "Instantiate any Windows COM/ActiveX object by ProgID and call its methods or read properties. \
         Supports both 64-bit and 32-bit (legacy) COM objects. \
         Use `arch: \"x86\"` for 32-bit COM objects registered under WOW6432Node (most legacy industrial/CAD software). \
         \
         Actions: \
         - 'create': Instantiate a COM object and optionally call a method. Returns object info. \
         - 'call_method': Create object and call a specific method with arguments. \
         - 'get_property': Create object and read a property value. \
         - 'set_property': Create object and set a property value. \
         - 'run_script': Run an arbitrary PowerShell script that uses COM objects (most flexible). \
         \
         Examples: \
         - Check if ProgID exists: action=create, prog_id=TBRuntime.TBEnvironment, arch=x86 \
         - Call a method: action=call_method, prog_id=Excel.Application, method=Workbooks.Open, args=[\"C:\\\\file.xlsx\"] \
         - Run script: action=run_script, script=\"$obj = New-Object -ComObject WScript.Shell; $obj.Popup('hello')\" \
         - 32-bit COM: action=create, prog_id=TBRuntime.TBEnvironment, arch=x86"
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "call_method", "get_property", "set_property", "run_script"],
                    "description": "Action to perform"
                },
                "prog_id": {
                    "type": "string",
                    "description": "COM ProgID (e.g. 'Excel.Application', 'TBRuntime.TBEnvironment')"
                },
                "method": {
                    "type": "string",
                    "description": "Method name to call (for call_method). Supports dot notation: 'Workbooks.Open'"
                },
                "property": {
                    "type": "string",
                    "description": "Property name to get/set (for get_property, set_property)"
                },
                "value": {
                    "description": "Value to set (for set_property)"
                },
                "args": {
                    "type": "array",
                    "description": "Arguments to pass to the method (for call_method)",
                    "items": {}
                },
                "arch": {
                    "type": "string",
                    "enum": ["x64", "x86"],
                    "description": "Architecture: 'x64' = 64-bit PowerShell (default), 'x86' = 32-bit PowerShell (required for WOW6432Node COM objects)"
                },
                "script": {
                    "type": "string",
                    "description": "Full PowerShell script to run (for run_script action). Most flexible option."
                },
                "timeout": {
                    "type": "integer",
                    "description": "Timeout in seconds (default 60)"
                }
            },
            "required": ["action"]
        })
    }

    fn needs_confirmation(&self, input: &Value) -> bool {
        // set_property and run_script can have side effects
        matches!(
            input["action"].as_str(),
            Some("set_property") | Some("run_script") | Some("call_method")
        )
    }

    async fn call(&self, input: Value, _ctx: &ToolContext) -> Result<ToolResult> {
        let action = match input["action"].as_str() {
            Some(a) => a,
            None => return Ok(ToolResult::err("Missing required parameter: action")),
        };

        let arch = input["arch"].as_str().unwrap_or("x64");
        let timeout_secs = input["timeout"].as_u64().unwrap_or(COM_TIMEOUT_SECS);

        let script = match action {
            "run_script" => match input["script"].as_str() {
                Some(s) => s.to_string(),
                None => return Ok(ToolResult::err("run_script requires 'script' parameter")),
            },
            "create" => {
                let prog_id = match input["prog_id"].as_str() {
                    Some(p) => p,
                    None => return Ok(ToolResult::err("create requires 'prog_id'")),
                };
                format!(
                    r#"
$ErrorActionPreference = 'Stop'
try {{
    $obj = New-Object -ComObject '{prog_id}'
    $type = $obj.GetType()
    $members = $type.GetMembers() | Select-Object -First 30 | ForEach-Object {{ $_.Name }}
    @{{
        success = $true
        prog_id = '{prog_id}'
        type_name = $type.FullName
        members = $members
    }} | ConvertTo-Json -Depth 3
}} catch {{
    @{{
        success = $false
        prog_id = '{prog_id}'
        error = $_.Exception.Message
        hresult = '0x{{0:X8}}' -f $_.Exception.HResult
    }} | ConvertTo-Json
}}
"#,
                    prog_id = prog_id
                )
            }
            "call_method" => {
                let prog_id = match input["prog_id"].as_str() {
                    Some(p) => p,
                    None => return Ok(ToolResult::err("call_method requires 'prog_id'")),
                };
                let method = match input["method"].as_str() {
                    Some(m) => m,
                    None => return Ok(ToolResult::err("call_method requires 'method'")),
                };
                let args_ps = build_ps_args(&input["args"]);
                // Support dot-notation methods like "Workbooks.Open"
                let invoke_expr = if method.contains('.') {
                    let parts: Vec<&str> = method.splitn(2, '.').collect();
                    format!("$obj.{}.{}({})", parts[0], parts[1], args_ps)
                } else {
                    format!("$obj.{}({})", method, args_ps)
                };
                format!(
                    r#"
$ErrorActionPreference = 'Stop'
try {{
    $obj = New-Object -ComObject '{prog_id}'
    $result = {invoke_expr}
    # Try ConvertTo-Json first for rich objects; fall back to ToString for simple values
    $resultValue = $null
    $resultType = 'null'
    if ($result -ne $null) {{
        $resultType = $result.GetType().Name
        try {{
            $jsonTest = $result | ConvertTo-Json -Depth 3 -ErrorAction Stop
            $resultValue = $result | ConvertTo-Json -Depth 3 -Compress
        }} catch {{
            $resultValue = $result.ToString()
        }}
    }}
    @{{
        success = $true
        method = '{method}'
        result_type = $resultType
        result = $resultValue
    }} | ConvertTo-Json -Depth 4
}} catch {{
    @{{
        success = $false
        method = '{method}'
        error = $_.Exception.Message
        hresult = '0x{{0:X8}}' -f $_.Exception.HResult
    }} | ConvertTo-Json
}}
"#,
                    prog_id = prog_id,
                    method = method,
                    invoke_expr = invoke_expr
                )
            }
            "get_property" => {
                let prog_id = match input["prog_id"].as_str() {
                    Some(p) => p,
                    None => return Ok(ToolResult::err("get_property requires 'prog_id'")),
                };
                let property = match input["property"].as_str() {
                    Some(p) => p,
                    None => return Ok(ToolResult::err("get_property requires 'property'")),
                };
                format!(
                    r#"
$ErrorActionPreference = 'Stop'
try {{
    $obj = New-Object -ComObject '{prog_id}'
    $val = $obj.{property}
    $valStr = $null
    $valType = 'null'
    if ($val -ne $null) {{
        $valType = $val.GetType().Name
        try {{
            $valStr = $val | ConvertTo-Json -Depth 3 -Compress -ErrorAction Stop
        }} catch {{
            $valStr = $val.ToString()
        }}
    }}
    @{{
        success = $true
        property = '{property}'
        value = $valStr
        type = $valType
    }} | ConvertTo-Json
}} catch {{
    @{{
        success = $false
        property = '{property}'
        error = $_.Exception.Message
    }} | ConvertTo-Json
}}
"#,
                    prog_id = prog_id,
                    property = property
                )
            }
            "set_property" => {
                let prog_id = match input["prog_id"].as_str() {
                    Some(p) => p,
                    None => return Ok(ToolResult::err("set_property requires 'prog_id'")),
                };
                let property = match input["property"].as_str() {
                    Some(p) => p,
                    None => return Ok(ToolResult::err("set_property requires 'property'")),
                };
                let value_ps = json_to_ps_value(&input["value"]);
                format!(
                    r#"
$ErrorActionPreference = 'Stop'
try {{
    $obj = New-Object -ComObject '{prog_id}'
    $obj.{property} = {value_ps}
    @{{ success = $true; property = '{property}'; message = 'Property set' }} | ConvertTo-Json
}} catch {{
    @{{ success = $false; error = $_.Exception.Message }} | ConvertTo-Json
}}
"#,
                    prog_id = prog_id,
                    property = property,
                    value_ps = value_ps
                )
            }
            _ => return Ok(ToolResult::err(format!("Unknown action: {}", action))),
        };

        run_com_script(&script, arch, timeout_secs).await
    }
}

fn build_ps_args(args: &Value) -> String {
    match args.as_array() {
        None => String::new(),
        Some(arr) if arr.is_empty() => String::new(),
        Some(arr) => arr
            .iter()
            .map(json_to_ps_value)
            .collect::<Vec<_>>()
            .join(", "),
    }
}

fn json_to_ps_value(v: &Value) -> String {
    match v {
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => {
            if *b {
                "$true".to_string()
            } else {
                "$false".to_string()
            }
        }
        Value::Null => "$null".to_string(),
        _ => format!("'{}'", v.to_string().replace('\'', "''")),
    }
}

async fn run_com_script(script: &str, arch: &str, timeout_secs: u64) -> Result<ToolResult> {
    let utf8_preamble = "[Console]::OutputEncoding=[System.Text.Encoding]::UTF8;\
                         $OutputEncoding=[System.Text.Encoding]::UTF8;\
                         chcp 65001 | Out-Null; ";
    let full_script = format!("{}{}", utf8_preamble, script);

    let ps_exe = if arch == "x86" {
        // 32-bit PowerShell — required for WOW6432Node COM objects
        r"C:\Windows\SysWOW64\WindowsPowerShell\v1.0\powershell.exe"
    } else {
        "powershell"
    };

    let mut cmd = Command::new(ps_exe);
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &full_script])
        .current_dir("C:\\")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // CREATE_NO_WINDOW: prevents a blue console window from flashing on screen
    #[cfg(target_os = "windows")]
    cmd.creation_flags(0x0800_0000);

    let result = timeout(Duration::from_secs(timeout_secs), cmd.output()).await;

    match result {
        Err(_) => Ok(ToolResult::err(format!(
            "COM script timed out after {}s",
            timeout_secs
        ))),
        Ok(Err(e)) => Ok(ToolResult::err(format!(
            "Failed to spawn PowerShell: {}",
            e
        ))),
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let exit_code = output.status.code().unwrap_or(-1);

            let mut parts = vec![format!("Exit code: {}", exit_code)];
            if !stdout.is_empty() {
                parts.push(format!("OUTPUT:\n{}", stdout));
            }
            if !stderr.is_empty() {
                parts.push(format!("STDERR:\n{}", stderr));
            }
            if stdout.is_empty() && stderr.is_empty() {
                parts.push("(no output)".to_string());
            }

            // Always ok — let LLM parse the JSON result's "success" field
            Ok(ToolResult::ok(parts.join("\n\n")))
        }
    }
}

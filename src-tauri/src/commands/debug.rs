/// Debug & E2E testing module for OpenPisci.
///
/// Provides:
/// - `run_debug_scenario`: Run a named test scenario through the real agent loop
/// - `get_debug_report`: Collect a full diagnostic snapshot (settings, tools, recent audit, logs)
/// - `get_log_tail`: Read the last N lines of the rolling log file
use crate::agent::loop_::AgentLoop;
use crate::agent::messages::AgentEvent;
use crate::agent::tool::ToolContext;
use crate::llm::{build_client, LlmMessage, MessageContent};
use crate::policy::PolicyGate;
use crate::store::AppState;
use crate::tools;
use serde::{Deserialize, Serialize};
use std::sync::{atomic::AtomicBool, Arc};
use tauri::{Manager, State};
use tracing::info;

// ─── Data types ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugScenario {
    pub id: String,
    pub name: String,
    pub description: String,
    /// English name for i18n display
    pub name_en: String,
    /// English description for i18n display
    pub description_en: String,
    pub prompt: String,
    /// Expected keywords that should appear in the result (for pass/fail judgement)
    pub expected_keywords: Vec<String>,
    /// Tools that should be called during this scenario
    pub expected_tools: Vec<String>,
    /// Configuration prerequisites. If any are not satisfied the scenario is shown as
    /// unavailable in the UI and skipped automatically.
    /// Supported values: "ssh_servers" (at least one SSH server configured)
    #[serde(default)]
    pub requires_config: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub tool_name: String,
    pub input_summary: String,
    pub result_summary: String,
    pub is_error: bool,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScenarioResult {
    pub scenario_id: String,
    pub scenario_name: String,
    pub passed: bool,
    pub response_text: String,
    pub tool_calls: Vec<ToolCallRecord>,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub missing_keywords: Vec<String>,
    pub missing_tools: Vec<String>,
    pub unexpected_tool_errors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugReport {
    pub timestamp: String,
    pub system_info: SystemInfo,
    pub settings_summary: SettingsSummary,
    pub available_tools: Vec<String>,
    pub recent_audit: Vec<crate::store::db::AuditEntry>,
    pub recent_errors: Vec<String>,
    pub log_tail: Vec<String>,
    pub scenario_results: Vec<ScenarioResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub os: String,
    pub provider: String,
    pub model: String,
    pub workspace_root: String,
    pub policy_mode: String,
    pub max_iterations: u32,
    pub tool_rate_limit: u32,
    pub api_key_configured: bool,
    pub vision_enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsSummary {
    pub provider: String,
    pub model: String,
    pub workspace_root: String,
    pub policy_mode: String,
    pub max_tokens: u32,
    pub max_iterations: u32,
    pub confirm_shell: bool,
    pub confirm_file_write: bool,
    pub enabled_tools: Vec<String>,
    pub disabled_tools: Vec<String>,
}

// ─── Built-in test scenarios ──────────────────────────────────────────────────

pub fn builtin_scenarios() -> Vec<DebugScenario> {
    vec![
        // ── P0: Core connectivity ──────────────────────────────────────────────
        DebugScenario {
            id: "ping".into(),
            name: "基础连通性测试".into(),
            name_en: "LLM Connectivity".into(),
            description: "验证 LLM 连接是否正常，不调用任何工具".into(),
            description_en: "Verify LLM connection is working, no tools called".into(),
            prompt: "请回复：PONG".into(),
            expected_keywords: vec!["PONG".into()],
            expected_tools: vec![],
            requires_config: None,
        },
        DebugScenario {
            id: "file_write_read".into(),
            name: "文件写入/读取".into(),
            name_en: "File Write & Read".into(),
            description: "写入一个测试文件，然后读取并验证内容".into(),
            description_en: "Write a test file then read it back and verify content".into(),
            prompt: "请用 file_write 工具在工作区（见 Debug context）写文件 debug_test.txt，内容为 'PISCI_DEBUG_OK'.\
                     然后用 file_read 工具读取该文件，告诉我文件内容.\
                     注意：使用工作区路径下的 debug_test.txt，不要使用其他路径。".into(),
            expected_keywords: vec!["PISCI_DEBUG_OK".into()],
            expected_tools: vec!["file_write".into(), "file_read".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "file_edit".into(),
            name: "文件精确编辑".into(),
            name_en: "File Edit (Patch)".into(),
            description: "创建文件后用 file_edit 精确替换其中一段文字，验证编辑工具可用".into(),
            description_en: "Create a file then use file_edit to replace a specific string, verifying patch tool works".into(),
            prompt: "请完成以下步骤:\
                     1) 用 file_write 工具在工作区（见 Debug context）写文件 edit_test.txt，内容为 'Hello World';\
                     2) 用 file_edit 工具把文件中的 'World' 替换为 'Pisci';\
                     3) 用 file_read 读取文件，在回复中原文引用文件内容（例如：文件内容为：Hello Pisci）。".into(),
            expected_keywords: vec!["Hello Pisci".into()],
            expected_tools: vec!["file_write".into(), "file_edit".into(), "file_read".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "file_search_glob".into(),
            name: "文件搜索 (glob)".into(),
            name_en: "File Search (glob)".into(),
            description: "用 file_search 工具按文件名模式搜索文件".into(),
            description_en: "Use file_search to find files by name pattern (glob)".into(),
            prompt: "请用 file_search 工具在 C:\\Windows\\System32 目录下搜索所有 *.exe 文件（max_results=5），告诉我找到了哪些文件".into(),
            expected_keywords: vec![".exe".into()],
            expected_tools: vec!["file_search".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "file_list".into(),
            name: "目录列表".into(),
            name_en: "Directory Listing".into(),
            description: "用 file_list 工具列出目录内容，验证结构化目录读取".into(),
            description_en: "Use file_list to list directory contents as structured JSON".into(),
            prompt: "请用 file_list 工具列出 C:\\Windows 目录的内容（不递归），告诉我有哪些子目录".into(),
            expected_keywords: vec!["System32".into()],
            expected_tools: vec!["file_list".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "shell_echo".into(),
            name: "Shell 命令执行".into(),
            name_en: "Shell Command".into(),
            description: "执行一个简单的 echo 命令，验证 shell 工具是否可用".into(),
            description_en: "Run a simple echo command to verify the shell tool works".into(),
            prompt: "请用 shell 工具执行命令：echo SHELL_OK，并告诉我输出".into(),
            expected_keywords: vec!["SHELL_OK".into()],
            expected_tools: vec!["shell".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "shell_sysinfo".into(),
            name: "系统信息查询".into(),
            name_en: "System Info Query".into(),
            description: "用 shell 查询 CPU、内存、磁盘等基本系统信息".into(),
            description_en: "Query CPU, memory, and disk info via shell".into(),
            prompt: "请用 shell 工具查询：1) CPU 型号（Get-WmiObject Win32_Processor | Select Name）；2) 内存总量（Get-WmiObject Win32_ComputerSystem | Select TotalPhysicalMemory）。告诉我结果".into(),
            expected_keywords: vec![],
            expected_tools: vec!["shell".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "shell_process".into(),
            name: "进程列表查询".into(),
            name_en: "Process List".into(),
            description: "查询当前运行的进程列表，验证 shell 的系统查询能力".into(),
            description_en: "List running processes to verify shell system query capability".into(),
            prompt: "请用 shell 工具列出当前占用内存最多的 5 个进程（Get-Process | Sort-Object WorkingSet -Descending | Select -First 5 Name,Id），告诉我结果".into(),
            expected_keywords: vec![],
            expected_tools: vec!["shell".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "web_search".into(),
            name: "网络搜索".into(),
            name_en: "Web Search".into(),
            description: "执行混合网络搜索（SearXNG + 本地引擎），验证网络访问是否正常".into(),
            description_en: "Run a hybrid web search (SearXNG + local engines) to verify network access".into(),
            prompt: "请搜索 'Windows 11 最新版本' 并告诉我找到了什么".into(),
            expected_keywords: vec![],
            expected_tools: vec!["web_search".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "powershell_query".into(),
            name: "PowerShell 结构化查询".into(),
            name_en: "PowerShell Structured Query".into(),
            description: "用 powershell_query 工具查询系统信息，返回结构化 JSON".into(),
            description_en: "Query system info via powershell_query tool, returns structured JSON".into(),
            prompt: "请用 powershell_query 工具查询当前系统的 Windows 版本（query: get_system_info），告诉我操作系统版本".into(),
            expected_keywords: vec![],
            expected_tools: vec!["powershell_query".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "powershell_installed_apps".into(),
            name: "已安装软件查询".into(),
            name_en: "Installed Apps Query".into(),
            description: "查询系统已安装的软件列表，验证注册表读取能力".into(),
            description_en: "Query installed software list to verify registry read capability".into(),
            prompt: "请用 powershell_query 工具查询已安装的软件（query: get_installed_apps），列出前 5 个软件名称".into(),
            expected_keywords: vec![],
            expected_tools: vec!["powershell_query".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "wmi_hardware".into(),
            name: "WMI 硬件信息".into(),
            name_en: "WMI Hardware Info".into(),
            description: "用 WMI 查询 CPU 和内存硬件信息".into(),
            description_en: "Query CPU and memory hardware info via WMI".into(),
            prompt: "请用 wmi 工具查询 CPU 信息（preset: cpu），告诉我 CPU 型号和核心数".into(),
            expected_keywords: vec![],
            expected_tools: vec!["wmi".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "process_control".into(),
            name: "进程控制".into(),
            name_en: "Process Control".into(),
            description: "检查进程是否在运行，验证 process_control 工具可用".into(),
            description_en: "Check if a process is running to verify process_control tool".into(),
            prompt: "请用 process_control 工具检查 explorer.exe 是否在运行（action: is_running, name: explorer）.\
                     工具返回 JSON，请告诉我其中 running 字段的值（true 或 false），以及 PID 列表.\
                     回复中必须包含英文单词 running（例如：running: true）。".into(),
            expected_keywords: vec!["running".into()],
            expected_tools: vec!["process_control".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "multi_step".into(),
            name: "多步骤任务".into(),
            name_en: "Multi-Step Task".into(),
            description: "验证 Agent 能否完成需要多个工具调用的任务".into(),
            description_en: "Verify Agent can complete tasks requiring multiple tool calls".into(),
            prompt: "请严格按顺序完成以下步骤:\
                     Step 1: 用 shell 工具执行命令 'echo STEP1_OK'，记录输出.\
                     Step 2: 用 file_write 工具在工作区（见 Debug context）写文件 multi_step_test.txt,\
                             内容只写 ASCII 英文：MULTI_STEP_DONE（不要写任何中文）.\
                     Step 3: 用 file_read 工具读取 multi_step_test.txt，告诉我文件内容.\
                     最终回复格式：Step1输出=STEP1_OK, 文件内容=MULTI_STEP_DONE".into(),
            expected_keywords: vec!["MULTI_STEP_DONE".into()],
            expected_tools: vec!["shell".into(), "file_write".into(), "file_read".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "file_search_grep".into(),
            name: "文件内容搜索 (grep)".into(),
            name_en: "File Content Search (grep)".into(),
            description: "在文件内容中搜索关键词，验证 grep 功能".into(),
            description_en: "Search file contents for keywords to verify grep functionality".into(),
            prompt: "请完成以下步骤:\
                     1) 用 file_write 工具在工作区（见 Debug context）写文件 grep_test.txt,\
                        内容为三行：line1: hello\nline2: world\nline3: pisci_grep_ok\n;\
                     2) 用 file_search 工具（action: grep）在工作区目录搜索关键词 'pisci_grep_ok';\
                     3) 告诉我搜索结果。".into(),
            expected_keywords: vec!["pisci_grep_ok".into()],
            expected_tools: vec!["file_write".into(), "file_search".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "memory_store".into(),
            name: "记忆存储与检索".into(),
            name_en: "Memory Store & Search".into(),
            description: "保存一条记忆，然后搜索验证记忆功能可用".into(),
            description_en: "Save a memory entry then search it to verify memory tool works".into(),
            prompt: "请用 memory_store 工具保存一条记忆（action: save, content: 'DEBUG_MEMORY_TEST_OK', category: fact），然后立即用 search 动作搜索 'DEBUG_MEMORY_TEST'，告诉我找到了什么".into(),
            expected_keywords: vec!["DEBUG_MEMORY_TEST_OK".into()],
            expected_tools: vec!["memory_store".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "screen_capture".into(),
            name: "屏幕截图".into(),
            name_en: "Screen Capture".into(),
            description: "截取当前屏幕，验证截图工具可用".into(),
            description_en: "Capture the current screen to verify screen capture tool works".into(),
            prompt: "请用 screen_capture 工具截取当前屏幕（action: fullscreen）,\
                     告诉我截图的宽度和高度各是多少（用 x 分隔，例如：1920x1080）。".into(),
            expected_keywords: vec!["x".into()],
            expected_tools: vec!["screen_capture".into()],
            requires_config: None,
        },
        // ── Context management tests ──────────────────────────────────────────
        DebugScenario {
            id: "ctx_tool_persistence".into(),
            name: "工具调用持久化".into(),
            name_en: "Tool Call Persistence".into(),
            description: "验证工具调用记录被正确写入数据库（tool_calls_json 非空）".into(),
            description_en: "Verify tool call records are persisted to DB with tool_calls_json populated".into(),
            prompt: "请用 file_write 工具在工作区（见 Debug context）写文件 ctx_persist_test.txt，内容为 'CTX_PERSIST_OK'.\
                     然后用 file_read 工具读取它，确认内容正确.\
                     最后告诉我：写入的内容是什么？".into(),
            expected_keywords: vec!["CTX_PERSIST_OK".into()],
            expected_tools: vec!["file_write".into(), "file_read".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "ctx_multi_turn_memory".into(),
            name: "跨轮上下文记忆".into(),
            name_en: "Cross-Turn Context Memory".into(),
            description: "验证多轮对话中 LLM 能正确引用前几轮的工具结果".into(),
            description_en: "Verify the LLM can reference tool results from previous turns across conversation rounds".into(),
            prompt: "这是一个多步骤测试:\
                     第一步：用 file_write 工具在工作区（见 Debug context）写文件 ctx_turn_test.txt，内容为 'TURN_VALUE_42'.\
                     第二步：用 file_read 工具读取该文件.\
                     第三步：告诉我文件中的数字是多少（只需回答数字）.\
                     注意：请严格按顺序执行这三步，最终只输出文件中的数字。".into(),
            expected_keywords: vec!["42".into()],
            expected_tools: vec!["file_write".into(), "file_read".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "ctx_compression_check".into(),
            name: "上下文压缩验证".into(),
            name_en: "Context Compression Check".into(),
            description: "验证大量工具输出被正确裁剪，不超出上下文预算".into(),
            description_en: "Verify large tool outputs are trimmed correctly and context stays within budget".into(),
            prompt: "请执行以下操作并报告结果:\
                     1. 用 powershell_query 工具执行 action: query，query: 'Get-Process | Select-Object Name,Id,CPU | ConvertTo-Json' 获取进程列表\
                     2. 用 file_list 工具列出 C:\\Windows\\System32 目录（不递归)\
                     3. 告诉我：系统中运行了多少个进程（大约），System32 目录中有多少个文件/文件夹？".into(),
            expected_keywords: vec!["进程".into(), "文件".into()],
            expected_tools: vec!["powershell_query".into(), "file_list".into()],
            requires_config: None,
        },
        DebugScenario {
            id: "ctx_trim_verify".into(),
            name: "工具结果裁剪验证".into(),
            name_en: "Tool Result Trim Verify".into(),
            description: "验证中间轮的大型工具输出经 head+tail 裁剪，保留关键信息".into(),
            description_en: "Verify that large tool outputs in middle turns are trimmed with head+tail strategy".into(),
            prompt: "请分两步完成:\
                     第一步：用 shell 工具执行命令 'dir C:\\Windows\\System32 /b' 获取文件列表（这会产生大量输出）.\
                     第二步：从上面的输出中，告诉我列表中的前 3 个文件名是什么？\
                     注意：只需告诉我前 3 个文件名，不需要完整列表。".into(),
            expected_keywords: vec![".".into()],
            expected_tools: vec!["shell".into()],
            requires_config: None,
        },

        // ── Office 操作 ────────────────────────────────────────────────────────

        DebugScenario {
            id: "excel_create_write".into(),
            name: "Excel 创建与写入".into(),
            name_en: "Excel Create & Write".into(),
            description: "创建 Excel 文件并批量写入数据，验证 office write_cells 动作".into(),
            description_en: "Create an Excel file and batch-write data to verify office write_cells action".into(),
            prompt: "请用 office 工具完成以下操作:\
                     1. 用 action=create, app=excel 在 C:\\Users\\Public\\debug_excel_test.xlsx 创建一个新 Excel 文件.\
                     2. 用 action=write_cells, app=excel 向该文件写入以下数据：A1=姓名, B1=分数, A2=张三, B2=95, A3=李四, B3=87, A4=王五, B4=92.\
                     3. 用 action=read_range, app=excel 读取 A1:B4 范围，确认数据正确.\
                     4. 最后告诉我读取到的数据内容。".into(),
            expected_keywords: vec!["姓名".into(), "分数".into()],
            expected_tools: vec!["office".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "excel_chart".into(),
            name: "Excel 折线图".into(),
            name_en: "Excel Line Chart".into(),
            description: "在 Excel 中创建折线图，验证 add_chart 动作中 chart_type=line 正确生效".into(),
            description_en: "Create a line chart in Excel to verify add_chart with chart_type=line".into(),
            prompt: "请用 office 工具完成以下操作:\
                     1. 用 action=create, app=excel 在 C:\\Users\\Public\\debug_chart_test.xlsx 创建新文件.\
                     2. 用 action=write_cells 写入月度数据：A1=月份, B1=销量, A2=1月, B2=100, A3=2月, B3=120, A4=3月, B4=115, A5=4月, B5=130.\
                     3. 用 action=add_chart, chart_type=line, range=A1:B5, chart_title=月度销量趋势 添加折线图.\
                     4. 告诉我操作结果，确认是折线图（line chart）而非其他类型。".into(),
            expected_keywords: vec!["折线".into(), "line".into()],
            expected_tools: vec!["office".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "word_create".into(),
            name: "Word 文档创建".into(),
            name_en: "Word Document Create".into(),
            description: "创建 Word 文档并写入标题和段落，验证 office Word 动作".into(),
            description_en: "Create a Word document with title and paragraphs to verify office Word actions".into(),
            prompt: "请用 office 工具完成以下操作:\
                     1. 用 action=create, app=word 在 C:\\Users\\Public\\debug_word_test.docx 创建一个 Word 文档.\
                     2. 用 action=add_paragraph, style=Heading 1 添加标题：调试测试报告.\
                     3. 用 action=add_paragraph, style=Normal 添加正文：本文档由调试面板自动生成，用于验证 Word 文档创建功能.\
                     4. 用 action=add_paragraph, style=Heading 2 添加二级标题：测试结果.\
                     5. 用 action=save 保存文档.\
                     6. 告诉我操作是否成功，文件保存在哪里。".into(),
            expected_keywords: vec!["成功".into(), "debug_word_test.docx".into()],
            expected_tools: vec!["office".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "ppt_create".into(),
            name: "PPT 演示文稿创建".into(),
            name_en: "PowerPoint Presentation Create".into(),
            description: "创建 PPT 并批量添加幻灯片，验证 office PowerPoint add_slides 动作".into(),
            description_en: "Create a PowerPoint presentation with multiple slides to verify add_slides action".into(),
            prompt: "请用 office 工具完成以下操作:\
                     1. 用 action=create, app=powerpoint 在 C:\\Users\\Public\\debug_ppt_test.pptx 创建一个 PPT 文件.\
                     2. 用 action=add_slides 批量添加 3 张幻灯片，内容如下:\
                        slides=[{title: '调试测试', content: '这是第一张幻灯片'}, {title: '功能验证', content: '验证 PPT 创建功能正常'}, {title: '测试完成', content: '所有测试通过'}].\
                     3. 用 action=get_slide_count 获取幻灯片数量，确认是 3 张.\
                     4. 告诉我幻灯片数量和操作结果。".into(),
            expected_keywords: vec!["3".into(), "幻灯片".into()],
            expected_tools: vec!["office".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "office_read".into(),
            name: "Excel 数据读取".into(),
            name_en: "Excel Data Read".into(),
            description: "读取已有 Excel 文件的数据范围，验证 office read_range 动作".into(),
            description_en: "Read data range from an existing Excel file to verify read_range action".into(),
            prompt: "请用 office 工具完成以下操作:\
                     1. 先用 action=create, app=excel 在 C:\\Users\\Public\\debug_read_test.xlsx 创建文件.\
                     2. 用 action=write_cells 写入：A1=产品, B1=价格, C1=库存, A2=苹果, B2=5.5, C2=100, A3=香蕉, B3=3.2, C3=200.\
                     3. 用 action=read_range, range=A1:C3 读取全部数据.\
                     4. 用 action=get_sheet_names 获取工作表名称列表.\
                     5. 告诉我读取到的数据和工作表名称。".into(),
            expected_keywords: vec!["产品".into(), "价格".into()],
            expected_tools: vec!["office".into()],
            requires_config: None,
        },

        // ── 邮件收发 ───────────────────────────────────────────────────────────

        DebugScenario {
            id: "outlook_send".into(),
            name: "Outlook 发送邮件".into(),
            name_en: "Outlook Send Email".into(),
            description: "通过 Outlook COM 接口发送测试邮件，若未安装 Outlook 则回复 SKIP".into(),
            description_en: "Send a test email via Outlook COM interface, reply SKIP if Outlook is not installed".into(),
            prompt: "请检查本机是否安装了 Microsoft Outlook，然后:\
                     如果未安装 Outlook，请直接回复：SKIP - Outlook 未安装.\
                     如果已安装，请用 office 工具执行 action=send_email, app=outlook,\
                     收件人填写 test@example.com（仅测试，实际不会发送）,\
                     主题为「调试测试邮件」，正文为「这是一封来自 OpenPisci 调试面板的测试邮件」.\
                     告诉我操作结果。".into(),
            expected_keywords: vec!["SKIP".into(), "成功".into(), "邮件".into()],
            expected_tools: vec![],
            requires_config: None,
        },

        DebugScenario {
            id: "email_send_smtp".into(),
            name: "SMTP 邮件发送".into(),
            name_en: "SMTP Email Send".into(),
            description: "通过 PowerShell Send-MailMessage 发送 SMTP 测试邮件，未配置时回复 SKIP".into(),
            description_en: "Send SMTP test email via PowerShell Send-MailMessage, reply SKIP if not configured".into(),
            prompt: "请用 powershell_query 工具检查系统是否有可用的 SMTP 邮件配置（检查环境变量 SMTP_HOST 和 SMTP_USER 是否存在）.\
                     如果没有配置，请回复：SKIP - SMTP 未配置.\
                     如果有配置，请用 shell 工具通过 PowerShell 的 Send-MailMessage 命令发送一封测试邮件,\
                     服务器使用环境变量 SMTP_HOST，用户名使用 SMTP_USER，收件人使用 SMTP_USER（发给自己）.\
                     告诉我操作结果。".into(),
            expected_keywords: vec!["SKIP".into(), "成功".into(), "SMTP".into(), "smtp".into(), "邮件".into(), "未配置".into(), "发送".into()],
            expected_tools: vec!["powershell_query".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "email_read_imap".into(),
            name: "IMAP 邮件读取".into(),
            name_en: "IMAP Email Read".into(),
            description: "通过 PowerShell 读取 IMAP 收件箱，未配置时回复 SKIP".into(),
            description_en: "Read IMAP inbox via PowerShell, reply SKIP if not configured".into(),
            prompt: "请用 powershell_query 工具检查系统是否有 IMAP 邮件配置（检查环境变量 IMAP_HOST 和 IMAP_USER 是否存在）.\
                     如果没有配置，请回复：SKIP - IMAP 未配置.\
                     如果有配置，请尝试用 PowerShell 连接 IMAP 服务器读取最新 5 封邮件的主题列表.\
                     告诉我操作结果或邮件主题列表。".into(),
            expected_keywords: vec!["SKIP".into(), "邮件".into(), "IMAP".into()],
            expected_tools: vec!["powershell_query".into()],
            requires_config: None,
        },

        // ── 技能市场 ───────────────────────────────────────────────────────────

        DebugScenario {
            id: "skill_search".into(),
            name: "技能市场搜索".into(),
            name_en: "Skill Market Search".into(),
            description: "搜索 Clawhub 技能市场，验证网络访问和技能列表获取".into(),
            description_en: "Search Clawhub skill market to verify network access and skill listing".into(),
            prompt: "请用 web_search 工具搜索「OpenPisci 技能 site:github.com」,\
                     或者直接用 shell 工具执行 curl 或 PowerShell Invoke-WebRequest 访问 https://api.github.com/search/repositories?q=pisci+skill,\
                     获取相关技能仓库列表.\
                     告诉我找到了哪些相关仓库或技能，以及它们的名称和描述。".into(),
            expected_keywords: vec!["github".into(), "skill".into()],
            expected_tools: vec!["web_search".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "skill_install".into(),
            name: "技能安装测试".into(),
            name_en: "Skill Install Test".into(),
            description: "模拟安装一个简单技能（创建技能目录和配置文件），验证技能安装流程".into(),
            description_en: "Simulate installing a simple skill by creating skill directory and config, verify install flow".into(),
            prompt: "请模拟安装一个测试技能，步骤如下:\
                     1. 用 file_list 工具列出工作区根目录下的 skills 和 user_tools 目录（如果存在）.\
                     2. 用 file_write 工具在工作区创建一个测试技能文件 debug_test_skill/skill.json，内容为:\
                        {\"name\": \"debug_test_skill\", \"version\": \"1.0.0\", \"description\": \"调试测试技能\", \"author\": \"debug\"}.\
                     3. 用 file_read 工具读取刚创建的文件，确认内容正确.\
                     4. 告诉我技能文件创建是否成功，内容是否正确。".into(),
            expected_keywords: vec!["debug_test_skill".into(), "成功".into()],
            expected_tools: vec!["file_write".into(), "file_read".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "skill_use".into(),
            name: "技能调用测试".into(),
            name_en: "Skill Use Test".into(),
            description: "列出已安装的用户工具并尝试调用，验证技能调用链路".into(),
            description_en: "List installed user tools and attempt to invoke one to verify skill invocation chain".into(),
            prompt: "请完成以下操作:\
                     1. 用 powershell_query 工具执行 Get-ChildItem 列出工作区目录中所有 .json 文件，找出技能配置文件.\
                     2. 如果找到了技能配置文件，读取其中一个并告诉我技能名称和描述.\
                     3. 如果没有找到任何技能，请回复：SKIP - 未安装任何用户技能.\
                     4. 告诉我当前已安装的技能列表（或 SKIP 原因）。".into(),
            expected_keywords: vec!["SKIP".into(), "技能".into(), "skill".into(), "Skill".into(), "未安装".into(), "找到".into(), "json".into(), "JSON".into()],
            expected_tools: vec!["powershell_query".into()],
            requires_config: None,
        },

        // ── 小鱼（Fish）─────────────────────────────────────────────────────────

        DebugScenario {
            id: "fish_list".into(),
            name: "小鱼列表".into(),
            name_en: "Fish List".into(),
            description: "列出所有已配置的小鱼（子智能体），验证小鱼配置读取".into(),
            description_en: "List all configured Fish (sub-agents) to verify Fish config reading".into(),
            prompt: "请用 file_list 工具列出小鱼（Fish）配置目录，步骤如下:\
                     1. 用 file_list 列出 fish 目录（路径：%APPDATA%\\com.pisci.desktop\\fish），查看有哪些 FISH.toml 文件.\
                     2. 如果目录不存在或为空，直接回复：SKIP - 未配置小鱼（当前没有安装任何 Fish 子智能体）.\
                     3. 如果找到了 FISH.toml 文件，告诉我找到了多少个小鱼（Fish），以及它们的名称.\
                     注意：无论结果如何，最终回复中必须包含小鱼和Fish这两个词。".into(),
            expected_keywords: vec!["SKIP".into(), "小鱼".into(), "Fish".into(), "未配置".into()],
            expected_tools: vec!["file_list".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "fish_invoke".into(),
            name: "小鱼调用测试".into(),
            name_en: "Fish Invoke Test".into(),
            description: "调用一个内置小鱼完成简单任务，验证 call_fish 工具链路".into(),
            description_en: "Invoke a built-in Fish sub-agent to complete a simple task, verify call_fish tool chain".into(),
            prompt: "请用 call_fish 工具调用一个小鱼完成简单任务.\
                     如果没有可用的小鱼，请回复：SKIP - 没有可用的小鱼.\
                     如果有可用的小鱼，请选择其中一个，调用它完成任务：「请告诉我今天是星期几」.\
                     告诉我调用结果，包括小鱼名称和返回内容。".into(),
            expected_keywords: vec!["SKIP".into(), "星期".into(), "小鱼".into(), "fish".into(), "Fish".into(), "Monday".into(), "Tuesday".into(), "Wednesday".into(), "Thursday".into(), "Friday".into(), "Saturday".into(), "Sunday".into(), "一".into()],
            expected_tools: vec![],
            requires_config: None,
        },

        // ── UIA 自动化 ─────────────────────────────────────────────────────────

        DebugScenario {
            id: "uia_list_windows".into(),
            name: "UIA 窗口列表".into(),
            name_en: "UIA Window List".into(),
            description: "列出所有顶层窗口，验证 uia 工具的 list_windows 动作".into(),
            description_en: "List all top-level windows to verify uia tool list_windows action".into(),
            prompt: "请用 uia 工具执行 action=list_windows，列出当前所有顶层窗口.\
                     告诉我当前打开了哪些窗口（列出窗口标题和进程名），至少列出 3 个。".into(),
            expected_keywords: vec!["窗口".into()],
            expected_tools: vec!["uia".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "uia_get_element".into(),
            name: "UIA 元素获取".into(),
            name_en: "UIA Element Get".into(),
            description: "打开记事本并获取其 UI 元素，验证 uia 工具的元素查找功能".into(),
            description_en: "Open Notepad and get its UI elements to verify uia element finding".into(),
            prompt: "请完成以下步骤:\
                     1. 用 shell 工具执行 'start notepad.exe' 打开记事本.\
                     2. 等待 2 秒（用 shell 执行 Start-Sleep -Seconds 2）.\
                     3. 用 uia 工具执行 action=list_windows，找到记事本窗口（标题包含「记事本」或「Notepad」）.\
                     4. 用 uia 工具执行 action=get_element，获取记事本编辑区域的元素信息.\
                     5. 用 shell 工具执行 'taskkill /f /im notepad.exe' 关闭记事本.\
                     6. 告诉我记事本的 UI 元素结构。".into(),
            expected_keywords: vec!["记事本".into(), "Notepad".into()],
            expected_tools: vec!["shell".into(), "uia".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "uia_click_type".into(),
            name: "UIA 点击与输入".into(),
            name_en: "UIA Click & Type".into(),
            description: "在记事本中定位并输入文字，验证 uia 鼠标定位和键盘输入精度".into(),
            description_en: "Locate and type text in Notepad to verify uia mouse positioning and keyboard input accuracy".into(),
            prompt: "请完成以下步骤（这是 UIA 键盘输入测试）:\
                     1. 用 shell 工具执行 'start notepad.exe' 打开记事本.\
                     2. 用 shell 工具执行 'Start-Sleep -Seconds 2' 等待 2 秒.\
                     3. 用 uia 工具执行 action=list_windows，找到记事本窗口（标题含「记事本」或「Notepad」）.\
                     4. 用 uia 工具执行 action=find，参数 window_title 设为记事本窗口标题，control_type=Edit，找到编辑区域.\
                     5. 用 uia 工具执行 action=click，点击编辑区域使其获得焦点.\
                     6. 用 uia 工具执行 action=type_text，参数 text=\"UIA定位测试-OpenPisci\"，向编辑区域输入文字.\
                     7. 用 uia 工具执行 action=get_text，读取编辑区域内容（如果返回空，尝试 get_value）.\
                     8. 用 shell 工具执行 'taskkill /f /im notepad.exe' 关闭记事本.\
                     9. 在最终回复中必须包含以下内容:\
                        - 你输入的文字原文：「UIA定位测试-OpenPisci」\
                        - 读取结果（成功读取到的内容，或说明读取为空但输入步骤已执行)\
                        - 鼠标点击是否成功".into(),
            expected_keywords: vec!["UIA定位测试".into(), "OpenPisci".into()],
            expected_tools: vec!["shell".into(), "uia".into()],
            requires_config: None,
        },

        // ── 多模态 ─────────────────────────────────────────────────────────────

        DebugScenario {
            id: "multimodal_image_describe".into(),
            name: "多模态图像描述".into(),
            name_en: "Multimodal Image Describe".into(),
            description: "截取屏幕截图后让模型描述图像内容，验证多模态能力（不支持则回复 SKIP）".into(),
            description_en: "Take a screenshot and ask the model to describe it, verify multimodal capability (SKIP if not supported)".into(),
            prompt: "请完成以下多模态测试:\
                     1. 用 screen_capture 工具截取当前屏幕截图.\
                     2. 尝试分析截图内容，描述你在图像中看到的主要元素（窗口、文字、颜色等）.\
                     3. 如果当前模型不支持图像输入（多模态），请回复：SKIP - 当前模型不支持多模态图像输入.\
                     4. 如果支持，请描述截图中的主要内容，至少提及 3 个可见元素。".into(),
            expected_keywords: vec!["SKIP".into(), "图像".into(), "截图".into(), "窗口".into()],
            expected_tools: vec!["screen_capture".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "multimodal_image_read_file".into(),
            name: "多模态本地图片读取".into(),
            name_en: "Multimodal Local Image Read".into(),
            description: "读取本地图片文件并让模型描述，验证图片文件多模态处理（不支持则回复 SKIP）".into(),
            description_en: "Read a local image file and ask the model to describe it, verify multimodal file handling (SKIP if not supported)".into(),
            prompt: "请完成以下多模态文件测试:\
                     1. 用 file_list 工具列出 C:\\Users\\Public 目录下的图片文件（.png, .jpg, .jpeg, .bmp）.\
                     2. 如果找到图片文件，用 file_read 工具读取其中一张.\
                     3. 如果当前模型支持多模态，描述图片内容；如果不支持，回复：SKIP - 当前模型不支持多模态.\
                     4. 如果没有找到图片文件，先用 screen_capture 截图保存，再读取描述.\
                     告诉我图片内容描述或 SKIP 原因。".into(),
            expected_keywords: vec!["SKIP".into(), "图片".into(), "图像".into(), "image".into(), "Image".into(), "png".into(), "PNG".into(), "描述".into(), "内容".into()],
            expected_tools: vec!["file_list".into()],
            requires_config: None,
        },

        // ── 浏览器 ─────────────────────────────────────────────────────────────

        DebugScenario {
            id: "browser_headless_search".into(),
            name: "Headless 浏览器搜索".into(),
            name_en: "Headless Browser Search".into(),
            description: "使用 headless 浏览器访问搜索引擎，验证 browser 工具的无头模式".into(),
            description_en: "Use headless browser to access a search engine, verify browser tool headless mode".into(),
            prompt: "请用 browser 工具（headless 模式）完成以下操作:\
                     1. 访问 https://www.bing.com.\
                     2. 在搜索框中输入「OpenPisci AI Agent」并提交搜索.\
                     3. 获取搜索结果页面的标题和前 3 条结果的标题.\
                     4. 告诉我搜索结果.\
                     注意：使用 headless=true 模式，不要打开可见浏览器窗口。".into(),
            expected_keywords: vec!["搜索".into(), "结果".into(), "bing".into(), "Bing".into()],
            expected_tools: vec!["browser".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "browser_headless_screenshot".into(),
            name: "Headless 浏览器截图".into(),
            name_en: "Headless Browser Screenshot".into(),
            description: "使用 headless 浏览器访问 URL 并截图，验证页面渲染能力".into(),
            description_en: "Use headless browser to visit a URL and take a screenshot, verify page rendering".into(),
            prompt: "请用 browser 工具（headless 模式）完成以下操作:\
                     1. 访问 https://example.com.\
                     2. 等待页面加载完成.\
                     3. 截取页面截图并保存到 C:\\Users\\Public\\debug_browser_screenshot.png.\
                     4. 获取页面标题和主要文字内容.\
                     5. 告诉我页面标题和主要内容，以及截图是否保存成功。".into(),
            expected_keywords: vec!["Example".into(), "页面".into(), "example.com".into(), "标题".into()],
            expected_tools: vec!["browser".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "browser_open_user".into(),
            name: "打开用户浏览器".into(),
            name_en: "Open User Browser".into(),
            description: "用 shell 命令打开用户默认浏览器访问指定 URL，验证系统浏览器集成".into(),
            description_en: "Open user's default browser to a URL via shell command, verify system browser integration".into(),
            prompt: "请用 shell 工具执行以下操作:\
                     1. 执行 PowerShell 命令 'Start-Process \"https://example.com\"' 打开用户默认浏览器访问 example.com.\
                     2. 等待 2 秒.\
                     3. 用 uia 工具执行 action=list_windows，检查是否有浏览器窗口出现（标题包含 Chrome、Edge、Firefox 等）.\
                     4. 告诉我浏览器是否成功打开，以及检测到的浏览器窗口信息。".into(),
            expected_keywords: vec!["浏览器".into(), "打开".into()],
            expected_tools: vec!["shell".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "browser_login_hint".into(),
            name: "浏览器登录场景".into(),
            name_en: "Browser Login Scenario".into(),
            description: "打开需要登录的页面，验证 browser 工具在需要用户介入时的处理方式".into(),
            description_en: "Open a login-required page to verify browser tool handling when user intervention is needed".into(),
            prompt: "请用 browser 工具完成以下登录场景测试:\
                     1. 访问 https://github.com/login 登录页面.\
                     2. 检查页面是否有登录表单（用户名/密码输入框）.\
                     3. 不要实际登录，只需确认登录表单存在并获取页面标题.\
                     4. 告诉我页面标题、是否检测到登录表单，以及如果需要真实登录应如何操作（提示用户手动介入）。".into(),
            expected_keywords: vec!["登录".into(), "GitHub".into(), "表单".into(), "login".into(), "github".into()],
            expected_tools: vec!["browser".into()],
            requires_config: None,
        },

        // ── IM 网关 ────────────────────────────────────────────────────────────

        DebugScenario {
            id: "im_feishu_config_check".into(),
            name: "飞书配置检查".into(),
            name_en: "Feishu Config Check".into(),
            description: "检查飞书 IM 网关配置是否完整，验证 App ID、App Secret 等关键配置项".into(),
            description_en: "Check if Feishu IM gateway configuration is complete, verify App ID, App Secret etc.".into(),
            prompt: "请用 app_control 工具（action=settings_get）读取当前应用设置，检查飞书（Feishu/Lark）IM 网关的配置状态:\
                     1. 调用 app_control(action=settings_get) 获取当前设置.\
                     2. 检查返回结果中是否包含飞书相关配置（feishu_app_id、feishu_app_secret 等字段是否非空）.\
                     3. 根据检查结果，必须在回复中输出以下两种标记之一（原文输出，不要修改）:\
                        - 如果飞书已配置（app_id 非空）：输出「FEISHU_CONFIGURED」\
                        - 如果飞书未配置（app_id 为空或缺失）：输出「FEISHU_NOT_CONFIGURED」\
                     4. 同时说明哪些配置项已设置、哪些缺失（不要输出具体密钥值）。".into(),
            expected_keywords: vec!["FEISHU_CONFIGURED".into(), "FEISHU_NOT_CONFIGURED".into()],
            expected_tools: vec!["app_control".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "im_send_text".into(),
            name: "IM 发送文本消息".into(),
            name_en: "IM Send Text Message".into(),
            description: "通过飞书 IM 发送测试文本消息，验证消息发送链路（需配置飞书且有测试收件人）".into(),
            description_en: "Send a test text message via Feishu IM to verify message sending chain (requires Feishu config and test recipient)".into(),
            prompt: "请检查飞书 IM 配置，然后:\
                     1. 用 powershell_query 检查环境变量 FEISHU_APP_ID 是否存在.\
                     2. 如果未配置，回复：SKIP - 飞书未配置.\
                     3. 如果已配置，用 powershell_query 检查环境变量 FEISHU_TEST_USER 是否存在（测试收件人）.\
                     4. 如果测试收件人未配置，回复：SKIP - 未配置测试收件人 FEISHU_TEST_USER.\
                     5. 如果两者都已配置，用 shell 工具调用飞书 API 发送一条测试消息（内容：「OpenPisci 调试测试消息 - 请忽略」）给 FEISHU_TEST_USER.\
                     告诉我操作结果。".into(),
            expected_keywords: vec!["SKIP".into(), "飞书".into(), "消息".into(), "Feishu".into(), "feishu".into(), "未配置".into(), "发送".into(), "配置".into()],
            expected_tools: vec!["powershell_query".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "im_send_file".into(),
            name: "IM 发送文件".into(),
            name_en: "IM Send File".into(),
            description: "创建文件后通过 IM 发送，验证文件发送链路（SEND_FILE 指令）".into(),
            description_en: "Create a file and send it via IM to verify file sending chain (SEND_FILE instruction)".into(),
            prompt: "请完成以下 IM 文件发送测试:\
                     1. 用 powershell_query 检查环境变量 FEISHU_APP_ID 和 FEISHU_TEST_USER 是否存在.\
                     2. 如果未配置，回复：SKIP - 飞书未配置或未设置测试收件人.\
                     3. 如果已配置，用 office 工具创建一个简单的 Excel 文件 C:\\Users\\Public\\debug_im_test.xlsx,\
                        写入内容：A1=测试, B1=数据, A2=IM, B2=发送测试.\
                     4. 在回复中包含发送指令（单独一行）：SEND_FILE:C:\\Users\\Public\\debug_im_test.xlsx\
                     5. 告诉我文件创建和发送操作的结果。".into(),
            expected_keywords: vec!["SKIP".into(), "debug_im_test.xlsx".into(), "飞书".into(), "未配置".into(), "xlsx".into(), "配置".into()],
            expected_tools: vec!["powershell_query".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "im_dingtalk_config".into(),
            name: "钉钉配置检查".into(),
            name_en: "DingTalk Config Check".into(),
            description: "检查钉钉 IM 网关配置是否完整，验证 App Key、App Secret 等关键配置项".into(),
            description_en: "Check if DingTalk IM gateway configuration is complete, verify App Key, App Secret etc.".into(),
            prompt: "请用 app_control 工具（action=settings_get）读取当前应用设置，检查钉钉（DingTalk）IM 网关的配置状态:\
                     1. 调用 app_control(action=settings_get) 获取当前设置.\
                     2. 检查返回结果中是否包含钉钉相关配置（dingtalk_app_key、dingtalk_app_secret 等字段是否非空）.\
                     3. 根据检查结果，必须在回复中输出以下两种标记之一（原文输出，不要修改）:\
                        - 如果钉钉已配置（app_key 非空）：输出「DINGTALK_CONFIGURED」\
                        - 如果钉钉未配置（app_key 为空或缺失）：输出「DINGTALK_NOT_CONFIGURED」\
                     4. 同时说明哪些配置项已设置、哪些缺失（不要输出具体密钥值）。".into(),
            expected_keywords: vec!["DINGTALK_CONFIGURED".into(), "DINGTALK_NOT_CONFIGURED".into()],
            expected_tools: vec!["app_control".into()],
            requires_config: None,
        },

        // ── 定时任务 ───────────────────────────────────────────────────────────

        DebugScenario {
            id: "scheduler_list".into(),
            name: "定时任务列表".into(),
            name_en: "Scheduler Task List".into(),
            description: "列出所有已配置的定时任务，验证调度器配置读取".into(),
            description_en: "List all configured scheduled tasks to verify scheduler config reading".into(),
            prompt: "请用 app_control 工具列出所有已配置的定时任务:\
                     1. 调用 app_control(action=task_list)，获取所有定时任务列表.\
                     2. 如果没有定时任务，回复：SCHEDULER_EMPTY - 当前没有配置任何定时任务.\
                     3. 如果有定时任务，列出每个任务的 ID、名称、Cron 表达式、状态和上次运行时间.\
                     4. 最终回复中必须包含「SCHEDULER_OK」标记（无论有无任务）。".into(),
            expected_keywords: vec!["SCHEDULER_OK".into(), "SCHEDULER_EMPTY".into()],
            expected_tools: vec!["app_control".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "scheduler_create_delete".into(),
            name: "定时任务创建与删除".into(),
            name_en: "Scheduler Create & Delete".into(),
            description: "创建一个测试定时任务并随后删除，验证定时任务的完整生命周期".into(),
            description_en: "Create a test scheduled task and then delete it to verify the full task lifecycle".into(),
            prompt: "请用 shell 工具完成以下 Windows 定时任务测试:\
                     1. 用 PowerShell 创建一个测试定时任务:\
                        $action = New-ScheduledTaskAction -Execute 'notepad.exe'; \
                        $trigger = New-ScheduledTaskTrigger -Once -At '2099-01-01 00:00:00'; \
                        Register-ScheduledTask -TaskName 'OpenPisciDebugTest' -Action $action -Trigger $trigger -Force.\
                     2. 用 PowerShell 查询该任务是否创建成功：Get-ScheduledTask -TaskName 'OpenPisciDebugTest'.\
                     3. 用 PowerShell 删除该任务：Unregister-ScheduledTask -TaskName 'OpenPisciDebugTest' -Confirm:$false.\
                     4. 确认任务已被删除.\
                     5. 告诉我任务创建和删除是否都成功。".into(),
            expected_keywords: vec!["OpenPisciDebugTest".into(), "成功".into(), "删除".into()],
            expected_tools: vec!["shell".into()],
            requires_config: None,
        },

        // ── SSH ───────────────────────────────────────────────────────────────

        DebugScenario {
            id: "ssh_connect_exec".into(),
            name: "SSH 临时连接测试".into(),
            name_en: "SSH Ad-hoc Connect".into(),
            description: "通过环境变量 SSH_TEST_HOST 等临时参数连接 SSH 服务器并执行命令；未配置环境变量时回复 SKIP".into(),
            description_en: "Connect to an SSH server using SSH_TEST_HOST env vars and execute a command; reply SKIP if env vars are not set".into(),
            prompt: "请用 ssh 工具完成以下 SSH 连接测试:\
                     1. 先用 powershell_query 检查环境变量 SSH_TEST_HOST 是否存在（执行：$env:SSH_TEST_HOST）.\
                     2. 如果未设置，回复：SKIP - 未配置 SSH 测试环境变量 SSH_TEST_HOST.\
                     3. 如果已设置，读取以下环境变量：SSH_TEST_HOST（主机）、SSH_TEST_USER（用户名，默认 root）、SSH_TEST_PASSWORD（密码）、SSH_TEST_PORT（端口，默认 22）.\
                     4. 用 ssh 工具 action=connect 建立连接，connection_id 设为 'debug-test'.\
                     5. 连接成功后，用 ssh 工具 action=exec 执行命令：echo SSH_EXEC_OK && uname -a.\
                     6. 用 ssh 工具 action=disconnect 断开连接.\
                     7. 告诉我连接是否成功、命令输出内容，以及是否包含 SSH_EXEC_OK。".into(),
            expected_keywords: vec!["SKIP".into(), "SSH_EXEC_OK".into(), "连接".into(), "成功".into()],
            expected_tools: vec!["powershell_query".into(), "ssh".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "ssh_via_settings".into(),
            name: "SSH 预配置服务器".into(),
            name_en: "SSH Pre-configured Server".into(),
            description: "使用「设置 → SSH 服务器」中的预配置服务器连接并执行命令，验证凭据存储与自动查找功能；未配置服务器时回复 SKIP".into(),
            description_en: "Connect using a pre-configured server from Settings > SSH Servers; reply SKIP if none are configured".into(),
            prompt: "请用 app_control 工具和 ssh 工具完成以下预配置服务器测试:\
                     1. 先用 app_control(action=settings_get) 获取当前设置，检查 ssh_servers 列表是否有配置的服务器.\
                     2. 如果 ssh_servers 为空或不存在，直接回复：SKIP - 未在「设置 → SSH 服务器」中配置任何服务器.\
                     3. 如果有配置，取第一台服务器的 connection_id（别名），用 ssh 工具 action=connect，只传 connection_id（不传 host/password），让工具自动从设置中读取凭据.\
                     4. 连接成功后，用 ssh 工具 action=exec 执行命令：echo SSH_SETTINGS_OK.\
                     5. 用 ssh 工具 action=disconnect 断开连接.\
                     6. 最终回复必须包含：SSH_SETTINGS_OK（命令执行成功）或 SKIP（未配置）或详细错误信息（连接失败）。".into(),
            expected_keywords: vec!["SSH_SETTINGS_OK".into(), "SKIP".into()],
            expected_tools: vec!["app_control".into()],
            requires_config: None,
        },

        // ── 心跳与系统资源 ─────────────────────────────────────────────────────

        DebugScenario {
            id: "heartbeat_basic".into(),
            name: "基础心跳".into(),
            name_en: "Basic Heartbeat".into(),
            description: "最简单的 LLM 响应测试，验证 API 连通性和响应延迟，不调用任何工具".into(),
            description_en: "Simplest LLM response test to verify API connectivity and response latency without any tool calls".into(),
            prompt: "请直接回复以下内容（不要调用任何工具）:\
                     「心跳正常 - OpenPisci 运行中。当前时间：[你知道的当前时间]。状态：OK」\
                     这是一个心跳检测，只需要文字回复，不需要执行任何操作。".into(),
            expected_keywords: vec!["心跳正常".into(), "OK".into()],
            expected_tools: vec![],
            requires_config: None,
        },

        DebugScenario {
            id: "heartbeat_tools".into(),
            name: "工具链路心跳".into(),
            name_en: "Tool Chain Heartbeat".into(),
            description: "通过文件写入/读取往返验证工具调用链路的完整性和延迟".into(),
            description_en: "Verify tool call chain integrity and latency via file write/read round-trip".into(),
            prompt: "请完成以下工具链路心跳测试:\
                     1. 用 file_write 工具将字符串 HEARTBEAT_OK 写入文件 C:\\Users\\Public\\debug_heartbeat.txt.\
                     2. 用 file_read 工具立即读取该文件，确认内容包含 HEARTBEAT_OK.\
                     3. 完成后，你的最终回复必须包含这两个词（原样输出，不要翻译）：HEARTBEAT_OK 和 工具链路正常。".into(),
            expected_keywords: vec!["工具链路正常".into(), "HEARTBEAT_OK".into()],
            expected_tools: vec!["file_write".into(), "file_read".into()],
            requires_config: None,
        },

        DebugScenario {
            id: "system_resource".into(),
            name: "系统资源监控".into(),
            name_en: "System Resource Monitor".into(),
            description: "获取当前系统 CPU 使用率、内存占用、磁盘空间等资源信息".into(),
            description_en: "Get current system CPU usage, memory usage, disk space and other resource metrics".into(),
            prompt: "请用 powershell_query 工具获取以下系统资源信息:\
                     1. CPU 使用率：执行 Get-WmiObject Win32_Processor | Select-Object LoadPercentage.\
                     2. 内存使用：执行 Get-WmiObject Win32_OperatingSystem | Select-Object TotalVisibleMemorySize,FreePhysicalMemory.\
                     3. 磁盘空间：执行 Get-WmiObject Win32_LogicalDisk | Select-Object DeviceID,Size,FreeSpace.\
                     4. 整理以上信息，告诉我：CPU 使用率是多少？总内存和可用内存各是多少 GB？C 盘总空间和剩余空间各是多少 GB？".into(),
            expected_keywords: vec!["CPU".into(), "内存".into(), "磁盘".into()],
            expected_tools: vec!["powershell_query".into()],
            requires_config: None,
        },
    ]
}

// ─── Commands ─────────────────────────────────────────────────────────────────

/// List all available debug scenarios.
#[tauri::command]
pub async fn list_debug_scenarios() -> Result<Vec<DebugScenario>, String> {
    Ok(builtin_scenarios())
}

/// Run a single named scenario through the real agent loop.
/// Returns a detailed `ScenarioResult` with pass/fail, tool calls, errors, etc.
#[tauri::command]
pub async fn run_debug_scenario(
    state: State<'_, AppState>,
    scenario_id: String,
) -> Result<ScenarioResult, String> {
    let scenarios = builtin_scenarios();
    let scenario = scenarios
        .iter()
        .find(|s| s.id == scenario_id)
        .ok_or_else(|| format!("Unknown scenario: {}", scenario_id))?
        .clone();

    info!(
        "Running debug scenario: {} ({})",
        scenario.id, scenario.name
    );

    let (
        provider,
        model,
        api_key,
        base_url,
        workspace_root,
        max_tokens,
        policy_mode,
        tool_rate_limit_per_minute,
        tool_settings,
        max_iterations,
        builtin_tool_enabled,
        ssh_servers_count,
    ) = {
        let settings = state.settings.lock().await;
        (
            settings.provider.clone(),
            settings.model.clone(),
            settings.active_api_key().to_string(),
            settings.custom_base_url.clone(),
            settings.workspace_root.clone(),
            settings.max_tokens,
            settings.policy_mode.clone(),
            settings.tool_rate_limit_per_minute,
            Arc::new(crate::agent::tool::ToolSettings::from_settings(&settings)),
            settings.max_iterations,
            settings.builtin_tool_enabled.clone(),
            settings.ssh_servers.len(),
        )
    };

    if api_key.is_empty() {
        return Ok(ScenarioResult {
            scenario_id: scenario.id.clone(),
            scenario_name: scenario.name.clone(),
            passed: false,
            response_text: String::new(),
            tool_calls: vec![],
            error: Some("API key not configured".into()),
            duration_ms: 0,
            input_tokens: 0,
            output_tokens: 0,
            missing_keywords: scenario.expected_keywords.clone(),
            missing_tools: scenario.expected_tools.clone(),
            unexpected_tool_errors: vec![],
        });
    }

    // Check configuration prerequisites
    for req in scenario.requires_config.as_deref().unwrap_or(&[]) {
        let satisfied = match req.as_str() {
            "ssh_servers" => ssh_servers_count > 0,
            _ => true,
        };
        if !satisfied {
            let hint = match req.as_str() {
                "ssh_servers" => "请先在「设置 → SSH 服务器」中添加至少一台服务器",
                _ => "请先完成相关配置",
            };
            return Ok(ScenarioResult {
                scenario_id: scenario.id.clone(),
                scenario_name: scenario.name.clone(),
                passed: true, // SKIP counts as pass — not a failure
                response_text: format!("SKIPPED: 缺少必要配置 '{}'。{}", req, hint),
                tool_calls: vec![],
                error: None,
                duration_ms: 0,
                input_tokens: 0,
                output_tokens: 0,
                missing_keywords: vec![],
                missing_tools: vec![],
                unexpected_tool_errors: vec![],
            });
        }
    }

    let start = std::time::Instant::now();
    let session_id = format!("debug_{}", scenario.id);

    let client = build_client(
        &provider,
        &api_key,
        if base_url.is_empty() {
            None
        } else {
            Some(&base_url)
        },
    );

    let user_tools_dir = state
        .app_handle
        .path()
        .app_data_dir()
        .map(|d: std::path::PathBuf| d.join("user-tools"))
        .ok();
    let app_data_dir_d = state.app_handle.path().app_data_dir().ok();

    let registry = Arc::new(tools::build_registry(
        state.browser.clone(),
        user_tools_dir.as_deref(),
        Some(state.db.clone()),
        Some(&builtin_tool_enabled),
        Some(state.app_handle.clone()),
        Some(state.settings.clone()),
        app_data_dir_d,
        None, // skill_search not used in debug scenarios
    ));

    let policy = Arc::new(PolicyGate::with_profile(
        &workspace_root,
        &policy_mode,
        tool_rate_limit_per_minute,
    ));

    // For debug runs, fall back to the system temp directory when workspace is not configured.
    // This ensures file_write/file_read tests always have a writable location.
    let effective_workspace = if workspace_root.is_empty() {
        std::env::temp_dir()
    } else {
        std::path::PathBuf::from(&workspace_root)
    };

    // Build a focused system prompt that names the exact tools to use
    let tools_hint = if scenario.expected_tools.is_empty() {
        "Do NOT call any tools for this task.".to_string()
    } else {
        format!(
            "For this task, use ONLY these tools: {}. \
             Do not call any other tools. \
             If a tool returns a non-zero exit code, read the output and report the result — do not retry with a different tool.",
            scenario.expected_tools.join(", ")
        )
    };

    let agent = AgentLoop {
        client,
        registry,
        policy,
        system_prompt: format!(
            "You are Pisci, a Windows AI Agent running a debug/test scenario.\n\
             Scenario: {}\n\
             Today's date: {}\n\
             Workspace directory: {}\n\
             When writing files, use the workspace directory above as the base path for relative filenames.\n\
             {}\n\
             Execute the task precisely. Do not ask for confirmation. \
             Complete the task in as few tool calls as possible.",
            scenario.name,
            chrono::Utc::now().format("%Y-%m-%d"),
            effective_workspace.display(),
            tools_hint
        ),
        model,
        max_tokens,
        db: Some(state.db.clone()),
        app_handle: None,
        confirmation_responses: None,
        confirm_flags: crate::agent::loop_::ConfirmFlags {
            confirm_shell: false,
            confirm_file_write: false,
        },
        vision_override: None,
        notification_rx: None,
    };

    let ctx = ToolContext {
        session_id: session_id.clone(),
        workspace_root: effective_workspace.clone(),
        bypass_permissions: false,
        settings: tool_settings,
        max_iterations: Some(max_iterations.min(10)),
        memory_owner_id: "pisci".to_string(),
        pool_session_id: None,
    };

    // Inject the effective workspace path into the prompt so the agent knows where to write files.
    let workspace_note = format!(
        "\n[Debug context: workspace = {}]",
        effective_workspace.display()
    );
    let prompt_with_workspace = format!("{}{}", scenario.prompt, workspace_note);

    let messages = vec![LlmMessage {
        role: "user".into(),
        content: MessageContent::text(&prompt_with_workspace),
    }];

    let cancel = Arc::new(AtomicBool::new(false));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);

    // Collect tool call records from events
    let tool_records: Arc<tokio::sync::Mutex<Vec<ToolCallRecord>>> =
        Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let tool_records_clone = tool_records.clone();

    // Track in-flight tool start times
    let tool_starts: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<String, (String, std::time::Instant, serde_json::Value)>,
        >,
    > = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let tool_starts_clone = tool_starts.clone();

    let event_collector = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                AgentEvent::ToolStart { id, name, input } => {
                    let summary = summarize_input(&name, &input);
                    tool_starts_clone
                        .lock()
                        .await
                        .insert(id, (name, std::time::Instant::now(), input));
                    let _ = summary; // suppress unused warning
                }
                AgentEvent::ToolEnd {
                    id,
                    name,
                    result,
                    is_error,
                } => {
                    let duration_ms = {
                        let mut starts = tool_starts_clone.lock().await;
                        starts
                            .remove(&id)
                            .map(|(_, t, input)| {
                                let dur = t.elapsed().as_millis() as u64;
                                (dur, summarize_input(&name, &input))
                            })
                            .unwrap_or((0, String::new()))
                    };
                    let result_summary: String = result.chars().take(500).collect();
                    tool_records_clone.lock().await.push(ToolCallRecord {
                        tool_name: name,
                        input_summary: duration_ms.1,
                        result_summary,
                        is_error,
                        duration_ms: duration_ms.0,
                    });
                }
                _ => {}
            }
        }
    });

    let run_result = agent.run(messages, event_tx, cancel, ctx).await;
    let _ = event_collector.await;

    let duration_ms = start.elapsed().as_millis() as u64;
    let tool_calls = tool_records.lock().await.clone();

    match run_result {
        Ok((final_messages, total_in, total_out)) => {
            let response_text = final_messages
                .iter()
                .rev()
                .find(|m| m.role == "assistant")
                .map(|m| m.content.as_text())
                .unwrap_or_default();

            // Check expected keywords (case-insensitive, OR logic: at least one must appear).
            // Search both the LLM response text AND all tool call results/inputs,
            // so scenarios pass even when the LLM paraphrases instead of quoting verbatim.
            // An empty keyword list means "no keyword check required".
            let response_lower = response_text.to_lowercase();
            let tool_text_lower: String = tool_calls
                .iter()
                .map(|t| format!("{} {}", t.result_summary, t.input_summary))
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            let missing_keywords: Vec<String> = if scenario.expected_keywords.is_empty() {
                vec![]
            } else {
                let any_found = scenario.expected_keywords.iter().any(|kw| {
                    let kw_lower = kw.to_lowercase();
                    response_lower.contains(&kw_lower) || tool_text_lower.contains(&kw_lower)
                });
                if any_found {
                    vec![]
                } else {
                    scenario.expected_keywords.clone()
                }
            };

            // Check expected tools were called
            let called_tools: Vec<String> =
                tool_calls.iter().map(|t| t.tool_name.clone()).collect();
            let missing_tools: Vec<String> = scenario
                .expected_tools
                .iter()
                .filter(|t| !called_tools.contains(t))
                .cloned()
                .collect();

            // Collect unexpected tool calls (tools called that are NOT in expected_tools)
            // Shell tool non-zero exit codes are no longer marked is_error, so we check
            // for tools called outside the expected set instead.
            let unexpected_tool_errors: Vec<String> = if scenario.expected_tools.is_empty() {
                vec![]
            } else {
                tool_calls
                    .iter()
                    .filter(|t| t.is_error && !scenario.expected_tools.contains(&t.tool_name))
                    .map(|t| format!("{}: {}", t.tool_name, t.result_summary))
                    .collect()
            };

            let passed = missing_keywords.is_empty()
                && missing_tools.is_empty()
                && unexpected_tool_errors.is_empty();

            info!(
                "Debug scenario '{}' completed: passed={} duration={}ms tools={} errors={}",
                scenario.id,
                passed,
                duration_ms,
                tool_calls.len(),
                unexpected_tool_errors.len()
            );

            Ok(ScenarioResult {
                scenario_id: scenario.id,
                scenario_name: scenario.name,
                passed,
                response_text,
                tool_calls,
                error: None,
                duration_ms,
                input_tokens: total_in,
                output_tokens: total_out,
                missing_keywords,
                missing_tools,
                unexpected_tool_errors,
            })
        }
        Err(e) => {
            let err_str = e.to_string();
            tracing::warn!("Debug scenario '{}' failed: {}", scenario.id, err_str);
            Ok(ScenarioResult {
                scenario_id: scenario.id,
                scenario_name: scenario.name,
                passed: false,
                response_text: String::new(),
                tool_calls,
                error: Some(err_str),
                duration_ms,
                input_tokens: 0,
                output_tokens: 0,
                missing_keywords: scenario.expected_keywords,
                missing_tools: scenario.expected_tools,
                unexpected_tool_errors: vec![],
            })
        }
    }
}

/// Run all built-in scenarios and return a full diagnostic report.
#[tauri::command]
pub async fn run_all_debug_scenarios(
    state: State<'_, AppState>,
) -> Result<Vec<ScenarioResult>, String> {
    let scenarios = builtin_scenarios();
    let mut results = Vec::new();
    for scenario in &scenarios {
        let result = run_debug_scenario(state.clone(), scenario.id.clone()).await?;
        results.push(result);
    }
    Ok(results)
}

/// Collect a full diagnostic report without running scenarios.
#[tauri::command]
pub async fn get_debug_report(state: State<'_, AppState>) -> Result<DebugReport, String> {
    let timestamp = chrono::Utc::now().to_rfc3339();

    // Settings summary
    let (settings_summary, system_info) = {
        let settings = state.settings.lock().await;
        let api_key_configured = settings.is_configured();

        let enabled_tools: Vec<String> = settings
            .builtin_tool_enabled
            .iter()
            .filter(|(_, &v)| v)
            .map(|(k, _)| k.clone())
            .collect();
        let disabled_tools: Vec<String> = settings
            .builtin_tool_enabled
            .iter()
            .filter(|(_, &v)| !v)
            .map(|(k, _)| k.clone())
            .collect();

        let summary = SettingsSummary {
            provider: settings.provider.clone(),
            model: settings.model.clone(),
            workspace_root: settings.workspace_root.clone(),
            policy_mode: settings.policy_mode.clone(),
            max_tokens: settings.max_tokens,
            max_iterations: settings.max_iterations,
            confirm_shell: settings.confirm_shell_commands,
            confirm_file_write: settings.confirm_file_writes,
            enabled_tools,
            disabled_tools,
        };

        let info = SystemInfo {
            os: std::env::consts::OS.into(),
            provider: settings.provider.clone(),
            model: settings.model.clone(),
            workspace_root: settings.workspace_root.clone(),
            policy_mode: settings.policy_mode.clone(),
            max_iterations: settings.max_iterations,
            tool_rate_limit: settings.tool_rate_limit_per_minute,
            api_key_configured,
            vision_enabled: settings.vision_enabled,
        };

        (summary, info)
    };

    // Available tools
    let available_tools = {
        let settings = state.settings.lock().await;
        let registry = tools::build_registry(
            state.browser.clone(),
            None,
            None,
            Some(&settings.builtin_tool_enabled),
            None,
            None,
            None,
            None,
        );
        registry
            .all()
            .iter()
            .map(|t| t.name().to_string())
            .collect()
    };

    // Recent audit log (last 20 entries)
    let recent_audit = {
        let db = state.db.lock().await;
        db.get_audit_log(None, None, 20, 0).unwrap_or_default()
    };

    // Recent errors from audit log
    let recent_errors: Vec<String> = recent_audit
        .iter()
        .filter(|e| e.is_error)
        .map(|e| {
            format!(
                "[{}] {} �?{}",
                e.tool_name,
                e.action,
                e.result_summary.as_deref().unwrap_or("(no detail)")
            )
        })
        .collect();

    // Log tail
    let log_tail = read_log_tail(50);

    Ok(DebugReport {
        timestamp,
        system_info,
        settings_summary,
        available_tools,
        recent_audit,
        recent_errors,
        log_tail,
        scenario_results: vec![],
    })
}

/// Read the last N lines of today's log file.
#[tauri::command]
pub async fn get_log_tail(lines: Option<usize>) -> Result<Vec<String>, String> {
    Ok(read_log_tail(lines.unwrap_or(100)))
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn summarize_input(tool_name: &str, input: &serde_json::Value) -> String {
    match tool_name {
        "shell" | "powershell" | "powershell_query" => input["command"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect(),
        "file_read" | "file_write" => input["path"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect(),
        "web_search" => input["query"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(80)
            .collect(),
        "browser" => {
            let action = input["action"].as_str().unwrap_or("?");
            let url = input["url"].as_str().unwrap_or("");
            if url.is_empty() {
                action.to_string()
            } else {
                format!("{} {}", action, &url[..url.len().min(60)])
            }
        }
        _ => input.to_string().chars().take(80).collect(),
    }
}

fn read_log_tail(n: usize) -> Vec<String> {
    let log_dir = {
        #[cfg(target_os = "windows")]
        {
            dirs::data_local_dir()
                .map(|d| d.join("pisci").join("logs"))
                .unwrap_or_else(|| std::path::PathBuf::from("logs"))
        }
        #[cfg(not(target_os = "windows"))]
        {
            std::path::PathBuf::from("logs")
        }
    };

    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let log_file = log_dir.join(format!("pisci.log.{}", today));

    if !log_file.exists() {
        return vec![format!("Log file not found: {}", log_file.display())];
    }

    match std::fs::read_to_string(&log_file) {
        Ok(content) => {
            let lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
            let start = lines.len().saturating_sub(n);
            lines[start..].to_vec()
        }
        Err(e) => vec![format!("Failed to read log: {}", e)],
    }
}

/// Result of a UIA drag precision test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiaDragTestResult {
    pub passed: bool,
    pub response_text: String,
    pub error: Option<String>,
    pub duration_ms: u64,
    pub tool_calls: Vec<ToolCallRecord>,
}

/// Run the UIA drag precision test using the agent.
///
/// The agent will:
/// 1. Take a screenshot of the current screen
/// 2. Identify the orange ball and green target zone coordinates from the image
/// 3. Execute a drag operation from ball center to target center
/// 4. Report whether the drag succeeded
///
/// Requires vision_enabled = true in settings.
#[tauri::command]
pub async fn run_uia_drag_test(state: State<'_, AppState>) -> Result<UiaDragTestResult, String> {
    let (
        provider,
        model,
        api_key,
        base_url,
        workspace_root,
        max_tokens,
        policy_mode,
        tool_rate_limit_per_minute,
        tool_settings,
        max_iterations,
        builtin_tool_enabled,
        vision_enabled,
    ) = {
        let settings = state.settings.lock().await;
        (
            settings.provider.clone(),
            settings.model.clone(),
            settings.active_api_key().to_string(),
            settings.custom_base_url.clone(),
            settings.workspace_root.clone(),
            settings.max_tokens,
            settings.policy_mode.clone(),
            settings.tool_rate_limit_per_minute,
            Arc::new(crate::agent::tool::ToolSettings::from_settings(&settings)),
            settings.max_iterations,
            settings.builtin_tool_enabled.clone(),
            settings.vision_enabled,
        )
    };

    if !vision_enabled {
        return Err("vision_not_enabled".into());
    }

    if api_key.is_empty() {
        return Err("api_key_not_configured".into());
    }

    info!("UIA drag test: starting vision-guided drag test");

    let start = std::time::Instant::now();
    let session_id = "debug_uia_drag_test".to_string();

    let client = build_client(
        &provider,
        &api_key,
        if base_url.is_empty() {
            None
        } else {
            Some(&base_url)
        },
    );

    let user_tools_dir = state
        .app_handle
        .path()
        .app_data_dir()
        .map(|d: std::path::PathBuf| d.join("user-tools"))
        .ok();
    let app_data_dir_d2 = state.app_handle.path().app_data_dir().ok();

    let registry = Arc::new(tools::build_registry(
        state.browser.clone(),
        user_tools_dir.as_deref(),
        Some(state.db.clone()),
        Some(&builtin_tool_enabled),
        Some(state.app_handle.clone()),
        Some(state.settings.clone()),
        app_data_dir_d2,
        None, // skill_search not used in debug scenarios
    ));

    let policy = Arc::new(PolicyGate::with_profile(
        &workspace_root,
        &policy_mode,
        tool_rate_limit_per_minute,
    ));

    let effective_workspace = if workspace_root.is_empty() {
        std::env::temp_dir()
    } else {
        std::path::PathBuf::from(&workspace_root)
    };

    let prompt = "任务：将橙色小球拖拽到绿色目标区域。\n\
         \n\
         步骤：\n\
         1. 用 screen_capture 工具（action=list_monitors）查看显示器布局和各显示器上的窗口分布，\n\
            找到 OpenPisci 窗口在哪个显示器（monitor_index）\n\
         2. 用 screen_capture 工具截取该显示器截图（action=capture, monitor_index=N, grid=true）\n\
         3. 仔细观察截图中的坐标网格（每200像素一条线，标签显示绝对物理屏幕坐标）\n\
         4. 找到橙色圆形小球的中心坐标（读取最近的网格线标签，精确估算）\n\
         5. 找到绿色虚线矩形（目标区域）的中心坐标（读取最近的网格线标签，精确估算）\n\
         6. 用 uia 工具的 drag_drop 操作，从小球中心拖拽到目标区域中心\n\
         \n\
         重要提示：\n\
         - 网格标签显示的是物理屏幕绝对坐标，可直接用于 uia drag_drop（无需任何转换）\n\
         - 读取坐标时，先找最近的网格线，再根据元素与网格线的相对位置微调\n\
         - 橙色小球是一个橙色圆形，直径约40像素\n\
         - 目标区域是一个绿色虚线矩形（有发光效果），约120x120像素，位于测试区域右侧\n\
         - 拖拽时 from 是小球中心坐标，to 是目标区域中心坐标\n\
         - 如果 OpenPisci 在主显示器，可直接用 monitor_index=0（默认）"
        .to_string();

    let agent = AgentLoop {
        client,
        registry,
        policy,
        system_prompt: format!(
            "You are Pisci, a Windows AI Agent running a UIA precision drag test.\nToday's date: {}\nWorkspace directory: {}\nUse ONLY these tools: screen_capture, uia. Do not call any other tools.\nExecute the task precisely. Do not ask for confirmation.",
            chrono::Utc::now().format("%Y-%m-%d"),
            effective_workspace.display(),
        ),
        model,
        max_tokens,
        db: Some(state.db.clone()),
        app_handle: None,
        confirmation_responses: None,
        confirm_flags: crate::agent::loop_::ConfirmFlags {
            confirm_shell: false,
            confirm_file_write: false,
        },
        vision_override: Some(vision_enabled),
        notification_rx: None,
    };

    let ctx = ToolContext {
        session_id: session_id.clone(),
        workspace_root: effective_workspace.clone(),
        bypass_permissions: false,
        settings: tool_settings,
        max_iterations: Some(max_iterations.min(12)),
        memory_owner_id: "pisci".to_string(),
        pool_session_id: None,
    };

    let messages = vec![LlmMessage {
        role: "user".into(),
        content: MessageContent::text(&prompt),
    }];

    let cancel = Arc::new(AtomicBool::new(false));
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);

    let tool_records: Arc<tokio::sync::Mutex<Vec<ToolCallRecord>>> =
        Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let tool_records_clone = tool_records.clone();

    let tool_starts: Arc<
        tokio::sync::Mutex<
            std::collections::HashMap<String, (String, std::time::Instant, serde_json::Value)>,
        >,
    > = Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new()));
    let tool_starts_clone = tool_starts.clone();

    let event_collector = tokio::spawn(async move {
        while let Some(event) = event_rx.recv().await {
            match event {
                AgentEvent::ToolStart { id, name, input } => {
                    tool_starts_clone
                        .lock()
                        .await
                        .insert(id, (name, std::time::Instant::now(), input));
                }
                AgentEvent::ToolEnd {
                    id,
                    name,
                    result,
                    is_error,
                } => {
                    let duration_ms = {
                        let mut starts = tool_starts_clone.lock().await;
                        starts
                            .remove(&id)
                            .map(|(_, t, input)| {
                                let dur = t.elapsed().as_millis() as u64;
                                (dur, summarize_input(&name, &input))
                            })
                            .unwrap_or((0, String::new()))
                    };
                    let result_summary: String = result.chars().take(500).collect();
                    tool_records_clone.lock().await.push(ToolCallRecord {
                        tool_name: name,
                        input_summary: duration_ms.1,
                        result_summary,
                        is_error,
                        duration_ms: duration_ms.0,
                    });
                }
                _ => {}
            }
        }
    });

    let run_result = agent.run(messages, event_tx, cancel, ctx).await;
    let _ = event_collector.await;
    let tool_calls = tool_records.lock().await.clone();
    let duration_ms = start.elapsed().as_millis() as u64;

    match run_result {
        Ok((final_messages, _total_in, _total_out)) => {
            let response_text = final_messages
                .iter()
                .rev()
                .find(|m| m.role == "assistant")
                .map(|m| m.content.as_text())
                .unwrap_or_default();

            // Whether the drag actually succeeded is determined by the frontend
            // checking the ball's visual position — not by parsing the agent's text.
            // The agent may hallucinate success even when the ball didn't move.
            // We always return passed=false here; the frontend's checkDrop() will
            // set dragState="success" if the ball is truly inside the target zone.
            info!(
                "UIA drag test completed (frontend will verify): duration={}ms tools={}",
                duration_ms,
                tool_calls.len()
            );

            Ok(UiaDragTestResult {
                passed: false,
                response_text,
                error: None,
                duration_ms,
                tool_calls,
            })
        }
        Err(e) => {
            let err_str = e.to_string();
            tracing::warn!("UIA drag test failed: {}", err_str);
            Ok(UiaDragTestResult {
                passed: false,
                response_text: String::new(),
                error: Some(err_str),
                duration_ms,
                tool_calls,
            })
        }
    }
}

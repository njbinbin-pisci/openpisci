use crate::store::AppState;
use serde::{Deserialize, Serialize};
use tauri::State;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuiltinToolInfo {
    pub name: String,
    pub description: String,
    pub icon: String,
    pub windows_only: bool,
}

/// Returns the list of system built-in tools with metadata.
#[tauri::command]
pub async fn list_builtin_tools(
    _state: State<'_, AppState>,
) -> Result<Vec<BuiltinToolInfo>, String> {
    let tools = vec![
        BuiltinToolInfo {
            name: "file_read".into(),
            description: "读取本地文件内容，支持文本和二进制文件".into(),
            icon: "📄".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "file_write".into(),
            description: "写入或修改本地文件，支持创建新文件和追加内容".into(),
            icon: "✏️".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "shell".into(),
            description: "执行系统 Shell 命令（cmd.exe / bash）".into(),
            icon: "⌨️".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "powershell_query".into(),
            description: "执行 PowerShell 脚本，支持 Windows 系统管理任务".into(),
            icon: "🪟".into(),
            windows_only: true,
        },
        BuiltinToolInfo {
            name: "web_search".into(),
            description: "搜索互联网，获取最新信息".into(),
            icon: "🔍".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "browser".into(),
            description: "控制 Chrome 浏览器，支持网页导航、点击、截图等操作".into(),
            icon: "🌐".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "wmi".into(),
            description: "通过 WMI 查询 Windows 系统信息（硬件、进程、服务等）".into(),
            icon: "💻".into(),
            windows_only: true,
        },
        BuiltinToolInfo {
            name: "office".into(),
            description: "操作 Office 文档（Word、Excel、PowerPoint）".into(),
            icon: "📊".into(),
            windows_only: true,
        },
        BuiltinToolInfo {
            name: "email".into(),
            description: "通过 SMTP/IMAP 发送和读取邮件（需在设置中配置）".into(),
            icon: "📧".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "plan_todo".into(),
            description: "维护当前复杂任务的可视化执行计划与待办状态".into(),
            icon: "📋".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "vision_context".into(),
            description: "管理可复用的视觉工件，并决定下一轮要送入多模态模型的图片".into(),
            icon: "🖼️".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "uia".into(),
            description: "通过 Windows UI Automation 控制桌面应用程序界面元素".into(),
            icon: "🖱️".into(),
            windows_only: true,
        },
        BuiltinToolInfo {
            name: "screen_capture".into(),
            description: "截取屏幕画面，用于视觉感知和 UI 状态分析".into(),
            icon: "📸".into(),
            windows_only: true,
        },
        BuiltinToolInfo {
            name: "com".into(),
            description: "通过 COM/OLE 接口与 Windows 应用程序交互（如 Excel、IE 等）".into(),
            icon: "🔌".into(),
            windows_only: true,
        },
        BuiltinToolInfo {
            name: "call_fish".into(),
            description: "委托子任务给专属 Fish 子代理，让专家处理特定领域任务".into(),
            icon: "🐠".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "call_koi".into(),
            description: "委托任务给持久化 Koi 代理，具备独立记忆与长期职责".into(),
            icon: "🐟".into(),
            windows_only: false,
        },
        BuiltinToolInfo {
            name: "pool_org".into(),
            description: "创建和管理项目池与组织规范，由 Pisci 主动发起协作项目".into(),
            icon: "🏊".into(),
            windows_only: false,
        },
    ];
    Ok(tools)
}

/// Manually trigger a heartbeat agent run.
#[tauri::command]
pub async fn trigger_heartbeat(state: State<'_, AppState>) -> Result<(), String> {
    let (prompt, enabled) = {
        let settings = state.settings.lock().await;
        (
            settings.heartbeat_prompt.clone(),
            settings.heartbeat_enabled,
        )
    };
    if !enabled {
        return Err("Heartbeat is not enabled in settings".into());
    }
    let state_ref = crate::store::AppState {
        db: state.db.clone(),
        settings: state.settings.clone(),
        plan_state: state.plan_state.clone(),
        browser: state.browser.clone(),
        cancel_flags: state.cancel_flags.clone(),
        confirmation_responses: state.confirmation_responses.clone(),
        interactive_responses: state.interactive_responses.clone(),
        app_handle: state.app_handle.clone(),
        scheduler: state.scheduler.clone(),
        gateway: state.gateway.clone(),
        pisci_heartbeat_cursor: state.pisci_heartbeat_cursor.clone(),
    };
    tokio::spawn(async move {
        let _ = crate::pisci::heartbeat::dispatch_heartbeat(&state_ref, &prompt, "internal").await;
    });
    Ok(())
}

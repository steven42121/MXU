//! 辅助函数
//!
//! 提供路径处理和其他通用工具函数

use super::types::MaaCallbackEvent;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter};

// ==================== 控制台输出 ====================

static CONSOLE_ENABLED: AtomicBool = AtomicBool::new(false);

#[cfg(windows)]
static CONSOLE_FILE: OnceLock<std::sync::Mutex<std::fs::File>> = OnceLock::new();

/// 初始化控制台输出（在 main 中调用）
/// 仅当命令行传入 `--console` 参数时启用
pub fn init_console_output() {
    if !std::env::args().any(|a| a == "--console") {
        return;
    }

    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Console::{
            AttachConsole, SetConsoleOutputCP, SetStdHandle, ATTACH_PARENT_PROCESS,
            STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        };

        // 附着到父进程终端（从 cmd/powershell 启动时生效）
        if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok() {
            // 设置控制台输出为 UTF-8
            unsafe {
                let _ = SetConsoleOutputCP(65001);
            }

            if let Ok(f) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
                let _ = CONSOLE_FILE.set(std::sync::Mutex::new(f));
                CONSOLE_ENABLED.store(true, Ordering::Relaxed);

                // 将进程的 stdout/stderr 重定向到 NUL
                // 防止 MaaFramework C++ 库的内部日志直接输出到终端
                if let Ok(nul) = std::fs::OpenOptions::new().write(true).open("NUL") {
                    let nul_handle =
                        HANDLE(nul.as_raw_handle() as *mut std::ffi::c_void);
                    unsafe {
                        let _ = SetStdHandle(STD_OUTPUT_HANDLE, nul_handle);
                        let _ = SetStdHandle(STD_ERROR_HANDLE, nul_handle);
                    }
                    std::mem::forget(nul); // 保持 NUL 句柄存活
                }
            }
        }
    }

    #[cfg(not(windows))]
    {
        CONSOLE_ENABLED.store(true, Ordering::Relaxed);
    }
}

/// 向控制台输出一行日志（仅在命令行启动时有效）
pub fn console_println(args: std::fmt::Arguments<'_>) {
    if !CONSOLE_ENABLED.load(Ordering::Relaxed) {
        return;
    }

    #[cfg(windows)]
    {
        use std::io::Write;
        if let Some(file) = CONSOLE_FILE.get() {
            if let Ok(mut f) = file.lock() {
                let _ = writeln!(f, "{}", args);
            }
        }
    }

    #[cfg(not(windows))]
    {
        println!("{}", args);
    }
}

/// 便捷宏：向控制台输出日志
#[macro_export]
macro_rules! cprintln {
    ($($arg:tt)*) => {
        $crate::commands::utils::console_println(format_args!($($arg)*))
    };
}

/// 返回控制台输出是否已启用
pub fn is_console_enabled() -> bool {
    CONSOLE_ENABLED.load(Ordering::Relaxed)
}

// ==================== 回调事件 ====================

/// 发送回调事件到前端
pub fn emit_callback_event<S: Into<String>>(app: &AppHandle, message: S, details: S) {
    let event = MaaCallbackEvent {
        message: message.into(),
        details: details.into(),
    };
    if let Err(e) = app.emit("maa-callback", event) {
        log::error!("Failed to emit maa-callback: {}", e);
    }
}

/// 获取应用数据目录
/// - macOS: ~/Library/Application Support/MXU/
/// - Windows/Linux: exe 所在目录（保持便携式部署）
pub fn get_app_data_dir() -> Result<PathBuf, String> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").map_err(|_| "无法获取 HOME 环境变量".to_string())?;
        let path = PathBuf::from(home)
            .join("Library")
            .join("Application Support")
            .join("MXU");
        Ok(path)
    }

    #[cfg(not(target_os = "macos"))]
    {
        // Windows/Linux 保持便携式，使用 exe 所在目录
        get_exe_directory()
    }
}

/// 规范化路径：移除冗余的 `.`、处理 `..`、统一分隔符
/// 使用 Path::components() 解析，不需要路径实际存在
pub fn normalize_path(path: &str) -> PathBuf {
    use std::path::{Component, Path};

    let path = Path::new(path);
    let mut components = Vec::new();

    for component in path.components() {
        match component {
            // 跳过当前目录标记 "."
            Component::CurDir => {}
            // 处理父目录 ".."：如果栈顶是普通目录则弹出，否则保留
            Component::ParentDir => {
                if matches!(components.last(), Some(Component::Normal(_))) {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            // 保留其他组件（Prefix、RootDir、Normal）
            _ => components.push(component),
        }
    }

    // 重建路径
    components.into_iter().collect()
}

/// 获取日志目录（应用数据目录下的 debug 子目录）
pub fn get_logs_dir() -> PathBuf {
    get_app_data_dir()
        .unwrap_or_else(|_| {
            // 回退到 exe 目录
            let exe_path = std::env::current_exe().unwrap_or_default();
            exe_path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .to_path_buf()
        })
        .join("debug")
}

/// 获取 exe 所在目录路径（内部使用）
pub fn get_exe_directory() -> Result<PathBuf, String> {
    let exe_path = std::env::current_exe().map_err(|e| format!("获取 exe 路径失败: {}", e))?;
    exe_path
        .parent()
        .map(|p| p.to_path_buf())
        .ok_or_else(|| "无法获取 exe 所在目录".to_string())
}

/// 获取可执行文件所在目录下的 maafw 子目录
pub fn get_maafw_dir() -> Result<PathBuf, String> {
    Ok(get_exe_directory()?.join("maafw"))
}

/// 构建 User-Agent 字符串
pub fn build_user_agent() -> String {
    let version = env!("CARGO_PKG_VERSION");
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let tauri_version = tauri::VERSION;
    format!("MXU/{} ({}; {}) Tauri/{}", version, os, arch, tauri_version)
}

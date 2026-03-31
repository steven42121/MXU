//! 辅助函数
//!
//! 提供路径处理和其他通用工具函数

use super::types::MaaCallbackEvent;
use std::path::PathBuf;
use std::sync::OnceLock;
use tauri::{AppHandle, Emitter};

// ==================== 控制台输出 ====================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogPrintMode {
    None,
    Raw,
    Ui,
    Verbose,
}

static LOG_PRINT_MODE: OnceLock<LogPrintMode> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConsoleMode {
    Ui,
    Verbose,
}

static CONSOLE_MODE: OnceLock<ConsoleMode> = OnceLock::new();

fn default_log_print_mode() -> LogPrintMode {
    #[cfg(debug_assertions)]
    {
        LogPrintMode::Raw
    }

    #[cfg(not(debug_assertions))]
    {
        LogPrintMode::None
    }
}

fn parse_log_print_mode(args: &[String]) -> LogPrintMode {
    if let Some(mode) = args.iter().find_map(|a| a.strip_prefix("--log-mode=")) {
        return match mode.to_ascii_lowercase().as_str() {
            "none" | "off" | "silent" => LogPrintMode::None,
            "raw" => LogPrintMode::Raw,
            "ui" => LogPrintMode::Ui,
            "verbose" => LogPrintMode::Verbose,
            _ => default_log_print_mode(),
        };
    }

    default_log_print_mode()
}

/// 初始化控制台输出（在 main 中调用）
/// 支持 `--log-mode=<none|raw|ui|verbose>`
/// - none: 不输出日志到控制台
/// - raw: 保留标准流原始日志输出（tauri_plugin_log Stdout target）
/// - ui: 附着父终端，CRT fd → NUL 丢弃 C++ 噪音，println! 输出格式化日志
/// - verbose: 附着父终端，C++ 原始日志保留，println! 输出格式化日志
pub fn init_console_output() {
    let args: Vec<String> = std::env::args().collect();
    let log_mode = parse_log_print_mode(&args);
    let _ = LOG_PRINT_MODE.set(log_mode);

    match log_mode {
        LogPrintMode::Ui => {
            let _ = CONSOLE_MODE.set(ConsoleMode::Ui);
        }
        LogPrintMode::Verbose => {
            let _ = CONSOLE_MODE.set(ConsoleMode::Verbose);
        }
        LogPrintMode::None | LogPrintMode::Raw => {
            return;
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::Console::{
            AttachConsole, GetStdHandle, SetConsoleOutputCP, SetStdHandle,
            ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE,
        };

        // 1. 附着父终端（从 cmd/powershell 启动时生效）
        if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_err() {
            log::warn!("AttachConsole failed; --log-mode=ui/verbose 需要从终端启动");
            return;
        }

        // 设置控制台输出代码页为 UTF-8
        unsafe { let _ = SetConsoleOutputCP(65001); }

        // 2. 打开 CONOUT$ 设置 Win32 stdout/stderr 句柄
        //    windows_subsystem="windows" 的 GUI 程序启动时 GetStdHandle 返回 NULL，
        //    AttachConsole 不会自动设置，需要手动指向 CONOUT$
        if let Ok(conout) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
            let conout_handle = HANDLE(conout.as_raw_handle() as *mut std::ffi::c_void);
            unsafe {
                let _ = SetStdHandle(STD_OUTPUT_HANDLE, conout_handle);
                let _ = SetStdHandle(STD_ERROR_HANDLE, conout_handle);
            }
            std::mem::forget(conout);
        }

        // 3. ui 模式：CRT fd 1/2 → NUL（丢弃 C++ 库噪音）
        //    _dup2 会自动调用 SetStdHandle 覆盖 Win32 句柄，
        //    所以先保存 → _dup2 → 恢复，确保 println! 仍输出到终端
        if log_mode == LogPrintMode::Ui {
            // 保存当前 Win32 stdout/stderr 句柄
            let saved_out = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) }.unwrap_or(HANDLE::default());
            let saved_err = unsafe { GetStdHandle(STD_ERROR_HANDLE) }.unwrap_or(HANDLE::default());

            if let Ok(nul) = std::fs::OpenOptions::new().write(true).open("NUL") {
                unsafe {
                    extern "C" {
                        fn _open_osfhandle(osfhandle: isize, flags: i32) -> i32;
                        fn _dup2(fd1: i32, fd2: i32) -> i32;
                    }
                    let nul_fd = _open_osfhandle(nul.as_raw_handle() as isize, 0);
                    if nul_fd >= 0 {
                        let _ = _dup2(nul_fd, 1); // CRT stdout → NUL（同时覆盖 Win32 句柄）
                        let _ = _dup2(nul_fd, 2); // CRT stderr → NUL（同时覆盖 Win32 句柄）
                    }

                    // 恢复 Win32 句柄，让 println! 继续输出到终端/管道
                    let _ = SetStdHandle(STD_OUTPUT_HANDLE, saved_out);
                    let _ = SetStdHandle(STD_ERROR_HANDLE, saved_err);
                }
                std::mem::forget(nul);
            }
        }
    }

    // macOS/Linux: ui 模式重定向 fd 1/2 到 /dev/null
    #[cfg(not(windows))]
    {
        if log_mode == LogPrintMode::Ui {
            if let Ok(nul) = std::fs::OpenOptions::new().write(true).open("/dev/null") {
                use std::os::unix::io::AsRawFd;
                let nul_fd = nul.as_raw_fd();
                unsafe {
                    let _ = libc::dup2(nul_fd, 1); // stdout → /dev/null
                    let _ = libc::dup2(nul_fd, 2); // stderr → /dev/null
                }
                std::mem::forget(nul);
            }
        }
    }
}

/// 向终端输出一行日志（println! 走 Win32 GetStdHandle，可被管道捕获）
pub fn console_println(args: std::fmt::Arguments<'_>) {
    if !is_console_enabled() {
        return;
    }
    println!("{}", args);
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
    matches!(
        get_log_print_mode(),
        LogPrintMode::Ui | LogPrintMode::Verbose
    )
}

/// 返回控制台输出模式
pub fn get_console_mode() -> ConsoleMode {
    *CONSOLE_MODE.get().unwrap_or(&ConsoleMode::Ui)
}

/// 返回日志打印模式
pub fn get_log_print_mode() -> LogPrintMode {
    *LOG_PRINT_MODE.get_or_init(default_log_print_mode)
}

/// 是否启用标准输出日志（用于 tauri_plugin_log 的 Stdout target）
pub fn should_log_to_stdout() -> bool {
    matches!(get_log_print_mode(), LogPrintMode::Raw)
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

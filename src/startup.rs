#[cfg(windows)]
use std::path::Path;
#[cfg(windows)]
use std::process::Command;

#[cfg(windows)]
const TASK_NAME: &str = "AIMToolkit RamDisk Manager";

#[cfg(windows)]
pub fn login_startup_enabled() -> Result<bool, String> {
    let output = run_schtasks(&["/Query", "/TN", TASK_NAME])?;
    if output.status.success() {
        Ok(true)
    } else {
        // schtasks reports a missing task with localized text. If it does not,
        // enumerate CSV task names so the missing-task check is independent of
        // the Windows display language and console encoding.
        let inventory = run_schtasks(&["/Query", "/FO", "CSV", "/NH"])?;
        match task_exists_in_csv(&inventory) {
            Some(false) => Ok(false),
            Some(true) => Err(format_schtasks_error(localized_text!("msg_0198"), &output)),
            None if task_is_missing(&output) => Ok(false),
            None => Err(format_schtasks_error(localized_text!("msg_0198"), &output)),
        }
    }
}

#[cfg(not(windows))]
pub fn login_startup_enabled() -> Result<bool, String> {
    Err(localized_text!("msg_0081").to_owned())
}

#[cfg(windows)]
pub fn set_login_startup_enabled(enabled: bool) -> Result<(), String> {
    if enabled {
        let executable = std::env::current_exe()
            .map_err(|error| localized_format!("msg_0193", error = error))?;
        let action = task_action(&executable);
        let output = run_schtasks(&[
            "/Create", "/F", "/TN", TASK_NAME, "/SC", "ONLOGON", "/TR", &action, "/RL", "HIGHEST",
            "/IT",
        ])?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format_schtasks_error(localized_text!("msg_0163"), &output))
        }
    } else {
        let output = run_schtasks(&["/Delete", "/F", "/TN", TASK_NAME])?;
        if output.status.success() {
            return Ok(());
        }
        // Deleting an already removed task is an idempotent request, but only
        // a verified not-found result counts as success.
        match login_startup_enabled() {
            Ok(false) => Ok(()),
            Ok(true) => Err(format_schtasks_error(localized_text!("msg_0399"), &output)),
            Err(verify_error) => Err(localized_format!(
                "msg_0204",
                format_schtasks_error(localized_text!("msg_0399"), &output),
                verify_error = verify_error
            )),
        }
    }
}

#[cfg(not(windows))]
pub fn set_login_startup_enabled(_enabled: bool) -> Result<(), String> {
    Err(localized_text!("msg_0081").to_owned())
}

#[cfg(windows)]
fn task_action(executable: &Path) -> String {
    // Windows paths cannot contain a quote; quoting protects spaces in the path.
    format!("\"{}\" --startup", executable.display())
}

#[cfg(windows)]
fn run_schtasks(arguments: &[&str]) -> Result<std::process::Output, String> {
    use std::os::windows::process::CommandExt;

    Command::new("schtasks.exe")
        .args(arguments)
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|error| localized_format!("msg_0239", error = error))
}

#[cfg(windows)]
fn task_is_missing(output: &std::process::Output) -> bool {
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_lowercase();
    [
        "cannot find the file specified",
        "cannot find the path specified",
        "task name cannot be found",
        "task name does not exist",
        "no scheduled task",
        "task does not exist",
        "specified task name does not exist",
        "系统找不到指定的文件",
        "系统找不到指定的路径",
        "找不到指定的文件",
        "找不到指定的路径",
        "找不到指定的任务",
        "任务名称无法找到",
        "任务不存在",
        "系統找不到指定的檔案",
        "系統找不到指定的路徑",
    ]
    .iter()
    .any(|message| text.contains(message))
}

#[cfg(windows)]
fn task_exists_in_csv(output: &std::process::Output) -> Option<bool> {
    if !output.status.success() {
        return None;
    }
    let task_path = format!("\\{TASK_NAME}");
    if output.stdout.contains(&0) {
        for candidate in [&task_path, TASK_NAME] {
            let utf16_bytes = candidate
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>();
            if output
                .stdout
                .windows(utf16_bytes.len())
                .any(|window| window == utf16_bytes.as_slice())
            {
                return Some(true);
            }
        }
        return None;
    }
    for line in output.stdout.split(|byte| *byte == b'\n') {
        let Some(field) = line.split(|byte| *byte == b',').next() else {
            continue;
        };
        let field = field.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(field);
        let field = field.strip_prefix(b"\"").unwrap_or(field);
        let field = field.strip_suffix(b"\"").unwrap_or(field);
        if field.eq_ignore_ascii_case(task_path.as_bytes())
            || field.eq_ignore_ascii_case(TASK_NAME.as_bytes())
        {
            return Some(true);
        }
    }
    Some(false)
}

#[cfg(windows)]
fn format_schtasks_error(operation: &str, output: &std::process::Output) -> String {
    let details = if output.stderr.is_empty() {
        String::from_utf8_lossy(&output.stdout)
    } else {
        String::from_utf8_lossy(&output.stderr)
    };
    let details = details.trim();
    if details.is_empty() {
        localized_format!("msg_0551", output.status, operation = operation)
    } else {
        localized_format!(
            "msg_0147",
            output.status,
            operation = operation,
            details = details
        )
    }
}

//! Elevation utilities: check admin status and restart with elevated privileges.

use windows::Win32::Security::{
    GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HWND};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOW;
use windows::core::PCWSTR;

/// Checks whether the current process is running with Administrator privileges.
/// Uses the TokenElevation approach.
pub fn check_elevated() -> bool {
    check_token_elevation().unwrap_or(false)
}

fn check_token_elevation() -> Result<bool, windows::core::Error> {
    unsafe {
        let mut token: HANDLE = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token)?;

        let mut elevation = TOKEN_ELEVATION::default();
        let mut return_length = 0u32;

        GetTokenInformation(
            token,
            TokenElevation,
            Some(&mut elevation as *mut _ as *mut _),
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut return_length,
        )?;

        let _ = CloseHandle(token);
        Ok(elevation.TokenIsElevated != 0)
    }
}

/// Parameters carried across an elevation restart so the UI can restore the
/// user's pending monitoring configuration.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RestartParams {
    pub executable: String,
    pub interval_ms: u64,
    pub enable_network: bool,
    pub retention_secs: u64,
}

/// Parses elevation-restart parameters from the process command line, if any.
pub fn parse_restart_params() -> Option<RestartParams> {
    let args: Vec<String> = std::env::args().collect();
    let mut executable: Option<String> = None;
    let mut interval_ms: Option<u64> = None;
    let mut retention_secs: Option<u64> = None;
    let mut enable_network = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--pending-exec" if i + 1 < args.len() => {
                executable = Some(args[i + 1].clone());
                i += 2;
            }
            "--pending-interval" if i + 1 < args.len() => {
                interval_ms = args[i + 1].parse().ok();
                i += 2;
            }
            "--pending-retention" if i + 1 < args.len() => {
                retention_secs = args[i + 1].parse().ok();
                i += 2;
            }
            "--pending-network" => {
                enable_network = true;
                i += 1;
            }
            _ => i += 1,
        }
    }

    executable.map(|e| RestartParams {
        executable: e,
        interval_ms: interval_ms.unwrap_or(1000),
        enable_network,
        retention_secs: retention_secs.unwrap_or(300),
    })
}

/// Restarts the current application with Administrator privileges.
/// Uses ShellExecuteW with the "runas" verb to trigger a UAC prompt.
/// If successful, the current process exits.
pub fn restart_as_admin(
    executable: &str,
    interval_ms: u64,
    enable_network: bool,
    retention_secs: u64,
) -> bool {
    let exe_path = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return false,
    };

    let exe_str = match exe_path.to_str() {
        Some(s) => s.to_string(),
        None => return false,
    };

    let safe_exec = executable.replace('"', "");
    let params = format!(
        "--pending-exec \"{}\" --pending-interval {} --pending-retention {} {}",
        safe_exec,
        interval_ms,
        retention_secs,
        if enable_network { "--pending-network" } else { "" }
    );

    // Encode as wide strings for ShellExecuteW
    let wide_exe: Vec<u16> = exe_str.encode_utf16().chain(std::iter::once(0)).collect();
    let wide_verb: Vec<u16> = "runas\0".encode_utf16().collect();
    let wide_params: Vec<u16> = params.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        let result = ShellExecuteW(
            Some(HWND::default()),
            PCWSTR::from_raw(wide_verb.as_ptr()),
            PCWSTR::from_raw(wide_exe.as_ptr()),
            PCWSTR::from_raw(wide_params.as_ptr()),
            PCWSTR::null(),
            SW_SHOW,
        );
        // ShellExecuteW returns an HINSTANCE; > 32 means success
        let code = result.0 as isize;
        if code > 32 {
            // Give the new process a moment to start, then exit
            std::process::exit(0);
        }
        code > 32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_check_elevated_does_not_crash() {
        let _ = check_elevated();
    }
}

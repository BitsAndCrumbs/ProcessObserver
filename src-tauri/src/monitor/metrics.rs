//! Process metric collection using Win32 APIs.
//!
//! Collects: CPU %, RAM (working set), I/O bytes/sec.
//! Uses GetProcessTimes for CPU, GetProcessMemoryInfo for RAM,
//! and GetProcessIoCounters for I/O.

use std::collections::HashMap;
use std::time::Instant;
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::ProcessStatus::{
    GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
};
use windows::Win32::System::Threading::{
    GetProcessTimes, OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
};

// ─── FFI for I/O counters (not yet exposed in windows-rs 0.62) ─────

#[repr(C)]
#[allow(non_snake_case)]
struct IO_COUNTERS {
    ReadOperationCount: u64,
    WriteOperationCount: u64,
    OtherOperationCount: u64,
    ReadTransferCount: u64,
    WriteTransferCount: u64,
    OtherTransferCount: u64,
}

extern "system" {
    fn GetProcessIoCounters(
        hProcess: HANDLE,
        lpIoCounters: *mut IO_COUNTERS,
    ) -> i32;
}

/// Cached state needed for CPU % calculation across intervals.
/// Uses a Mutex for thread-safe access across concurrent monitoring sessions.
use std::sync::Mutex;

static CPU_CACHE: Mutex<Option<CpuCache>> = Mutex::new(None);

struct CpuCache {
    /// Maps PID → (last_kernel_time, last_user_time, last_sample_instant)
    entries: HashMap<u32, (u64, u64, Instant)>,
}

impl CpuCache {
    fn with<R>(f: impl FnOnce(&mut Self) -> R) -> R {
        let mut guard = CPU_CACHE.lock().expect("CPU_CACHE lock poisoned");
        let cache = guard.get_or_insert_with(|| CpuCache {
            entries: HashMap::new(),
        });
        f(cache)
    }
}

/// Aggregated process metrics.
#[derive(Debug, Clone)]
pub struct ProcessMetrics {
    pub cpu_percent: f64,
    pub working_set_bytes: u64,
    pub io_read_bytes: u64,
    pub io_write_bytes: u64,
}

impl ProcessMetrics {
    /// Collect all metrics for a given PID.
    pub fn collect(pid: u32) -> Result<Self, String> {
        let handle = open_process_handle(pid)?;

        let cpu = Self::compute_cpu_percent(pid, &handle)?;
        let ram = Self::get_working_set(&handle)?;
        let (io_read, io_write) = Self::get_io_counters(&handle)?;

        unsafe { let _ = CloseHandle(handle); }

        Ok(ProcessMetrics {
            cpu_percent: cpu,
            working_set_bytes: ram,
            io_read_bytes: io_read,
            io_write_bytes: io_write,
        })
    }

    fn compute_cpu_percent(pid: u32, handle: &HANDLE) -> Result<f64, String> {
        unsafe {
            let mut creation = FILETIME::default();
            let mut exit = FILETIME::default();
            let mut kernel = FILETIME::default();
            let mut user = FILETIME::default();

            GetProcessTimes(*handle, &mut creation, &mut exit, &mut kernel, &mut user)
                .map_err(|e| format!("GetProcessTimes failed for PID {}: {:?}", pid, e))?;

            let kernel_u = filetime_to_u64(&kernel);
            let user_u = filetime_to_u64(&user);
            let now = Instant::now();

            let percent = CpuCache::with(|cache| {
                let pct = if let Some(&(prev_kernel, prev_user, prev_instant)) = cache.entries.get(&pid) {
                    let elapsed = now.duration_since(prev_instant);
                    let elapsed_100ns = elapsed.as_nanos() as u64 / 100;

                    if elapsed_100ns > 0 {
                        let delta_kernel = kernel_u.saturating_sub(prev_kernel);
                        let delta_user = user_u.saturating_sub(prev_user);
                        let total_delta = delta_kernel + delta_user;
                        (total_delta as f64 / elapsed_100ns as f64) * 100.0
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };
                cache.entries.insert(pid, (kernel_u, user_u, now));
                pct.clamp(0.0, 100.0)
            });

            Ok(percent)
        }
    }

    fn get_working_set(handle: &HANDLE) -> Result<u64, String> {
        unsafe {
            let mut pmc = PROCESS_MEMORY_COUNTERS::default();
            GetProcessMemoryInfo(
                *handle,
                &mut pmc,
                std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
            )
            .map_err(|e| format!("GetProcessMemoryInfo failed: {:?}", e))?;
            Ok(pmc.WorkingSetSize as u64)
        }
    }

    fn get_io_counters(handle: &HANDLE) -> Result<(u64, u64), String> {
        unsafe {
            let mut io: IO_COUNTERS = std::mem::zeroed();
            let ret = GetProcessIoCounters(*handle, &mut io);
            if ret == 0 {
                // Process may not be accessible; return zeros
                return Ok((0, 0));
            }
            Ok((io.ReadTransferCount, io.WriteTransferCount))
        }
    }
}

/// Enumerate all running process names for autocomplete suggestions.
pub fn enumerate_process_names() -> Result<Vec<String>, String> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .map_err(|e| format!("CreateToolhelp32Snapshot failed: {:?}", e))?;

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        let mut names: Vec<String> = Vec::new();

        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let name = String::from_utf16_lossy(&entry.szExeFile);
                let name = name.trim_end_matches('\0').to_lowercase();
                if !name.is_empty() {
                    names.push(name);
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }

        let _ = CloseHandle(snapshot);
        names.sort();
        names.dedup();
        Ok(names)
    }
}

/// Find all PIDs matching a given executable name (case-insensitive).
pub fn find_pids_by_name(name: &str) -> Result<Vec<u32>, String> {
    let lower = name.to_lowercase();
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)
            .map_err(|e| format!("CreateToolhelp32Snapshot failed: {:?}", e))?;

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        let mut pids: Vec<u32> = Vec::new();

        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let proc_name = String::from_utf16_lossy(&entry.szExeFile);
                let proc_name = proc_name.trim_end_matches('\0');
                if proc_name.to_lowercase() == lower {
                    pids.push(entry.th32ProcessID);
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }

        let _ = CloseHandle(snapshot);
        Ok(pids)
    }
}

// ─── Helpers ──────────────────────────────────────────────

fn open_process_handle(pid: u32) -> Result<HANDLE, String> {
    unsafe {
        let handle = OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ,
            false,
            pid,
        )
        .map_err(|e| format!("OpenProcess failed for PID {}: {:?}", pid, e))?;
        Ok(handle)
    }
}

fn filetime_to_u64(ft: &FILETIME) -> u64 {
    ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64)
}

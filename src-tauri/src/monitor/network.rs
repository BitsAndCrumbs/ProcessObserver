//! Network statistics collection for ProcessObserver.
//!
//! **Elevated mode (WFP/ESTATS):** Uses `GetExtendedTcpTable` + raw-FFI
//! `GetPerTcpConnectionEStats` to retrieve true per-connection byte counters
//! (DataBytesIn / DataBytesOut) from the TCP Extended Statistics subsystem.
//! Requires Administrator privileges.
//!
//! **Fallback mode:** When not elevated, counts active TCP connections per PID
//! as an approximate proxy metric. No byte-level accuracy — a banner in the UI
//! indicates degraded network data.

use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID,
    TCP_TABLE_OWNER_PID_ALL,
};
use windows::Win32::Networking::WinSock::AF_INET;

// ─── Raw FFI for ESTATS ────────────────────────────────────
// windows-rs 0.62 wraps these functions with `&[u8]` slice parameters, which
// is cumbersome to use with typed structs.  We bind them directly so we can
// pass typed structs by pointer.

/// Mirrors `MIB_TCPROW` (tcpmib.h): the five DWORDs that identify an IPv4 TCP
/// connection.  `MIB_TCPROW_OWNER_PID` starts with these same five fields.
#[repr(C)]
#[allow(non_snake_case)]
struct MIB_TCPROW {
    dwState: u32,
    dwLocalAddr: u32,
    dwLocalPort: u32,
    dwRemoteAddr: u32,
    dwRemotePort: u32,
}

/// Read-only dynamic information for `TcpConnectionEstatsData` (version 0).
/// Field order and types MUST match `TCP_ESTATS_DATA_ROD_v0` from tcpestats.h.
#[repr(C)]
#[allow(non_snake_case)]
struct TCP_ESTATS_DATA_ROD_v0 {
    DataBytesOut: u64,
    DataSegsOut: u64,
    DataBytesIn: u64,
    DataSegsIn: u64,
    SegsOut: u64,
    SegsIn: u64,
    SoftErrors: u32,
    SoftErrorReason: u32,
    SndUna: u32,
    SndNxt: u32,
    SndMax: u32,
    ThruBytesAcked: u64,
    RcvNxt: u32,
    ThruBytesReceived: u64,
}

/// Read/write information for `TcpConnectionEstatsData` (version 0).
/// Layout MUST match `TCP_ESTATS_DATA_RW_v0` from tcpestats.h (a single BOOLEAN).
#[repr(C)]
#[allow(non_snake_case)]
struct TCP_ESTATS_DATA_RW_v0 {
    EnableCollection: u8,
}

/// `TcpConnectionEstatsData` from the `TCP_ESTATS_TYPE` enumeration.
/// NOTE: value 0 is `TcpConnectionEstatsSynOpts`; data is 1.
const TCP_ESTATS_TYPE_DATA: u32 = 1;

#[link(name = "iphlpapi")]
extern "system" {
    /// Direct FFI binding – the real function has 11 parameters:
    /// Row, EstatsType, Rw, RwVersion, RwSize, Ros, RosVersion, RosSize,
    /// Rod, RodVersion, RodSize.
    fn GetPerTcpConnectionEStats(
        row: *const MIB_TCPROW,
        estatstype: u32,
        rw: *mut u8,
        rwversion: u32,
        rwsize: u32,
        ros: *mut u8,
        rosversion: u32,
        rossize: u32,
        rod: *mut u8,
        rodversion: u32,
        rodsize: u32,
    ) -> u32;

    /// Enables/disables collection of a specific ESTATS type for a connection.
    fn SetPerTcpConnectionEStats(
        row: *const MIB_TCPROW,
        estatstype: u32,
        rw: *const u8,
        rwversion: u32,
        rwsize: u32,
        offset: u32,
    ) -> u32;
}

// ─── Public API ────────────────────────────────────────────

/// Network statistics for a specific process.
#[derive(Debug, Clone, Default)]
pub struct NetworkStats {
    /// Number of active TCP connections owned by this PID.
    #[allow(dead_code)]
    pub connection_count: u32,
    /// Total bytes received (WFP ESTATS) or estimated (fallback).
    pub bytes_received: u64,
    /// Total bytes sent (WFP ESTATS) or estimated (fallback).
    pub bytes_sent: u64,
    /// Whether true byte counters were used (true = elevated/WFP, false = fallback).
    #[allow(dead_code)]
    pub using_wfp: bool,
}

impl NetworkStats {
    /// Collect per-process network statistics.
    ///
    /// When `use_wfp` is `true` **and** the process is elevated, this uses
    /// `GetPerTcpConnectionEStats` to sum real TCP byte counters across all
    /// connections owned by `pid`.  Otherwise it falls back to connection
    /// counting.
    pub fn collect(pid: u32, use_wfp: bool) -> Result<Self, String> {
        let table = query_tcp_table()?;

        if use_wfp {
            // ── Elevated: real byte counters via ESTATS ──────────────
            let mut recv: u64 = 0;
            let mut sent: u64 = 0;
            let mut count: u32 = 0;

            for row in &table {
                if row.dwOwningPid == pid {
                    count += 1;
                    if let Ok((r, s)) = get_connection_estats_ffi(row) {
                        recv += r;
                        sent += s;
                    }
                }
            }

            Ok(NetworkStats {
                connection_count: count,
                bytes_received: recv,
                bytes_sent: sent,
                using_wfp: true,
            })
        } else {
            // ── Fallback: connection-count proxy ────────────────────
            let count = table.iter().filter(|r| r.dwOwningPid == pid).count() as u32;

            Ok(NetworkStats {
                connection_count: count,
                bytes_received: count as u64 * 1024,
                bytes_sent: count as u64 * 512,
                using_wfp: false,
            })
        }
    }
}

// ─── Internal helpers ──────────────────────────────────────

/// Query the MIB_TCPTABLE_OWNER_PID for all IPv4 TCP connections.
fn query_tcp_table() -> Result<Vec<MIB_TCPROW_OWNER_PID>, String> {
    unsafe {
        let mut size: u32 = 0;
        let _ret = GetExtendedTcpTable(
            None,
            &mut size,
            false,
            AF_INET.0 as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        );

        let mut buffer: Vec<u8> = vec![0u8; size as usize];
        let ret = GetExtendedTcpTable(
            Some(buffer.as_mut_ptr() as *mut _),
            &mut size,
            false,
            AF_INET.0 as u32,
            TCP_TABLE_OWNER_PID_ALL,
            0,
        );

        if ret != 0 {
            return Err(format!("GetExtendedTcpTable failed: 0x{:X}", ret));
        }

        let table_ptr = buffer.as_ptr() as *const MIB_TCPTABLE_OWNER_PID;
        let table = &*table_ptr;
        let count = table.dwNumEntries as usize;

        let rows: Vec<MIB_TCPROW_OWNER_PID> = std::slice::from_raw_parts(
            &table.table[0] as *const _ as *const MIB_TCPROW_OWNER_PID,
            count,
        )
        .to_vec();

        Ok(rows)
    }
}

/// Retrieve (bytes_in, bytes_out) for a single TCP connection via raw FFI.
///
/// `TcpConnectionEstatsData` collection is disabled by default for a
/// connection, so this first enables it with `SetPerTcpConnectionEStats` and
/// then reads the read-only dynamic (`Rod`) buffer with
/// `GetPerTcpConnectionEStats`.
fn get_connection_estats_ffi(row: &MIB_TCPROW_OWNER_PID) -> Result<(u64, u64), String> {
    unsafe {
        // Cast MIB_TCPROW_OWNER_PID → MIB_TCPROW (the first 5 fields match).
        let tcp_row = MIB_TCPROW {
            dwState: row.dwState,
            dwLocalAddr: row.dwLocalAddr,
            dwLocalPort: row.dwLocalPort,
            dwRemoteAddr: row.dwRemoteAddr,
            dwRemotePort: row.dwRemotePort,
        };

        // Enable data collection (idempotent) so the byte counters accumulate.
        let mut rw = TCP_ESTATS_DATA_RW_v0 { EnableCollection: 1 };
        let rw_size = std::mem::size_of::<TCP_ESTATS_DATA_RW_v0>() as u32;
        let set_ret = SetPerTcpConnectionEStats(
            &tcp_row,
            TCP_ESTATS_TYPE_DATA,
            &mut rw as *mut TCP_ESTATS_DATA_RW_v0 as *const u8,
            0,       // rwversion
            rw_size, // rwsize
            0,       // offset (EnableCollection is the first member)
        );
        if set_ret != 0 {
            return Err(format!("SetPerTcpConnectionEStats failed: 0x{:X}", set_ret));
        }

        let mut data: TCP_ESTATS_DATA_ROD_v0 = std::mem::zeroed();
        let rod_size = std::mem::size_of::<TCP_ESTATS_DATA_ROD_v0>() as u32;

        let ret = GetPerTcpConnectionEStats(
            &tcp_row,
            TCP_ESTATS_TYPE_DATA,
            std::ptr::null_mut(), // rw  – no read/write buffer
            0,                    // rwversion
            0,                    // rwsize
            std::ptr::null_mut(), // ros – no read-only static buffer
            0,                    // rosversion
            0,                    // rossize
            &mut data as *mut TCP_ESTATS_DATA_ROD_v0 as *mut u8, // rod – read-only dynamic
            0,                    // rodversion
            rod_size,             // rodsize
        );

        if ret != 0 {
            return Err(format!("GetPerTcpConnectionEStats failed: 0x{:X}", ret));
        }

        Ok((data.DataBytesIn, data.DataBytesOut))
    }
}

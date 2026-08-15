//! Session data model, storage, and export functionality.

use chrono::{DateTime, Local, Utc};
use serde::{Deserialize, Serialize};

// ─── Data Structures ──────────────────────────────────────

/// A single timestamped measurement of all monitored metrics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataPoint {
    pub timestamp: DateTime<Utc>,
    pub cpu_percent: f64,
    pub ram_mb: f64,
    /// Current read rate in bytes per second (computed over the sample interval).
    pub io_read_bytes_per_sec: u64,
    /// Current write rate in bytes per second (computed over the sample interval).
    pub io_write_bytes_per_sec: u64,
    /// Cumulative bytes read since the session started.
    pub io_read_bytes_total: u64,
    /// Cumulative bytes written since the session started.
    pub io_write_bytes_total: u64,
    pub net_recv_bytes_per_sec: u64,
    pub net_sent_bytes_per_sec: u64,
    /// Cumulative bytes received since the session started (WFP) or estimated.
    pub net_recv_bytes_total: u64,
    /// Cumulative bytes sent since the session started (WFP) or estimated.
    pub net_sent_bytes_total: u64,
    /// Number of active TCP connections for the monitored process group.
    pub net_connection_count: u32,
    pub active_pids: Vec<u32>,
}

/// Lightweight session metadata for the tab bar.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub label: String,
    pub executable: String,
    pub interval_ms: u64,
    pub retention_secs: u64,
    pub active: bool,
    pub network_enabled: bool,
    pub data_point_count: usize,
    pub started_at: DateTime<Local>,
    pub ended_at: Option<DateTime<Local>>,
}

/// Full snapshot of a session for rendering charts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub info: SessionInfo,
    pub data_points: Vec<DataPoint>,
    pub max_cpu: f64,
    pub avg_cpu: f64,
    pub max_ram_mb: f64,
    pub avg_ram_mb: f64,
    pub max_io_bytes: u64,
    pub avg_io_bytes: u64,
    pub max_net_bytes: u64,
    pub avg_net_bytes: u64,
}

/// Supported export formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Csv,
    Json,
}

// ─── Session ──────────────────────────────────────────────

/// A single monitoring session with all its accumulated data.
#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub label: String,
    pub executable: String,
    pub interval_ms: u64,
    pub retention_secs: u64,
    pub active: bool,
    pub network_enabled: bool,
    pub started_at: DateTime<Local>,
    pub ended_at: Option<DateTime<Local>>,
    pub data_points: Vec<DataPoint>,
}

impl Session {
    pub fn new(
        counter: u64,
        label: String,
        executable: String,
        interval_ms: u64,
        retention_secs: u64,
        network_enabled: bool,
    ) -> Self {
        Self {
            id: format!("session-{}", counter),
            label,
            executable,
            interval_ms,
            retention_secs,
            active: true,
            network_enabled,
            started_at: Local::now(),
            ended_at: None,
            data_points: Vec::new(),
        }
    }

    /// Returns lightweight info for the frontend.
    pub fn info(&self) -> SessionInfo {
        SessionInfo {
            id: self.id.clone(),
            label: self.label.clone(),
            executable: self.executable.clone(),
            interval_ms: self.interval_ms,
            retention_secs: self.retention_secs,
            active: self.active,
            network_enabled: self.network_enabled,
            data_point_count: self.data_points.len(),
            started_at: self.started_at,
            ended_at: self.ended_at,
        }
    }

    /// Returns a full snapshot with computed statistics.
    pub fn snapshot(&self) -> SessionSnapshot {
        let count = self.data_points.len() as f64;
        let (sum_cpu, sum_ram, sum_io, sum_net, max_cpu, max_ram, max_io, max_net) =
            self.data_points.iter().fold(
                (0.0f64, 0.0f64, 0u64, 0u64, 0.0f64, 0.0f64, 0u64, 0u64),
                |(sc, sr, si, sn, mc, mr, mi, mn), dp| {
                    let io = dp.io_read_bytes_per_sec + dp.io_write_bytes_per_sec;
                    let net = dp.net_recv_bytes_per_sec + dp.net_sent_bytes_per_sec;
                    (
                        sc + dp.cpu_percent,
                        sr + dp.ram_mb,
                        si + io,
                        sn + net,
                        mc.max(dp.cpu_percent),
                        mr.max(dp.ram_mb),
                        mi.max(io),
                        mn.max(net),
                    )
                },
            );

        let denom = if count > 0.0 { count } else { 1.0 };

        SessionSnapshot {
            info: self.info(),
            data_points: self.data_points.clone(),
            max_cpu: (max_cpu * 100.0).round() / 100.0,
            avg_cpu: (sum_cpu / denom * 100.0).round() / 100.0,
            max_ram_mb: (max_ram * 100.0).round() / 100.0,
            avg_ram_mb: (sum_ram / denom * 100.0).round() / 100.0,
            max_io_bytes: max_io,
            avg_io_bytes: (sum_io as f64 / denom) as u64,
            max_net_bytes: max_net,
            avg_net_bytes: (sum_net as f64 / denom) as u64,
        }
    }

    /// Exports all session data to the requested format.
    pub fn export(&self, format: ExportFormat) -> Result<String, String> {
        match format {
            ExportFormat::Csv => self.export_csv(),
            ExportFormat::Json => self.export_json(),
        }
    }

    fn export_csv(&self) -> Result<String, String> {
        let mut wtr = csv::Writer::from_writer(Vec::new());

        // Write metadata header as comments
        let meta = format!(
            "# ProcessObserver Session Export\n\
             # Executable: {}\n\
             # Session: {}\n\
             # Started: {}\n\
             # Interval: {}ms\n\
             # Points: {}\n",
            self.executable,
            self.label,
            self.started_at.format("%Y-%m-%d %H:%M:%S"),
            self.interval_ms,
            self.data_points.len(),
        );

        // Write CSV header
        wtr.write_record(&[
            "Timestamp",
            "CPU_%",
            "RAM_MB",
            "IO_Read_Bps",
            "IO_Write_Bps",
            "IO_Read_Total",
            "IO_Write_Total",
            "Net_Recv_Bps",
            "Net_Sent_Bps",
            "Net_Recv_Total",
            "Net_Sent_Total",
            "Net_Connections",
            "Active_PIDs",
        ])
        .map_err(|e| format!("CSV write error: {}", e))?;

        for dp in &self.data_points {
            wtr.write_record(&[
                dp.timestamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
                format!("{:.2}", dp.cpu_percent),
                format!("{:.2}", dp.ram_mb),
                dp.io_read_bytes_per_sec.to_string(),
                dp.io_write_bytes_per_sec.to_string(),
                dp.io_read_bytes_total.to_string(),
                dp.io_write_bytes_total.to_string(),
                dp.net_recv_bytes_per_sec.to_string(),
                dp.net_sent_bytes_per_sec.to_string(),
                dp.net_recv_bytes_total.to_string(),
                dp.net_sent_bytes_total.to_string(),
                dp.net_connection_count.to_string(),
                dp.active_pids
                    .iter()
                    .map(|p| p.to_string())
                    .collect::<Vec<_>>()
                    .join(";"),
            ])
            .map_err(|e| format!("CSV write error: {}", e))?;
        }

        let data = wtr.into_inner().map_err(|e| format!("CSV finalize error: {}", e))?;
        let csv_content = String::from_utf8(data).map_err(|e| format!("UTF-8 error: {}", e))?;

        Ok(format!("{}\n{}", meta, csv_content))
    }

    fn export_json(&self) -> Result<String, String> {
        #[derive(Serialize)]
        struct Export {
            metadata: SessionInfo,
            data: Vec<DataPoint>,
        }

        let export = Export {
            metadata: self.info(),
            data: self.data_points.clone(),
        };

        serde_json::to_string_pretty(&export).map_err(|e| format!("JSON serialize error: {}", e))
    }
}

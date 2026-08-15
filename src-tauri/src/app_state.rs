//! Application state management for ProcessObserver.
//!
//! Uses a `Mutex<AppState>` shared across all Tauri command handlers
//! and monitoring tasks.

use crate::session::{DataPoint, ExportFormat, Session};
use chrono::{Duration, Local, Utc};

/// Global application state holding all monitoring sessions.
pub struct AppState {
    sessions: Vec<Session>,
    session_counter: u64,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            session_counter: 0,
        }
    }

    /// Creates a new session, returning its unique ID.
    pub fn create_session(
        &mut self,
        executable: &str,
        interval_ms: u64,
        network_enabled: bool,
        retention_secs: u64,
    ) -> String {
        self.session_counter += 1;
        let now = Local::now();
        let label = format!(
            "Session #{} – {}",
            self.session_counter,
            now.format("%H:%M:%S")
        );
        let session = Session::new(
            self.session_counter,
            label,
            executable.to_string(),
            interval_ms,
            retention_secs,
            network_enabled,
        );
        let id = session.id.clone();
        self.sessions.push(session);
        id
    }

    /// Marks a session as stopped. Data remains in memory.
    pub fn stop_session(&mut self, id: &str) {
        if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
            s.active = false;
            s.ended_at = Some(Local::now());
        }
    }

    /// Completely removes a session and all its data.
    pub fn remove_session(&mut self, id: &str) {
        self.sessions.retain(|s| s.id != id);
    }

    /// Checks if a session is currently active (polling).
    pub fn is_session_active(&self, id: &str) -> bool {
        self.sessions
            .iter()
            .any(|s| s.id == id && s.active)
    }

    /// Returns whether network monitoring is enabled for this session.
    pub fn is_network_enabled(&self, id: &str) -> bool {
        self.sessions
            .iter()
            .any(|s| s.id == id && s.network_enabled)
    }

    /// Appends a data point to the specified session.
    pub fn push_data_point(&mut self, id: &str, point: DataPoint) {
        if let Some(s) = self.sessions.iter_mut().find(|s| s.id == id) {
            s.data_points.push(point);

            // Only retain points within the configured retention window.
            let retention_secs = s.retention_secs;
            if retention_secs > 0 {
                let cutoff = Utc::now() - Duration::seconds(retention_secs as i64);
                s.data_points.retain(|p| p.timestamp >= cutoff);
            }

            // Hard cap to bound memory regardless of retention settings.
            if s.data_points.len() > 100_000 {
                // Drop oldest half
                let keep = s.data_points.len() - 50_000;
                s.data_points = s.data_points.split_off(s.data_points.len() - keep);
            }
        }
    }

    /// Returns lightweight session info for the frontend tab bar.
    pub fn get_session_infos(&self) -> Vec<crate::session::SessionInfo> {
        self.sessions.iter().map(|s| s.info()).collect()
    }

    /// Returns a full snapshot of a session's data for rendering.
    pub fn get_session_snapshot(&self, id: &str) -> Option<crate::session::SessionSnapshot> {
        self.sessions.iter().find(|s| s.id == id).map(|s| s.snapshot())
    }

    /// Exports session data in the requested format.
    pub fn export_session(&self, id: &str, format: ExportFormat) -> Result<String, String> {
        let session = self
            .sessions
            .iter()
            .find(|s| s.id == id)
            .ok_or_else(|| format!("Session not found: {}", id))?;
        session.export(format)
    }
}

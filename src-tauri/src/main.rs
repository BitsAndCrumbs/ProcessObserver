//! ProcessObserver – Entry point
//!
//! Prevents the console window from appearing in release mode
//! and delegates to the Tauri library.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    process_observer_lib::run();
}

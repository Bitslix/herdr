use std::path::PathBuf;

use super::{ClipboardImage, ForegroundJob, Signal};

pub fn raise_server_nofile_limit() {}

pub fn foreground_job(_child_pid: u32) -> Option<ForegroundJob> {
    None
}

pub fn foreground_group_leader_job(_process_group_id: u32) -> Option<ForegroundJob> {
    None
}

pub fn foreground_process_group_id(_child_pid: u32) -> Option<u32> {
    None
}

pub fn process_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

pub fn session_processes(_child_pid: u32) -> Vec<u32> {
    Vec::new()
}

pub fn signal_processes(_pids: &[u32], _signal: Signal) {}

pub fn process_exists(_pid: u32) -> bool {
    false
}

pub fn write_clipboard(_bytes: &[u8]) -> bool {
    false
}

pub fn open_url(_url: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "opening URLs is not supported on this platform",
    ))
}

pub fn read_clipboard_image() -> Option<ClipboardImage> {
    None
}

pub fn show_desktop_notification(_title: &str, _body: Option<&str>) -> std::io::Result<bool> {
    Ok(false)
}

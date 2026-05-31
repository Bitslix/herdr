use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::io::Write;

use super::{ClipboardImage, ForegroundJob, ForegroundProcess, Signal};

const MAX_PATH: usize = 260;

mod ffi {
    pub const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    pub const PROCESS_TERMINATE: u32 = 0x0001;
    pub const FALSE: i32 = 0;
    pub const TH32CS_SNAPPROCESS: u32 = 0x00000002;
    pub const INVALID_HANDLE_VALUE: isize = -1;

    #[repr(C)]
    pub struct PROCESSENTRY32W {
        pub dw_size: u32,
        cnt_usage: u32,
        pub th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        pub th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        pub sz_exe_file: [u16; super::MAX_PATH],
    }

    extern "system" {
        pub fn CreateToolhelp32Snapshot(
            dw_flags: u32,
            th32_process_id: u32,
        ) -> *mut std::ffi::c_void;

        pub fn Process32FirstW(
            h_snapshot: *mut std::ffi::c_void,
            lppe: *mut PROCESSENTRY32W,
        ) -> i32;

        pub fn Process32NextW(
            h_snapshot: *mut std::ffi::c_void,
            lppe: *mut PROCESSENTRY32W,
        ) -> i32;

        pub fn OpenProcess(
            dw_desired_access: u32,
            b_inherit_handle: i32,
            dw_process_id: u32,
        ) -> *mut std::ffi::c_void;

        pub fn CloseHandle(h_object: *mut std::ffi::c_void) -> i32;

        pub fn TerminateProcess(
            h_process: *mut std::ffi::c_void,
            u_exit_code: u32,
        ) -> i32;
    }
}

fn enumerate_processes() -> Vec<(u32, u32, String)> {
    let mut entries = Vec::new();
    unsafe {
        let snapshot = ffi::CreateToolhelp32Snapshot(ffi::TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() || snapshot as isize == ffi::INVALID_HANDLE_VALUE {
            return entries;
        }

        let mut entry = ffi::PROCESSENTRY32W {
            dw_size: std::mem::size_of::<ffi::PROCESSENTRY32W>() as u32,
            cnt_usage: 0,
            th32_process_id: 0,
            th32_default_heap_id: 0,
            th32_module_id: 0,
            cnt_threads: 0,
            th32_parent_process_id: 0,
            pc_pri_class_base: 0,
            dw_flags: 0,
            sz_exe_file: [0u16; MAX_PATH],
        };

        if ffi::Process32FirstW(snapshot, &mut entry) != 0 {
            loop {
                let name_len = entry
                    .sz_exe_file
                    .iter()
                    .position(|&c| c == 0)
                    .unwrap_or(MAX_PATH);
                let name = String::from_utf16_lossy(&entry.sz_exe_file[..name_len]);
                entries.push((entry.th32_process_id, entry.th32_parent_process_id, name));

                if ffi::Process32NextW(snapshot, &mut entry) == 0 {
                    break;
                }
            }
        }

        ffi::CloseHandle(snapshot);
    }
    entries
}

fn find_process_children(pid: u32, entries: &[(u32, u32, String)]) -> Vec<ForegroundProcess> {
    entries
        .iter()
        .filter(|&&(_, parent, _)| parent == pid)
        .map(|&(child_pid, _, ref name)| ForegroundProcess {
            pid: child_pid,
            name: name.clone(),
            argv0: None,
            argv: None,
            cmdline: None,
        })
        .collect()
}

pub fn raise_server_nofile_limit() {}

pub fn foreground_job(child_pid: u32) -> Option<ForegroundJob> {
    if child_pid == 0 {
        return None;
    }

    let entries = enumerate_processes();
    let has_process = entries.iter().any(|&(pid, _, _)| pid == child_pid);
    if !has_process {
        return None;
    }

    let processes = find_process_children(child_pid, &entries);
    Some(ForegroundJob {
        process_group_id: child_pid,
        processes,
    })
}

pub fn foreground_group_leader_job(process_group_id: u32) -> Option<ForegroundJob> {
    if process_group_id == 0 {
        return None;
    }
    let entries = enumerate_processes();
    let has_process = entries.iter().any(|&(pid, _, _)| pid == process_group_id);
    if !has_process {
        return None;
    }
    let processes = find_process_children(process_group_id, &entries);
    Some(ForegroundJob {
        process_group_id,
        processes,
    })
}

pub fn foreground_process_group_id(_child_pid: u32) -> Option<u32> {
    None
}

pub fn process_cwd(_pid: u32) -> Option<PathBuf> {
    None
}

pub fn session_processes(child_pid: u32) -> Vec<u32> {
    let mut pids = Vec::new();
    let entries = enumerate_processes();
    for &(pid, parent, _) in &entries {
        if pid == child_pid || parent == child_pid {
            pids.push(pid);
        }
    }
    pids
}

pub fn signal_processes(pids: &[u32], signal: Signal) {
    match signal {
        Signal::Hangup => return,
        Signal::Terminate | Signal::Kill => {}
    }

    for &pid in pids {
        if pid == 0 {
            continue;
        }

        unsafe {
            let handle = ffi::OpenProcess(ffi::PROCESS_TERMINATE, ffi::FALSE, pid);
            if handle.is_null() {
                continue;
            }
            ffi::TerminateProcess(handle, 1);
            ffi::CloseHandle(handle);
        }
    }
}

pub fn process_exists(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    unsafe {
        let handle = ffi::OpenProcess(ffi::PROCESS_QUERY_LIMITED_INFORMATION, ffi::FALSE, pid);
        if handle.is_null() {
            let err = std::io::Error::last_os_error();
            return err.raw_os_error() == Some(5);
        }
        ffi::CloseHandle(handle);
        true
    }
}

pub fn write_clipboard(bytes: &[u8]) -> bool {
    let mut child = match Command::new("powershell")
        .args(["-NoProfile", "-Command", "$input | Set-Clipboard"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };

    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(bytes);
    }

    child.wait().map(|s| s.success()).unwrap_or(false)
}

pub fn open_url(url: &str) -> std::io::Result<()> {
    Command::new("cmd")
        .args(["/c", "start", "", url])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

pub fn read_clipboard_image() -> Option<ClipboardImage> {
    None
}

pub fn show_desktop_notification(title: &str, body: Option<&str>) -> std::io::Result<bool> {
    let body = body.unwrap_or("");
    let mut child = Command::new("powershell")
        .args(["-NoProfile", "-Command", &format!(
            "$t = [Console]::In.ReadLine(); \
             $b = [Console]::In.ReadLine(); \
             $xml = New-Object Windows.Data.Xml.Dom.XmlDocument; \
             $xml.LoadXml('<toast><visual><binding template=\"ToastText02\"><text id=\"1\"></text><text id=\"2\"></text></binding></visual></toast>'); \
             $texts = $xml.GetElementsByTagName('text'); \
             $texts.Item(0).AppendChild($xml.CreateTextNode($t)) | Out-Null; \
             $texts.Item(1).AppendChild($xml.CreateTextNode($b)) | Out-Null; \
             $toast = New-Object Windows.UI.Notifications.ToastNotification $xml; \
             [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('Herdr').Show($toast)"
        )])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = writeln!(stdin, "{title}");
        let _ = writeln!(stdin, "{body}");
    }

    let status = child.wait()?;
    Ok(status.success())
}

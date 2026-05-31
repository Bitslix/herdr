# Windows Build Plan

This document outlines every change needed to make herdr compile and run on native Windows
(MSVC toolchain, no WSL). The guiding principle: **all existing Unix/Linux/macOS code stays
exactly as-is**. Every Windows change is additive — new `#[cfg(windows)]` blocks alongside
existing `#[cfg(unix)]` blocks, new platform files, conditional dependency re-exports.

---

## Overview

herdr is built on Unix primitives: Unix domain sockets, file descriptors, `/proc`, PTY master
fds, `SCM_RIGHTS` fd passing, `libc::kill`, chmod, symlinks, and XDG paths. Windows provides
none of these directly. The project already has a `src/platform/` abstraction layer and
`#[cfg(not(unix))]` fallbacks in several places, which provides the structural pattern.

---

## Priority tiers

- **P0** — must compile and run a basic session (server + panes)
- **P1** — parity on core features (clipboard, notifications, config paths)
- **P2** — nice-to-have but can ship without (handoff, URL open, sound)
- **P3** — CI/release infra (GitHub Actions, Nix, release assets)

---

## 1. Build system (`build.rs`)

**State:** `zig_target()` panics on unknown targets. All existing logic unchanged.

- [ ] Add Windows targets to `zig_target()`:
  - `x86_64-pc-windows-msvc` → `x86_64-windows`
  - `aarch64-pc-windows-msvc` → `aarch64-windows`
- [ ] Verify `libghostty-vt` (Zig library) compiles for Windows. The Zig build targets
      `x86_64-windows` / `aarch64-windows` already exist upstream; test.
- [ ] The macOS link step (`rustc-link-arg` for `.a`) was already gated behind
      `target.contains("apple-darwin")` — that stays, no change needed.
- [ ] If `libghostty-vt` does not support Windows yet: gate the ghostty parser behind
      `#[cfg(not(windows))]` and provide a no-op VT parser fallback.

## 2. Dependencies (`Cargo.toml`)

- [ ] Add `[target.'cfg(windows)'.dependencies]` section with `uds_windows` for IPC
      compatibility (provides `UnixStream`/`UnixListener` API on top of named pipes).
- [ ] The `libc` crate compiles on Windows but many symbols don't exist at link time.
      Gate all `use libc::...` imports behind `#[cfg(unix)]` (they already are in most
      files — verify).

## 3. Platform abstraction layer (`src/platform/`)

### 3.1 `src/platform/mod.rs` — additive only

Existing Linux/macOS blocks stay untouched. Add after them:

```rust
#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;
```

Update the fallback selector from `#[cfg(not(any(target_os = "linux", target_os = "macos")))]`
to `#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]`.

### 3.2 New file: `src/platform/windows.rs` (entirely new)

Implement every function from `mod.rs`.

| Function | Strategy |
|----------|----------|
| `raise_server_nofile_limit()` | No-op (no rlimit on Windows). |
| `foreground_job(pid)` | `CreateToolhelp32Snapshot` + `Process32First/Next` to enumerate. No direct "foreground job" concept; approximate via parent PID chain to the console host. |
| `foreground_group_leader_job(pgid)` | Map to PID (no process groups). |
| `foreground_process_group_id(pid)` | Always `None`. |
| `foreground_process_group_id_for_tty_fd(fd)` | Always `None`. |
| `process_cwd(pid)` | `OpenProcess` + `GetProcessImageFileName`. CWD is harder — `NtQueryInformationProcess` or `None`. |
| `session_processes(pid)` | Enumerate all processes, return empty vec. |
| `signal_processes(pids, signal)` | Map `Terminate`/`Kill` → `TerminateProcess`. `Hangup` → no-op. |
| `process_exists(pid)` | `OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` succeeds. |
| `write_clipboard(bytes)` | Win32 `OpenClipboard` → `EmptyClipboard` → `SetClipboardData`. |
| `read_clipboard_image()` | `OpenClipboard` → `GetClipboardData(CF_DIB)` → encode to PNG in-process. |
| `open_url(url)` | `cmd /c start "" "<url>"`. |
| `show_desktop_notification(title, body)` | PowerShell toast or bundled `SnoreToast.exe`. The ratatui in-app notification already works. |

### 3.3 `src/server/clipboard_image.rs` — additive `#[cfg(windows)]` blocks

No existing code changes. Two issues:
- `libc::geteuid()` line 79 — replace with `std::process::id()` (works everywhere, no cfg needed).
- `.mode(0o600)` via `OpenOptionsExt` — add `#[cfg(windows)]` fallback that opens without `.mode()`.

## 4. IPC: Unix domain sockets

Everywhere the code does `use std::os::unix::net::UnixStream` (or `UnixListener`), it
unconditionally depends on Unix. On Windows these types don't exist, so each import site
needs a path that resolves on both platforms.

**Approach — conditional re-export module (minimal diff per file):**

1. Create `src/ipc/compat.rs`:
   ```rust
   #[cfg(unix)]
   pub use std::os::unix::net::{UnixStream, UnixListener, SocketAddr};
   #[cfg(windows)]
   pub use uds_windows::{UnixStream, UnixListener, SocketAddr};
   ```
   Add similarly for `UnixDatagram` if used anywhere (grep: it's not used).

2. In each of the ~17 files that import these types, change one import line:
   ```diff
   -use std::os::unix::net::UnixStream;
   +use crate::ipc::compat::UnixStream;
   ```
   (and similarly for `UnixListener`).

**This is the only mechanical per-file change.** No logic changes. The Unix code path
still resolves to `std::os::unix::net::UnixStream` — completely unchanged.

**Files needing this import change:**

- `src/client/mod.rs`
- `src/client/input.rs` (uses `std::os::fd::AsRawFd` — see section 6, not IPC)
- `src/server/autodetect.rs`
- `src/server/client_accept.rs`
- `src/server/client_transport.rs`
- `src/server/headless.rs`
- `src/server/handoff.rs` (already `#[cfg(unix)]`, but its `use` lines are also behind `#[cfg(unix)]` — add the re-export import for consistency)
- `src/server/socket_paths.rs` (test code only)
- `src/session.rs`
- `src/ipc.rs` (also uses `PermissionsExt`, see section 8)
- `src/protocol/wire.rs`
- `src/remote.rs`
- `src/api/client.rs`
- `src/api/server.rs`
- `src/api/wait.rs`
- `src/update.rs` (also uses `PermissionsExt`, see section 8)

## 5. Live handoff (`src/server/handoff.rs`)

**Already entirely `#[cfg(unix)]`.** Handoff ships PTY master fds over `SCM_RIGHTS`, which
has no Windows equivalent. The `#[cfg(not(unix))]` stubs in `headless.rs` already return
"unsupported platform" on non-Unix. **No changes needed** to Unix handoff code.

## 6. PTY FD operations (`src/pty/fd.rs`)

Every function is currently `#[cfg(unix)]`. Keep them as-is. Add `#[cfg(windows)]`
implementations alongside:

| Unix fn | Windows equivalent |
|---------|-------------------|
| `duplicate_fd(fd)` | `DuplicateHandle(GetCurrentProcess(), handle, GetCurrentProcess(), &dup, 0, FALSE, DUPLICATE_SAME_ACCESS)` |
| `set_cloexec(fd)` | No-op (handles not inherited by default in modern Rust std). |
| `set_nonblocking(fd)` | `SetNamedPipeHandleState` for pipe handles, or async I/O mode. |
| `duplicate_cloexec_fd(fd)` | `duplicate_fd` without cloexec (no-op on Windows). |
| `poll_read_ready(fd)` | `WaitForSingleObject(handle, timeout_ms)` with `WAIT_OBJECT_0`. |
| `poll_write_ready(fd)` | Same as above, or always-ready for write handles. |
| `resize_pty_fd(fd)` | No-op (`portable-pty` handles PTY resize on Windows via `PtySize`). |

## 7. PTY actor (`src/pty/actor.rs`)

The actor thread is currently `#[cfg(unix)]` via the module gate. The core logic
(read/write loop, state machine, control commands) is not Unix-specific — only the
fd operations are. Options:

- **Option A** (simpler): keep the entire module behind `#[cfg(any(unix, windows))]`,
  abstract the fd ops behind a platform-conditional call in `fd.rs` (which already has
  the right shape).
- **Option B**: make the actor cross-platform by replacing `AsRawFd` usage with
  `std::fs::File` (which works for `Read`/`Write` on both platforms).

The existing tests use `UnixStream::pair()` which doesn't exist on Windows — gate them
behind `#[cfg(unix)]`.

## 8. File permissions (`PermissionsExt`)

Used in 7 files for `mode()`, `set_mode()`, `from_mode()`. Each needs a `#[cfg(windows)]`
alternative.

| File | Unix code | Windows fallback |
|------|-----------|-----------------|
| `src/ipc.rs` | `permissions.set_mode(mode)` | `std::fs::set_permissions` with DACL or just no-op (named pipes handle permissions) |
| `src/server/socket_paths.rs` | `from_mode(0o600)` | Same |
| `src/server/clipboard_image.rs` | `.mode(0o600)` in `OpenOptions` | Use `OpenOptions` without `.mode()` |
| `src/pane.rs` | `is_executable_file` — already has `#[cfg(not(unix))]` returning `true` | No change needed |
| `src/integration/mod.rs` | `make_executable(0o755)` | `#[cfg(windows)]` no-op |
| `src/update.rs` | `PermissionsExt` on downloaded binary | `#[cfg(windows)]` no-op (`.exe` handles executability) |
| `src/persist/restore.rs` | `PermissionsExt` usage | `#[cfg(windows)]` no-op |

## 9. Symlinks (`std::os::unix::fs::symlink`)

**`src/persist/io.rs`** — Keep Unix code behind `#[cfg(unix)]`, add `#[cfg(windows)]` with
`std::os::windows::fs::symlink_file` / `symlink_dir`.

**`src/detect/mod.rs`** — Same pattern.

## 10. Config paths (`src/config/io.rs`)

The existing `config_dir()` uses XDG paths / `$HOME`. Keep as-is behind `#[cfg(unix)]`,
add `#[cfg(windows)]` path using `%APPDATA%` and `%LOCALAPPDATA%`.

```rust
#[cfg(windows)]
pub fn config_dir() -> PathBuf {
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(r"C:\Users\Default\AppData\Roaming"));
    base.join(app_dir_name())
}
```

Same pattern for `state_dir()`.

## 11. Sound (`src/sound.rs`)

The `run_player()` function already dispatches on `cfg!(target_os = "macos")` with a
Linux fallback. Add `#[cfg(windows)]` alternative using `powershell` sound player or
Win32 `PlaySound` via the `windows` crate.

## 12. Remote / SSH (`src/remote.rs`)

- `fits_unix_socket_path()` uses `std::os::unix::ffi::OsStrExt` — gate import behind
  `#[cfg(unix)]`. On Windows, named pipe paths have a 256-char limit; always return `true`.
- `/tmp` fallback path → use `std::env::temp_dir()` cross-platform.

## 13. WSL detection (`src/selection.rs`)

No changes needed. `is_wsl()` tries to read `/proc/...` — on Windows these reads fail
gracefully (returns `None`), `should_prefer_osc52()` falls through to `false`, and the
clipboard write path uses `platform::write_clipboard` as expected.

## 14. `std::os::fd` imports

These appear in ~30 files. Most are `RawFd`, `AsRawFd`, `OwnedFd`, `FromRawFd` tied to
PTY/handoff operations already behind `#[cfg(unix)]`.

**Files that need attention** (unconditional `std::os::fd` import):
- `src/pane.rs` — `std::os::fd::{RawFd, FromRawFd, OwnedFd}` — used in handoff-methods
  that are already `#[cfg(unix)]`. Verify the imports are gated.
- `src/pty/backend.rs` — `std::os::fd::{FromRawFd, OwnedFd}` — replace with Windows
  equivalent via `#[cfg(windows)]` or refactor to use `portable-pty`'s cross-platform API.
- `src/pty/actor.rs` — `std::os::fd::{AsRawFd, OwnedFd, RawFd}` — gate behind `#[cfg(unix)]`
  or provide `#[cfg(windows)]` alternatives using `OwnedHandle`/`AsRawHandle`.

## 15. Kitty graphics

Terminal-agnostic. No changes needed.

## 16. Tests

**Existing tests gated by `#[cfg(target_os = "linux")]` or `#[cfg(unix)]`** — keep as-is,
they simply won't compile on Windows, which is correct.

**Tests that unconditionally use Unix APIs:**
- `src/server/socket_paths.rs` tests — add `#[cfg(unix)]`
- `src/pty/actor.rs` tests — add `#[cfg(unix)]`
- `src/session.rs` tests — add `#[cfg(unix)]` (they use `UnixStream::pair()`, `UnixListener`)
- `src/pty/backend.rs` tests — already `#[cfg(all(test, target_os = "linux"))]`

**New tests to add:**
- `src/platform/windows.rs` — clipboard, process inspection, signal helpers, URL open

## 17. CI and release

### `.github/workflows/ci.yml`

Add to the `os`: `[ubuntu-latest, macos-latest, windows-latest]`:
- `windows-latest` already has Zig support in `mlugg/setup-zig`.
- No special build tools needed (CMake/Ninja are for macOS Zig builds only).
- `just` may not be available on Windows — can run `cargo nextest` directly instead.

### `.github/workflows/release.yml`

Add Windows build targets:
```yaml
- target: x86_64-pc-windows-msvc
  os: windows-latest
  name: herdr-windows-x86_64
- target: aarch64-pc-windows-msvc
  os: windows-latest
  name: herdr-windows-aarch64
```

Update the release artifact file list:
```yaml
files: |
  herdr-linux-x86_64/herdr-linux-x86_64
  herdr-linux-aarch64/herdr-linux-aarch64
  herdr-macos-x86_64/herdr-macos-x86_64
  herdr-macos-aarch64/herdr-macos-aarch64
  herdr-windows-x86_64/herdr-windows-x86_64.exe
  herdr-windows-aarch64/herdr-windows-aarch64.exe
```

Update `website/latest.json` schema to include `windows-x86_64` and `windows-aarch64`.

### `src/update.rs`
- `RemotePlatform::local()` — add `"windows"` for `cfg!(windows)`.
- `RemotePlatform::from_uname()` — Windows remote targets use `uname` via SSH, so no
  change needed (remote machine is still Linux/macOS). Local Windows being the client
  is the new case.

## 18. Implementation order

### Phase 1: Compile on Windows (P0)
1. `build.rs` — add Windows Zig targets, gate ghostty if needed
2. `src/platform/windows.rs` — stubs for every function returning None/empty/false
3. `src/platform/mod.rs` — wire in windows module
4. Add `uds_windows` to `Cargo.toml` under `[target.'cfg(windows)'.dependencies]`
5. Create `src/ipc/compat.rs` — conditional re-exports
6. 17 import-line changes across IPC files
7. `src/pty/fd.rs` — add `#[cfg(windows)]` implementations alongside existing
8. `src/pty/mod.rs` — ungate to `#[cfg(any(unix, windows))]`
9. `src/config/io.rs` — Windows config/state dirs
10. Gate remaining `PermissionsExt`, `symlink`, `OsStrExt` behind `#[cfg(unix)]`
11. Gate `libc::geteuid()` → `std::process::id()`

### Phase 2: Basic functionality (P1)
12. `src/platform/windows.rs` — `write_clipboard`, `process_exists`, `signal_processes`
13. `src/platform/windows.rs` — `open_url`
14. `src/platform/windows.rs` — `show_desktop_notification`
15. `src/sound.rs` — Windows audio player

### Phase 3: Polish (P2)
16. `src/platform/windows.rs` — `foreground_job`, `process_cwd`, remaining
17. Gate Unix-only tests with `#[cfg(unix)]`
18. CI workflow — add `windows-latest`
19. Release workflow — add Windows targets
20. Update `website/latest.json` schema for Windows assets — handled by release workflow, skip manual edit per AGENTS.md rules

### Phase 4: Windows host verification + remaining gaps

21. Cross-compile verification — run `cargo check --target x86_64-pc-windows-msvc` and `cargo check --target aarch64-pc-windows-msvc` on a Windows host or in CI
22. `src/pty/mod.rs` — ungate to `#[cfg(any(unix, windows))]` and align Windows fd stubs with Unix caller signatures
23. `read_clipboard_image()` — implement with `OpenClipboard` → `GetClipboardData(CF_DIB)` → encode to PNG in-process
24. `website/latest.json` — will be updated by release workflow when first Windows release ships; no manual edit needed per AGENTS.md

---

## Summary of what stays untouched

| Area | Status |
|------|--------|
| All existing `#[cfg(unix)]` blocks | Untouched |
| `src/platform/linux.rs` (458 lines) | Untouched |
| `src/platform/macos.rs` (904 lines) | Untouched |
| `src/platform/fallback.rs` (62 lines) | Untouched |
| `src/server/handoff.rs` (entirely `#[cfg(unix)]`) | Untouched |
| `src/handoff_runtime.rs` (entirely `#[cfg(unix)]`) | Untouched |
| `src/detect/` agent detectors | Untouched |
| `src/ui/`, `src/app/`, `src/workspace/` | Untouched |
| `src/input/`, `src/selection.rs`, `src/terminal_notify.rs` | Untouched |
| All unit tests for existing platform modules | Untouched |
| Nix build / flake | Untouched (Linux/macOS only) |

## Summary of what changes

| Change | Files touched | Nature |
|--------|---------------|--------|
| `build.rs` — add Zig targets | 1 | Additive |
| `Cargo.toml` — conditional deps | 1 | Additive |
| `src/platform/mod.rs` — wire windows module | 1 | 3 lines added |
| **New** `src/platform/windows.rs` | 1 (new) | Entirely new |
| `src/ipc/compat.rs` — conditional re-exports | 1 (new) | Entirely new |
| IPC import lines — `UnixStream` → `crate::ipc::compat::UnixStream` | ~17 | 1-line import changes |
| `src/pty/fd.rs` — add `#[cfg(windows)]` fns | 1 | Additive blocks |
| `src/pty/mod.rs` — ungate module guard | 1 | 1-line cfg change |
| `src/config/io.rs` — add Windows paths | 1 | Additive cfg block |
| `src/sound.rs` — add Windows player | 1 | Additive cfg branch |
| `src/server/clipboard_image.rs` — gate mode/permissions | 1 | Cfg guards |
| `src/ipc.rs` — gate PermissionsExt | 1 | Cfg guard |
| `src/persist/io.rs` — gate symlink | 1 | Cfg guard |
| `src/detect/mod.rs` — gate symlink | 1 | Cfg guard |
| `src/remote.rs` — gate OsStrExt | 1 | Cfg guard |
| `src/update.rs` — gate PermissionsExt, add windows platform | 1 | Cfg guard + additive |
| Test files — add `#[cfg(unix)]` to platform-specific tests | ~4 | Cfg guards |
| CI/release YAML — add Windows | 2 | Additive matrix entries |

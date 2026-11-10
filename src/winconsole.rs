//! Console handling for the Windows GUI-subsystem build. Command-line flags (`--version`, `--keys`,
//! ...) attach to the parent console so their output shows up; a plain launch gets a hidden console so
//! child processes (git, node, language servers) never flash a window of their own.

#[cfg(not(windows))]
pub fn init(_cli: bool) {}

#[cfg(windows)]
pub fn init(cli: bool) {
    use windows_sys::Win32::{
        Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::{CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING},
        System::Console::{
            ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole, GetConsoleWindow, GetStdHandle, STD_ERROR_HANDLE,
            STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle,
        },
        UI::WindowsAndMessaging::{SW_HIDE, ShowWindow},
    };
    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }
    // SAFETY: plain Win32 calls with valid arguments; handles opened here live for the whole process.
    unsafe {
        if !GetConsoleWindow().is_null() {
            return; // started from a console (debug build): nothing to do
        }
        if cli && AttachConsole(ATTACH_PARENT_PROCESS) != 0 {
            for (slot, name, access) in [
                (STD_OUTPUT_HANDLE, "CONOUT$", GENERIC_WRITE),
                (STD_ERROR_HANDLE, "CONOUT$", GENERIC_WRITE),
                (STD_INPUT_HANDLE, "CONIN$", GENERIC_READ),
            ] {
                let cur = GetStdHandle(slot);
                if cur.is_null() || cur == INVALID_HANDLE_VALUE {
                    let h = CreateFileW(
                        wide(name).as_ptr(),
                        access,
                        FILE_SHARE_READ | FILE_SHARE_WRITE,
                        std::ptr::null(),
                        OPEN_EXISTING,
                        0,
                        std::ptr::null_mut(),
                    );
                    if h != INVALID_HANDLE_VALUE {
                        SetStdHandle(slot, h);
                    }
                }
            }
            return;
        }
        if AllocConsole() != 0 {
            ShowWindow(GetConsoleWindow(), SW_HIDE);
        }
    }
}

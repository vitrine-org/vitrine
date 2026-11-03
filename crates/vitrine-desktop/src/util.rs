use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::GetClassNameW;
use windows::core::PCWSTR;

/// A NUL-terminated UTF-16 string that can be passed as `PCWSTR`.
pub struct WideStr(Vec<u16>);

impl WideStr {
    pub fn new(s: &str) -> Self {
        Self(s.encode_utf16().chain(std::iter::once(0)).collect())
    }

    pub fn as_pcwstr(&self) -> PCWSTR {
        PCWSTR(self.0.as_ptr())
    }
}

/// Returns the window class name of `hwnd`, or an empty string on failure.
pub fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    let len = unsafe { GetClassNameW(hwnd, &mut buf) };
    String::from_utf16_lossy(&buf[..len.max(0) as usize])
}

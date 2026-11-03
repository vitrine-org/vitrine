//! The user's Windows appearance: light/dark app theme and accent colour.

use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::core::{PCWSTR, w};

fn dword(key: PCWSTR, value: PCWSTR) -> Option<u32> {
    let mut data = 0u32;
    let mut size = 4u32;
    unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key,
            value,
            RRF_RT_REG_DWORD,
            None,
            Some(&mut data as *mut u32 as *mut _),
            Some(&mut size),
        )
        .ok()
        .ok()?;
    }
    Some(data)
}

/// `{"dark": bool, "accent": "#rrggbb"}` for the current user.
pub fn appearance_json() -> String {
    let light = dword(
        w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
        w!("AppsUseLightTheme"),
    )
    .unwrap_or(0)
        != 0;
    // Stored as 0xAABBGGRR.
    let accent = dword(w!(r"Software\Microsoft\Windows\DWM"), w!("AccentColor"))
        .map(|c| {
            format!(
                "#{:02x}{:02x}{:02x}",
                c & 0xFF,
                (c >> 8) & 0xFF,
                (c >> 16) & 0xFF
            )
        })
        .unwrap_or_else(|| "#0078d4".into());
    serde_json::json!({ "dark": !light, "accent": accent }).to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn appearance_is_valid_json() {
        let v: serde_json::Value = serde_json::from_str(&super::appearance_json()).unwrap();
        assert!(v["dark"].is_boolean());
        assert_eq!(v["accent"].as_str().unwrap().len(), 7);
    }
}

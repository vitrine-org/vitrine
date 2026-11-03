//! Power state helpers.

use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

pub(crate) fn status() -> Option<SYSTEM_POWER_STATUS> {
    let mut s = SYSTEM_POWER_STATUS::default();
    // SAFETY: `s` is a valid out-pointer for the duration of the call.
    unsafe { GetSystemPowerStatus(&mut s) }.ok()?;
    Some(s)
}

/// True when running on battery (AC line offline).
pub fn on_battery() -> bool {
    status().is_some_and(|s| s.ACLineStatus == 0)
}

/// True when Windows Battery Saver is active.
pub fn battery_saver() -> bool {
    status().is_some_and(|s| s.SystemStatusFlag == 1)
}

#[cfg(test)]
mod tests {
    #[test]
    fn callable() {
        let _ = super::on_battery();
        let _ = super::battery_saver();
    }
}

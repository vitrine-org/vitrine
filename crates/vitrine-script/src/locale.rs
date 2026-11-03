//! Locale data for widgets (QuickJS has no `Intl`): day and month names,
//! AM/PM designators and the 12/24-hour preference, read from Windows.

use windows::Win32::Globalization::GetLocaleInfoEx;
use windows::core::HSTRING;

const LOCALE_SNAME: u32 = 0x5c;
const LOCALE_S1159: u32 = 0x28;
const LOCALE_S2359: u32 = 0x29;
const LOCALE_STIMEFORMAT: u32 = 0x1003;
const LOCALE_SSHORTDATE: u32 = 0x1f;
const LOCALE_SDAYNAME1: u32 = 0x2a; // Monday
const LOCALE_SABBREVDAYNAME1: u32 = 0x31;
const LOCALE_SMONTHNAME1: u32 = 0x38;
const LOCALE_SABBREVMONTHNAME1: u32 = 0x44;
const LOCALE_IFIRSTDAYOFWEEK: u32 = 0x100c; // 0 = Monday

fn get(locale: Option<&HSTRING>, lctype: u32) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe {
        match locale {
            Some(l) => GetLocaleInfoEx(l, lctype, Some(&mut buf)),
            None => GetLocaleInfoEx(None, lctype, Some(&mut buf)),
        }
    };
    if n <= 1 {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..(n - 1) as usize])
}

/// JSON with `locale`, `days` / `daysShort` (Sunday first, like `Date#getDay`),
/// `months` / `monthsShort`, `am`, `pm`, `hour12`, `shortDate`, `firstDayOfWeek` (0 = Sunday).
pub fn locale_info_json(name: Option<&str>) -> String {
    let h = name.filter(|n| !n.is_empty()).map(HSTRING::from);
    let l = h.as_ref();
    // Windows lists Monday..Sunday; JS uses Sunday..Saturday.
    let days_from =
        |base: u32| -> Vec<String> { (0..7).map(|i| get(l, base + (i + 6) % 7)).collect() };
    let months_from = |base: u32| -> Vec<String> { (0..12).map(|i| get(l, base + i)).collect() };
    let time_fmt = get(l, LOCALE_STIMEFORMAT);
    let first: u32 = get(l, LOCALE_IFIRSTDAYOFWEEK).parse().unwrap_or(0);
    serde_json::json!({
        "locale": get(l, LOCALE_SNAME),
        "days": days_from(LOCALE_SDAYNAME1),
        "daysShort": days_from(LOCALE_SABBREVDAYNAME1),
        "months": months_from(LOCALE_SMONTHNAME1),
        "monthsShort": months_from(LOCALE_SABBREVMONTHNAME1),
        "am": get(l, LOCALE_S1159),
        "pm": get(l, LOCALE_S2359),
        "hour12": !time_fmt.contains('H'),
        "shortDate": get(l, LOCALE_SSHORTDATE),
        "firstDayOfWeek": (first + 1) % 7,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn english_names() {
        let v: serde_json::Value =
            serde_json::from_str(&super::locale_info_json(Some("en-US"))).unwrap();
        assert_eq!(v["days"][0], "Sunday");
        assert_eq!(v["months"][11], "December");
        assert_eq!(v["hour12"], true);
        let it: serde_json::Value =
            serde_json::from_str(&super::locale_info_json(Some("it-IT"))).unwrap();
        assert_eq!(it["days"][1], "lunedì");
        assert_eq!(it["hour12"], false);
    }
}

//! Synchronous sampling of system metrics.

use std::time::Instant;

use serde_json::{Map, Value, json};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE};
use windows::Win32::NetworkManagement::IpHelper::{FreeMibTable, GetIfTable2, MIB_IF_TABLE2};
use windows::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
use windows::Win32::System::SystemInformation::{
    GetSystemInfo, GetTickCount64, GlobalMemoryStatusEx, MEMORYSTATUSEX, SYSTEM_INFO,
};
use windows::Win32::System::Threading::{
    GetSystemTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::core::PCWSTR;

use crate::Metric;
use crate::power;

const TOP_PROCESSES: usize = 10;
const DRIVE_FIXED: u32 = 3;
const IF_TYPE_SOFTWARE_LOOPBACK: u32 = 24;
const IF_OPER_STATUS_UP: i32 = 1;

/// Cumulative CPU times in 100 ns units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTimes {
    pub idle: u64,
    /// Kernel time (includes idle).
    pub kernel: u64,
    pub user: u64,
}

/// CPU usage percentage (0..=100) between two readings; 0 if no time elapsed.
pub fn cpu_usage(prev: CpuTimes, cur: CpuTimes) -> f64 {
    let idle = cur.idle.saturating_sub(prev.idle);
    let total = cur.kernel.saturating_sub(prev.kernel) + cur.user.saturating_sub(prev.user);
    if total == 0 {
        return 0.0;
    }
    (total.saturating_sub(idle) as f64 / total as f64 * 100.0).clamp(0.0, 100.0)
}

/// Bytes per second between two cumulative counters.
pub fn rate_per_sec(prev: u64, cur: u64, secs: f64) -> f64 {
    if secs <= 0.0 {
        return 0.0;
    }
    cur.saturating_sub(prev) as f64 / secs
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn ft_to_u64(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn from_wide(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle was obtained from a successful Win32 call and is closed once.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

#[derive(Default)]
pub struct Sampler {
    prev_cpu: Option<CpuTimes>,
    prev_net: Option<(u64, u64, Instant)>,
}

impl Sampler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Samples the requested metrics; only those keys are present in the result.
    /// CPU and network need two readings: the first sample reports 0.
    pub fn sample(&mut self, metrics: &[Metric]) -> Value {
        let mut out = Map::new();
        for &m in metrics {
            let v = match m {
                Metric::System => system_info(),
                Metric::Cpu => self.cpu(),
                Metric::Memory => memory(),
                Metric::Battery => battery(),
                Metric::Drives => drives(),
                Metric::Processes => processes(),
                Metric::Network => self.network(),
            };
            out.insert(m.as_str().to_string(), v);
        }
        Value::Object(out)
    }

    fn cpu(&mut self) -> Value {
        let cores = logical_cores();
        let usage = match read_cpu_times() {
            Some(cur) => {
                let u = self.prev_cpu.map_or(0.0, |p| cpu_usage(p, cur));
                self.prev_cpu = Some(cur);
                u
            }
            None => 0.0,
        };
        json!({ "usage": round1(usage), "cores": cores })
    }

    fn network(&mut self) -> Value {
        let now = Instant::now();
        let (mut rx, mut tx) = (0.0, 0.0);
        if let Some((irx, itx)) = read_net_octets() {
            if let Some((prx, ptx, t)) = self.prev_net {
                let secs = now.duration_since(t).as_secs_f64();
                rx = rate_per_sec(prx, irx, secs);
                tx = rate_per_sec(ptx, itx, secs);
            }
            self.prev_net = Some((irx, itx, now));
        }
        json!({ "rxPerSec": rx.round() as u64, "txPerSec": tx.round() as u64 })
    }
}

fn read_cpu_times() -> Option<CpuTimes> {
    let (mut idle, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: all three pointers are valid for the call.
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
    Some(CpuTimes {
        idle: ft_to_u64(idle),
        kernel: ft_to_u64(kernel),
        user: ft_to_u64(user),
    })
}

fn logical_cores() -> u32 {
    let mut si = SYSTEM_INFO::default();
    // SAFETY: `si` is a valid out-pointer.
    unsafe { GetSystemInfo(&mut si) };
    si.dwNumberOfProcessors
}

fn memory() -> Value {
    let mut ms = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: dwLength is set, pointer is valid.
    if unsafe { GlobalMemoryStatusEx(&mut ms) }.is_err() {
        return json!({ "total": 0, "available": 0, "used": 0, "load": 0 });
    }
    json!({
        "total": ms.ullTotalPhys,
        "available": ms.ullAvailPhys,
        "used": ms.ullTotalPhys.saturating_sub(ms.ullAvailPhys),
        "load": ms.dwMemoryLoad,
    })
}

fn battery() -> Value {
    let Some(s) = power::status() else {
        return json!({
            "hasBattery": false, "charging": false, "level": null,
            "remainingSeconds": null, "powerSource": "unknown", "saver": false
        });
    };
    // BatteryFlag: 128 = no system battery, 255 = unknown, 8 = charging.
    let has = s.BatteryFlag != 128 && s.BatteryFlag != 255;
    let level = (s.BatteryLifePercent <= 100).then_some(s.BatteryLifePercent);
    let remaining = (s.BatteryLifeTime != u32::MAX).then_some(s.BatteryLifeTime);
    let source = match s.ACLineStatus {
        0 => "battery",
        1 => "ac",
        _ => "unknown",
    };
    json!({
        "hasBattery": has,
        "charging": has && s.BatteryFlag & 8 != 0,
        "level": level,
        "remainingSeconds": remaining,
        "powerSource": source,
        "saver": s.SystemStatusFlag == 1,
    })
}

fn drives() -> Value {
    // SAFETY: plain query with no arguments.
    let mask = unsafe { GetLogicalDrives() };
    let mut list = Vec::new();
    for i in 0..26u32 {
        if mask & (1 << i) == 0 {
            continue;
        }
        let letter = (b'A' + i as u8) as char;
        let root = wide(&format!("{letter}:\\"));
        let root_p = PCWSTR(root.as_ptr());
        // SAFETY: `root` is NUL-terminated and outlives the calls.
        if unsafe { GetDriveTypeW(root_p) } != DRIVE_FIXED {
            continue;
        }
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        // SAFETY: out-pointers are valid locals.
        if unsafe {
            GetDiskFreeSpaceExW(root_p, Some(&mut avail), Some(&mut total), Some(&mut free))
        }
        .is_err()
        {
            continue;
        }
        let mut label = [0u16; 261];
        // SAFETY: the label buffer is valid; other outputs are not requested.
        let label =
            if unsafe { GetVolumeInformationW(root_p, Some(&mut label), None, None, None, None) }
                .is_ok()
            {
                from_wide(&label)
            } else {
                String::new()
            };
        list.push(json!({
            "name": format!("{letter}:"),
            "label": label,
            "total": total,
            "free": free,
            "used": total.saturating_sub(free),
        }));
    }
    Value::Array(list)
}

fn processes() -> Value {
    // SAFETY: snapshot handle is wrapped and closed by OwnedHandle.
    let Ok(snap) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return Value::Array(Vec::new());
    };
    let snap = OwnedHandle(snap);
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut found: Vec<(String, u32, u64)> = Vec::new();
    // SAFETY: `entry.dwSize` is set; the snapshot handle is valid.
    let mut ok = unsafe { Process32FirstW(snap.0, &mut entry) }.is_ok();
    while ok {
        let pid = entry.th32ProcessID;
        if pid != 0
            && let Some(mem) = working_set(pid)
        {
            found.push((from_wide(&entry.szExeFile), pid, mem));
        }
        // SAFETY: same as above.
        ok = unsafe { Process32NextW(snap.0, &mut entry) }.is_ok();
    }
    found.sort_by_key(|e| std::cmp::Reverse(e.2));
    found.truncate(TOP_PROCESSES);
    Value::Array(
        found
            .into_iter()
            .map(|(name, pid, memory)| json!({ "name": name, "pid": pid, "memory": memory }))
            .collect(),
    )
}

fn working_set(pid: u32) -> Option<u64> {
    // SAFETY: failure (protected process, exited) is reported via Err.
    let h =
        OwnedHandle(unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?);
    let mut pmc = PROCESS_MEMORY_COUNTERS {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..Default::default()
    };
    // SAFETY: `pmc.cb` is set and the handle is valid.
    let ok = unsafe { K32GetProcessMemoryInfo(h.0, &mut pmc, pmc.cb) }.as_bool();
    ok.then_some(pmc.WorkingSetSize as u64)
}

/// Summed (received, sent) octets over non-loopback interfaces that are up.
fn read_net_octets() -> Option<(u64, u64)> {
    let mut table: *mut MIB_IF_TABLE2 = std::ptr::null_mut();
    // SAFETY: on success `table` points to a system-allocated table freed below.
    let rc = unsafe { GetIfTable2(&mut table) };
    if rc.0 != 0 || table.is_null() {
        return None;
    }
    let (mut rx, mut tx) = (0u64, 0u64);
    // SAFETY: the table holds `NumEntries` contiguous rows starting at `Table`.
    unsafe {
        let n = (*table).NumEntries as usize;
        let rows = std::slice::from_raw_parts((*table).Table.as_ptr(), n);
        for r in rows {
            // Bit 1 of the flags is FilterInterface (duplicate of a real NIC).
            let filter = r.InterfaceAndOperStatusFlags._bitfield & 0b10 != 0;
            if r.Type != IF_TYPE_SOFTWARE_LOOPBACK && r.OperStatus.0 == IF_OPER_STATUS_UP && !filter
            {
                rx += r.InOctets;
                tx += r.OutOctets;
            }
        }
        FreeMibTable(table as *const _);
    }
    Some((rx, tx))
}

fn system_info() -> Value {
    // SAFETY: plain query with no arguments.
    let uptime_secs = unsafe { GetTickCount64() } / 1000;
    json!({
        "hostname": std::env::var("COMPUTERNAME").unwrap_or_default(),
        "os": os_version(),
        "uptime": uptime_secs,
    })
}

fn reg_string(value: &str) -> Option<String> {
    let key = wide("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion");
    let name = wide(value);
    let mut buf = [0u16; 256];
    let mut size = std::mem::size_of_val(&buf) as u32;
    // SAFETY: `buf`/`size` describe a valid writable buffer.
    let rc = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR(name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };
    (rc.0 == 0).then(|| from_wide(&buf))
}

fn os_version() -> String {
    let mut product = reg_string("ProductName").unwrap_or_else(|| "Windows".to_string());
    let build: u32 = reg_string("CurrentBuildNumber")
        .and_then(|b| b.parse().ok())
        .unwrap_or(0);
    // ProductName still says "Windows 10" on Windows 11.
    if build >= 22000 {
        product = product.replacen("Windows 10", "Windows 11", 1);
    }
    let mut s = product;
    if let Some(v) = reg_string("DisplayVersion") {
        s.push(' ');
        s.push_str(&v);
    }
    if build > 0 {
        s.push_str(&format!(" (build {build})"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_math() {
        let a = CpuTimes {
            idle: 100,
            kernel: 200,
            user: 100,
        };
        let b = CpuTimes {
            idle: 150,
            kernel: 300,
            user: 200,
        };
        // total delta 200, idle delta 50 -> 75 %
        assert_eq!(cpu_usage(a, b), 75.0);
        assert_eq!(cpu_usage(a, a), 0.0);
        assert_eq!(rate_per_sec(1000, 3000, 2.0), 1000.0);
        assert_eq!(rate_per_sec(3000, 1000, 2.0), 0.0);
        assert_eq!(rate_per_sec(1, 2, 0.0), 0.0);
    }

    #[test]
    fn sane_values() {
        let mut s = Sampler::new();
        let all = s.sample(&Metric::ALL);
        std::thread::sleep(std::time::Duration::from_millis(250));
        let v = s.sample(&Metric::ALL);
        let first_cpu = all["cpu"]["usage"].as_f64().unwrap();
        assert_eq!(first_cpu, 0.0);
        let cpu = v["cpu"]["usage"].as_f64().unwrap();
        assert!((0.0..=100.0).contains(&cpu), "{cpu}");
        assert!(v["cpu"]["cores"].as_u64().unwrap() >= 1);
        let m = &v["memory"];
        assert!(m["total"].as_u64().unwrap() > 0);
        assert!(m["available"].as_u64().unwrap() <= m["total"].as_u64().unwrap());
        assert!(m["load"].as_u64().unwrap() <= 100);
        assert!(!v["system"]["os"].as_str().unwrap().is_empty());
        assert!(v["system"]["uptime"].as_u64().unwrap() > 0);
        assert!(v["battery"]["hasBattery"].is_boolean());
        assert!(!v["drives"].as_array().unwrap().is_empty());
        let procs = v["processes"].as_array().unwrap();
        assert!(!procs.is_empty() && procs.len() <= TOP_PROCESSES);
        assert!(v["network"]["rxPerSec"].is_u64());
    }

    #[test]
    fn only_requested_keys() {
        let v = Sampler::new().sample(&[Metric::Memory, Metric::Battery]);
        let mut keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        assert_eq!(keys, ["battery", "memory"]);
        assert!(Sampler::new().sample(&[]).as_object().unwrap().is_empty());
    }
}

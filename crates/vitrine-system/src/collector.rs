//! Background collector: samples each metric at its own interval and emits
//! one merged snapshot per tick.

use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde_json::Value;
use windows::Win32::System::Threading::{
    GetCurrentThread, SetThreadInformation, THREAD_POWER_THROTTLING_CURRENT_VERSION,
    THREAD_POWER_THROTTLING_EXECUTION_SPEED, THREAD_POWER_THROTTLING_STATE, ThreadPowerThrottling,
};

use crate::{Metric, Sampler};

pub const MIN_INTERVAL_MS: u32 = 1000;

struct Entry {
    metric: Metric,
    base_ms: u32,
    next_due: Instant,
}

/// Pure scheduling logic; the clock is injected through the `now` arguments.
pub struct Scheduler {
    entries: Vec<Entry>,
    multiplier: f32,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self { entries: Vec::new(), multiplier: 1.0 }
    }
}

impl Scheduler {
    pub fn new() -> Self {
        Self::default()
    }

    fn interval(&self, base_ms: u32) -> Duration {
        let ms = (base_ms.max(MIN_INTERVAL_MS) as f64 * f64::from(self.multiplier)).max(f64::from(MIN_INTERVAL_MS));
        Duration::from_millis(ms as u64)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Replaces the plan. Metrics whose interval is unchanged keep their
    /// schedule; new or changed ones are due immediately. Duplicate metrics
    /// keep the shortest interval.
    pub fn set_plan(&mut self, plan: &[(Metric, u32)], now: Instant) {
        let mut next: Vec<Entry> = Vec::new();
        for &(metric, ms) in plan {
            let ms = ms.max(MIN_INTERVAL_MS);
            if let Some(e) = next.iter_mut().find(|e| e.metric == metric) {
                e.base_ms = e.base_ms.min(ms);
                continue;
            }
            let next_due = self
                .entries
                .iter()
                .find(|e| e.metric == metric && e.base_ms == ms)
                .map_or(now, |e| e.next_due);
            next.push(Entry { metric, base_ms: ms, next_due });
        }
        self.entries = next;
    }

    /// Changes the interval multiplier (battery saver). Pending deadlines are
    /// pulled in if they are further away than the new interval.
    pub fn set_multiplier(&mut self, m: f32, now: Instant) {
        self.multiplier = if m.is_finite() && m > 0.0 { m } else { 1.0 };
        let mult = self.multiplier;
        for e in &mut self.entries {
            let ms = (e.base_ms as f64 * f64::from(mult)).max(f64::from(MIN_INTERVAL_MS));
            let latest = now + Duration::from_millis(ms as u64);
            e.next_due = e.next_due.min(latest);
        }
    }

    /// Makes every metric due right away (used when resuming from pause).
    pub fn make_all_due(&mut self, now: Instant) {
        for e in &mut self.entries {
            e.next_due = now;
        }
    }

    /// Returns the metrics due at `now` and schedules their next run.
    pub fn take_due(&mut self, now: Instant) -> Vec<Metric> {
        let mut due = Vec::new();
        for i in 0..self.entries.len() {
            if self.entries[i].next_due <= now {
                let iv = self.interval(self.entries[i].base_ms);
                self.entries[i].next_due = now + iv;
                due.push(self.entries[i].metric);
            }
        }
        due
    }

    /// Earliest upcoming deadline, if any metric is scheduled.
    pub fn next_wakeup(&self) -> Option<Instant> {
        self.entries.iter().map(|e| e.next_due).min()
    }
}

struct State {
    plan: Vec<(Metric, u32)>,
    plan_dirty: bool,
    paused: bool,
    resumed: bool,
    multiplier: f32,
    multiplier_dirty: bool,
    stop: bool,
}

struct Shared {
    state: Mutex<State>,
    cv: Condvar,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
}

pub struct Collector {
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

impl Collector {
    /// Starts the worker thread. It sleeps (no wakeups) until a plan is set.
    pub fn start(on_snapshot: impl Fn(Value) + Send + 'static) -> Collector {
        let shared = Arc::new(Shared {
            state: Mutex::new(State {
                plan: Vec::new(),
                plan_dirty: false,
                paused: false,
                resumed: false,
                multiplier: 1.0,
                multiplier_dirty: false,
                stop: false,
            }),
            cv: Condvar::new(),
        });
        let worker = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("vitrine-system-collector".into())
            .spawn(move || run(worker, on_snapshot))
            .map_err(|e| log::error!("failed to spawn collector thread: {e}"))
            .ok();
        Collector { shared, handle }
    }

    /// Sets what to sample: `(metric, interval in ms)`, intervals below 1000 are raised.
    /// An empty plan puts the thread to sleep.
    pub fn set_plan(&self, plan: Vec<(Metric, u32)>) {
        let mut s = self.shared.lock();
        s.plan = plan;
        s.plan_dirty = true;
        self.shared.cv.notify_all();
    }

    pub fn set_paused(&self, paused: bool) {
        let mut s = self.shared.lock();
        if s.paused && !paused {
            s.resumed = true;
        }
        s.paused = paused;
        self.shared.cv.notify_all();
    }

    /// Scales every interval (e.g. 2.0 under battery saver).
    pub fn set_interval_multiplier(&self, m: f32) {
        let mut s = self.shared.lock();
        s.multiplier = m;
        s.multiplier_dirty = true;
        self.shared.cv.notify_all();
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        {
            let mut s = self.shared.lock();
            s.stop = true;
            self.shared.cv.notify_all();
        }
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Opt this thread into EcoQoS so Windows runs it on efficient cores.
fn enable_eco_qos() {
    let state = THREAD_POWER_THROTTLING_STATE {
        Version: THREAD_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: THREAD_POWER_THROTTLING_EXECUTION_SPEED,
    };
    // SAFETY: `state` outlives the call and the size matches the struct.
    let r = unsafe {
        SetThreadInformation(
            GetCurrentThread(),
            ThreadPowerThrottling,
            std::ptr::from_ref(&state).cast(),
            std::mem::size_of::<THREAD_POWER_THROTTLING_STATE>() as u32,
        )
    };
    if let Err(e) = r {
        log::debug!("EcoQoS not applied: {e}");
    }
}

fn run(shared: Arc<Shared>, on_snapshot: impl Fn(Value)) {
    enable_eco_qos();
    let mut sampler = Sampler::new();
    let mut sched = Scheduler::new();
    let mut state = shared.lock();
    loop {
        if state.stop {
            return;
        }
        let now = Instant::now();
        if state.plan_dirty {
            state.plan_dirty = false;
            sched.set_plan(&state.plan, now);
        }
        if state.multiplier_dirty {
            state.multiplier_dirty = false;
            sched.set_multiplier(state.multiplier, now);
        }
        if state.resumed {
            state.resumed = false;
            sched.make_all_due(now);
        }
        if state.paused || sched.is_empty() {
            state = shared.cv.wait(state).unwrap_or_else(|p| p.into_inner());
            continue;
        }
        let due = sched.take_due(now);
        if due.is_empty() {
            let wait = sched.next_wakeup().map_or(Duration::from_secs(1), |t| t.saturating_duration_since(now));
            state = shared.cv.wait_timeout(state, wait).unwrap_or_else(|p| p.into_inner()).0;
            continue;
        }
        drop(state);
        let snapshot = sampler.sample(&due);
        on_snapshot(snapshot);
        state = shared.lock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn scheduler_intervals_and_merging() {
        let t0 = Instant::now();
        let mut s = Scheduler::new();
        assert!(s.is_empty() && s.next_wakeup().is_none());
        s.set_plan(&[(Metric::Cpu, 1000), (Metric::Memory, 3000), (Metric::Battery, 10)], t0);
        // Everything due at start; Battery's interval is clamped to 1000.
        assert_eq!(s.take_due(t0), vec![Metric::Cpu, Metric::Memory, Metric::Battery]);
        assert!(s.take_due(t0 + ms(500)).is_empty());
        assert_eq!(s.next_wakeup(), Some(t0 + ms(1000)));
        assert_eq!(s.take_due(t0 + ms(1000)), vec![Metric::Cpu, Metric::Battery]);
        assert_eq!(s.take_due(t0 + ms(2000)), vec![Metric::Cpu, Metric::Battery]);
        assert_eq!(s.take_due(t0 + ms(3000)), vec![Metric::Cpu, Metric::Memory, Metric::Battery]);
    }

    #[test]
    fn scheduler_plan_update_keeps_phase() {
        let t0 = Instant::now();
        let mut s = Scheduler::new();
        s.set_plan(&[(Metric::Cpu, 2000), (Metric::Drives, 5000)], t0);
        s.take_due(t0);
        // Cpu unchanged keeps its deadline, Drives changes -> due now, Network new -> due now.
        s.set_plan(&[(Metric::Cpu, 2000), (Metric::Drives, 4000), (Metric::Network, 1000)], t0 + ms(500));
        assert_eq!(s.take_due(t0 + ms(500)), vec![Metric::Drives, Metric::Network]);
        assert_eq!(s.take_due(t0 + ms(2000)), vec![Metric::Cpu, Metric::Network]);
        s.set_plan(&[], t0 + ms(2000));
        assert!(s.is_empty() && s.take_due(t0 + ms(9000)).is_empty());
    }

    #[test]
    fn scheduler_multiplier_and_resume() {
        let t0 = Instant::now();
        let mut s = Scheduler::new();
        s.set_plan(&[(Metric::Cpu, 1000)], t0);
        s.take_due(t0);
        s.set_multiplier(3.0, t0);
        assert_eq!(s.next_wakeup(), Some(t0 + ms(1000))); // already pending, not pushed out
        assert_eq!(s.take_due(t0 + ms(1000)), vec![Metric::Cpu]);
        assert_eq!(s.next_wakeup(), Some(t0 + ms(4000)));
        // Back to 1x pulls the deadline in.
        s.set_multiplier(1.0, t0 + ms(1500));
        assert_eq!(s.next_wakeup(), Some(t0 + ms(2500)));
        s.make_all_due(t0 + ms(1600));
        assert_eq!(s.take_due(t0 + ms(1600)), vec![Metric::Cpu]);
        // Invalid multipliers fall back to 1.
        s.set_multiplier(f32::NAN, t0);
        s.set_multiplier(-2.0, t0);
    }

    #[test]
    fn collector_emits_pauses_and_drops() {
        let (tx, rx) = mpsc::channel();
        let c = Collector::start(move |v| {
            let _ = tx.send(v);
        });
        // Empty plan: nothing is emitted.
        assert!(rx.recv_timeout(ms(300)).is_err());
        c.set_plan(vec![(Metric::Memory, 1000), (Metric::Cpu, 1000)]);
        let v = rx.recv_timeout(ms(3000)).expect("first snapshot");
        let obj = v.as_object().unwrap();
        assert!(obj.contains_key("memory") && obj.contains_key("cpu") && obj.len() == 2);
        // Paused: no further snapshots.
        c.set_paused(true);
        while rx.try_recv().is_ok() {}
        assert!(rx.recv_timeout(ms(1500)).is_err());
        c.set_paused(false);
        assert!(rx.recv_timeout(ms(3000)).is_ok());
        let start = Instant::now();
        drop(c);
        assert!(start.elapsed() < ms(2000));
    }
}

//! Host load: CPU, memory, network, disk activity and free space.
//!
//! Plain system calls only (no performance counters, which load a pile of DLLs and are
//! slow to open): a sample costs microseconds, and the thread sleeps in between. Rates
//! are averaged over the time since the previous sample. Samples are taken every few
//! seconds and sent together at the client's interval.

use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rdp_companion_proto::{DiskLoad, HostStats, Message, Volume};
use windows::Win32::Foundation::{CloseHandle, FILETIME, HANDLE, NO_ERROR};
use windows::Win32::NetworkManagement::IpHelper::{
    FreeMibTable, GetIfTable2, IF_TYPE_SOFTWARE_LOOPBACK, MIB_IF_TABLE2,
};
use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAGS_AND_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW, GetDriveTypeW,
    GetLogicalDrives, OPEN_EXISTING,
};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{DISK_PERFORMANCE, IOCTL_DISK_PERFORMANCE};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::GetSystemTimes;
use windows::core::HSTRING;

use crate::channel::Channel;

/// The first sample goes out this soon after an interval is set, so the client has
/// something to show right away.
const FIRST_SAMPLE: Duration = Duration::from_secs(1);
/// The host is sampled this often; samples go to the client in a batch at the interval it
/// asked for, so trend lines get detail without more messages.
const SAMPLE_EVERY: Duration = Duration::from_secs(5);
const DRIVE_FIXED: u32 = 3;

#[derive(Default)]
struct Control {
    interval: Option<Duration>,
    changed: bool,
    stop: bool,
}

/// Reports host stats while attached, at the interval the client asks for.
pub(crate) struct Stats {
    control: Arc<(Mutex<Control>, Condvar)>,
}

impl Stats {
    pub(crate) fn start(channel: Arc<Channel>) -> Self {
        let control: Arc<(Mutex<Control>, Condvar)> = Arc::default();
        let shared = Arc::clone(&control);
        let spawned = std::thread::Builder::new()
            .name("stats".into())
            .spawn(move || run(&channel, &shared));
        if let Err(error) = spawned {
            tracing::warn!(%error, "host stats unavailable");
        }
        Self { control }
    }

    /// Report every `secs` seconds (0 stops reporting).
    pub(crate) fn set_interval(&self, secs: u32) {
        let (lock, changed) = &*self.control;
        let mut control = lock.lock().unwrap_or_else(PoisonError::into_inner);
        control.interval = (secs > 0).then(|| Duration::from_secs(u64::from(secs)));
        control.changed = true;
        changed.notify_all();
    }
}

impl Drop for Stats {
    fn drop(&mut self) {
        let (lock, changed) = &*self.control;
        lock.lock().unwrap_or_else(PoisonError::into_inner).stop = true;
        changed.notify_all();
    }
}

fn run(channel: &Channel, control: &(Mutex<Control>, Condvar)) {
    let (lock, changed) = control;
    let mut sampler = Sampler::new();
    let mut batch: Vec<HostStats> = Vec::new();
    let mut next_sample: Option<Instant> = None;
    let mut next_send: Option<Instant> = None;
    let mut state = lock.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        if state.stop {
            return;
        }
        if std::mem::take(&mut state.changed) {
            // A new interval: the first sample goes out soon (the baseline is reset so it
            // covers a short, recent span rather than the time spent off).
            sampler = Sampler::new();
            batch.clear();
            let first = state.interval.map(|_| Instant::now() + FIRST_SAMPLE);
            next_sample = first;
            next_send = first;
        }
        let Some(interval) = state.interval else {
            state = changed.wait(state).unwrap_or_else(PoisonError::into_inner);
            continue;
        };
        let now = Instant::now();
        let wake = match (next_sample, next_send) {
            (Some(a), Some(b)) => a.min(b),
            (a, b) => a.or(b).unwrap_or(now),
        };
        if wake > now {
            state = changed
                .wait_timeout(state, wake - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
            continue;
        }
        drop(state);
        if next_sample.is_some_and(|at| at <= now) {
            batch.push(sampler.sample());
            next_sample = Some(now + SAMPLE_EVERY.min(interval));
        }
        if next_send.is_some_and(|at| at <= now) {
            if !batch.is_empty() && !channel.send(&Message::Stats(std::mem::take(&mut batch))) {
                return;
            }
            next_send = Some(now + interval);
        }
        state = lock.lock().unwrap_or_else(PoisonError::into_inner);
    }
}

fn ticks(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}

/// (idle, total) processor time across all CPUs, in 100 ns ticks.
fn cpu_times() -> Option<(u64, u64)> {
    let (mut idle, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: three out-pointers to locals.
    unsafe { GetSystemTimes(Some(&mut idle), Some(&mut kernel), Some(&mut user)) }.ok()?;
    // Kernel time includes idle time.
    Some((ticks(idle), ticks(kernel) + ticks(user)))
}

/// Bytes received and sent, in total, across the machine's network interfaces.
fn network_totals() -> (u64, u64) {
    let mut table: *mut MIB_IF_TABLE2 = core::ptr::null_mut();
    // SAFETY: the API allocates the table; freed below with FreeMibTable.
    if unsafe { GetIfTable2(&mut table) } != NO_ERROR || table.is_null() {
        return (0, 0);
    }
    // SAFETY: the table holds `NumEntries` rows, valid until FreeMibTable.
    let rows = unsafe {
        let count = (*table).NumEntries as usize;
        core::slice::from_raw_parts((*table).Table.as_ptr(), count)
    };
    const HARDWARE: u8 = 0x1;
    const FILTER: u8 = 0x2;
    let up: Vec<_> = rows
        .iter()
        .filter(|r| {
            r.OperStatus == IfOperStatusUp
                && r.Type != IF_TYPE_SOFTWARE_LOOPBACK
                // Filter drivers show up as interfaces of their own and would double count.
                && r.InterfaceAndOperStatusFlags._bitfield & FILTER == 0
        })
        .collect();
    // Count the hardware interfaces where there are some: virtual switches and tunnels
    // carry the same traffic again.
    let hardware: Vec<_> = up
        .iter()
        .filter(|r| r.InterfaceAndOperStatusFlags._bitfield & HARDWARE != 0)
        .collect();
    let totals = if hardware.is_empty() {
        up.iter()
            .fold((0, 0), |(rx, tx), r| (rx + r.InOctets, tx + r.OutOctets))
    } else {
        hardware
            .iter()
            .fold((0, 0), |(rx, tx), r| (rx + r.InOctets, tx + r.OutOctets))
    };
    // SAFETY: freeing the table GetIfTable2 allocated.
    unsafe { FreeMibTable(table.cast()) };
    totals
}

/// A physical disk, opened for performance queries only (no read or write access).
struct Disk {
    handle: HANDLE,
    last: Option<DISK_PERFORMANCE>,
}

impl Drop for Disk {
    fn drop(&mut self) {
        // SAFETY: closing the handle opened in `open_disks`.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

fn open_disks() -> Vec<Disk> {
    (0..16)
        .filter_map(|n| {
            let path = HSTRING::from(format!(r"\\.\PhysicalDrive{n}"));
            // SAFETY: opening a device by name with no access rights, just to query it.
            let handle = unsafe {
                CreateFileW(
                    &path,
                    0,
                    FILE_SHARE_READ | FILE_SHARE_WRITE,
                    None,
                    OPEN_EXISTING,
                    FILE_FLAGS_AND_ATTRIBUTES(0),
                    None,
                )
            }
            .ok()?;
            Some(Disk { handle, last: None })
        })
        .collect()
}

fn disk_performance(disk: &Disk) -> Option<DISK_PERFORMANCE> {
    let mut perf = DISK_PERFORMANCE::default();
    let mut returned = 0u32;
    // SAFETY: the output buffer is a DISK_PERFORMANCE of the size given.
    unsafe {
        DeviceIoControl(
            disk.handle,
            IOCTL_DISK_PERFORMANCE,
            None,
            0,
            Some((&mut perf as *mut DISK_PERFORMANCE).cast()),
            size_of::<DISK_PERFORMANCE>() as u32,
            Some(&mut returned),
            None,
        )
    }
    .ok()?;
    Some(perf)
}

fn memory() -> (u64, u64) {
    let mut status = MEMORYSTATUSEX {
        dwLength: size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: `status` has its size set, as the API requires.
    if unsafe { GlobalMemoryStatusEx(&mut status) }.is_err() {
        return (0, 0);
    }
    (status.ullTotalPhys - status.ullAvailPhys, status.ullTotalPhys)
}

fn volumes() -> Vec<Volume> {
    // SAFETY: plain query.
    let mask = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|i| mask & (1 << i) != 0)
        .filter_map(|i| {
            let name = format!("{}:", char::from(b'A' + i));
            let root = HSTRING::from(format!("{name}\\"));
            // SAFETY: plain queries on a drive root.
            unsafe {
                if GetDriveTypeW(&root) != DRIVE_FIXED {
                    return None;
                }
                let (mut total, mut free) = (0u64, 0u64);
                GetDiskFreeSpaceExW(&root, None, Some(&mut total), Some(&mut free)).ok()?;
                Some(Volume { name, free, total })
            }
        })
        .collect()
}

/// Turns cumulative counters into rates between samples.
struct Sampler {
    at: Instant,
    cpu: Option<(u64, u64)>,
    net: (u64, u64),
    disks: Vec<Disk>,
}

impl Sampler {
    fn new() -> Self {
        let mut disks = open_disks();
        for disk in &mut disks {
            disk.last = disk_performance(disk);
        }
        Self {
            at: Instant::now(),
            cpu: cpu_times(),
            net: network_totals(),
            disks,
        }
    }

    fn sample(&mut self) -> HostStats {
        let now = Instant::now();
        let span = now.saturating_duration_since(self.at).as_secs_f64().max(0.001);
        self.at = now;
        let per_second = |delta: u64| (delta as f64 / span) as u64;

        let cpu_now = cpu_times();
        let cpu = match (self.cpu, cpu_now) {
            (Some((idle0, total0)), Some((idle1, total1))) if total1 > total0 => {
                let idle = idle1.saturating_sub(idle0) as f64;
                let total = (total1 - total0) as f64;
                ((1.0 - idle / total) * 100.0).clamp(0.0, 100.0) as f32
            }
            _ => 0.0,
        };
        self.cpu = cpu_now;

        let net_now = network_totals();
        let (net_rx, net_tx) = (
            per_second(net_now.0.saturating_sub(self.net.0)),
            per_second(net_now.1.saturating_sub(self.net.1)),
        );
        self.net = net_now;

        let mut disk: Option<DiskLoad> = None;
        for d in &mut self.disks {
            let Some(now) = disk_performance(d) else { continue };
            if let Some(before) = d.last {
                let load = disk.get_or_insert_with(DiskLoad::default);
                let delta = |a: i64, b: i64| u64::try_from(a.saturating_sub(b)).unwrap_or(0);
                load.read += per_second(delta(now.BytesRead, before.BytesRead));
                load.write += per_second(delta(now.BytesWritten, before.BytesWritten));
                let elapsed = delta(now.QueryTime, before.QueryTime);
                if elapsed > 0 {
                    let idle = delta(now.IdleTime, before.IdleTime).min(elapsed);
                    let busy = (1.0 - idle as f64 / elapsed as f64) * 100.0;
                    load.busy = load.busy.max(busy.clamp(0.0, 100.0) as f32);
                }
            }
            d.last = Some(now);
        }

        let (mem_used, mem_total) = memory();
        HostStats {
            span: span as f32,
            cpu,
            mem_used,
            mem_total,
            net_rx,
            net_tx,
            disk,
            volumes: volumes(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_this_machine() {
        let mut sampler = Sampler::new();
        std::thread::sleep(Duration::from_millis(200));
        let stats = sampler.sample();
        eprintln!("{stats:?}");
        assert!(stats.span > 0.1);
        assert!((0.0..=100.0).contains(&stats.cpu));
        assert!(stats.mem_total > 0 && stats.mem_used <= stats.mem_total);
        assert!(!stats.volumes.is_empty(), "a fixed drive");
        if let Some(disk) = &stats.disk {
            assert!((0.0..=100.0).contains(&disk.busy));
        }
    }
}

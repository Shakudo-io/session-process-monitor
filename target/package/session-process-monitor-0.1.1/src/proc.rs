use std::collections::{HashMap, HashSet};
use std::fs;
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use crate::app::ProcessSnapshot;

struct CpuSample {
    total_time: u64,
    timestamp: Instant,
}

struct DiskSample {
    read_bytes: u64,
    write_bytes: u64,
    timestamp: Instant,
}

static CPU_SAMPLES: OnceLock<Mutex<HashMap<u32, CpuSample>>> = OnceLock::new();
static DISK_SAMPLES: OnceLock<Mutex<HashMap<u32, DiskSample>>> = OnceLock::new();

pub fn collect_processes() -> Vec<ProcessSnapshot> {
    let mut processes = Vec::new();
    let entries = match fs::read_dir("/proc") {
        Ok(entries) => entries,
        Err(_) => return processes,
    };

    let now = Instant::now();
    let ticks_per_second = ticks_per_second();
    let page_size = page_size_bytes();
    let mut seen_pids: HashSet<u32> = HashSet::new();

    let mut cpu_samples = match CPU_SAMPLES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        Ok(guard) => guard,
        Err(_) => return processes,
    };

    let mut disk_samples = match DISK_SAMPLES
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
    {
        Ok(guard) => guard,
        Err(_) => return processes,
    };

    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(_) => continue,
        };
        let pid = match entry
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        {
            Some(pid) => pid,
            None => continue,
        };

        let stat_path = format!("/proc/{pid}/stat");
        let stat_content = match read_to_string(&stat_path) {
            Some(content) => content,
            None => continue,
        };
        let (name, total_time) = match parse_stat(&stat_content) {
            Some(values) => values,
            None => continue,
        };

        let cmdline_path = format!("/proc/{pid}/cmdline");
        let mut cmdline = match read_to_string(&cmdline_path) {
            Some(content) => parse_cmdline(&content),
            None => String::new(),
        };
        if cmdline.trim().is_empty() {
            cmdline = name.clone();
        }

        let rss = match read_rss_bytes(pid, page_size) {
            Some(value) => value,
            None => 0,
        };

        let (uss, pss) = read_uss_pss(pid);
        let cpu_percent =
            compute_cpu_percent(pid, total_time, now, ticks_per_second, &mut cpu_samples);
        let is_system = is_system_process(&name, &cmdline);

        let (read_bytes, write_bytes) = read_disk_io(pid);
        let (disk_read_rate, disk_write_rate) =
            compute_disk_rates(pid, read_bytes, write_bytes, now, &mut disk_samples);

        processes.push(ProcessSnapshot {
            pid,
            name,
            cmdline,
            cpu_percent,
            uss,
            pss,
            rss,
            is_system,
            growth_rate: None,
            disk_read_rate,
            disk_write_rate,
        });
        seen_pids.insert(pid);
    }

    cpu_samples.retain(|pid, _| seen_pids.contains(pid));
    disk_samples.retain(|pid, _| seen_pids.contains(pid));
    processes
}

fn read_to_string(path: &str) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(content) => Some(content),
        Err(_) => None,
    }
}

fn parse_stat(stat_content: &str) -> Option<(String, u64)> {
    let start = stat_content.find('(')?;
    let end = stat_content.rfind(')')?;
    if end <= start {
        return None;
    }
    let name = stat_content[start + 1..end].to_string();
    let rest = stat_content[end + 1..].trim();
    let fields: Vec<&str> = rest.split_whitespace().collect();
    if fields.len() <= 12 {
        return None;
    }
    let utime = fields.get(11)?.parse::<u64>().ok()?;
    let stime = fields.get(12)?.parse::<u64>().ok()?;
    Some((name, utime.saturating_add(stime)))
}

fn parse_cmdline(cmdline_content: &str) -> String {
    cmdline_content.replace('\0', " ").trim().to_string()
}

fn read_rss_bytes(pid: u32, page_size: Option<u64>) -> Option<u64> {
    let page_size = page_size?;
    let statm_path = format!("/proc/{pid}/statm");
    let statm_content = read_to_string(&statm_path)?;
    let mut parts = statm_content.split_whitespace();
    let _size = parts.next()?;
    let resident = parts.next()?;
    let resident_pages = resident.parse::<u64>().ok()?;
    Some(resident_pages.saturating_mul(page_size))
}

fn read_uss_pss(pid: u32) -> (u64, u64) {
    match read_smaps_rollup(pid).or_else(|| read_smaps(pid)) {
        Some((uss, pss)) => (uss, pss),
        None => (0, 0),
    }
}

fn read_smaps_rollup(pid: u32) -> Option<(u64, u64)> {
    let path = format!("/proc/{pid}/smaps_rollup");
    let content = read_to_string(&path)?;
    let mut pss_bytes = 0u64;
    let mut private_clean_bytes = 0u64;
    let mut private_dirty_bytes = 0u64;

    for line in content.lines() {
        if line.starts_with("Pss:") {
            if let Some(value) = parse_kb_line(line) {
                pss_bytes = pss_bytes.saturating_add(value);
            }
        } else if line.starts_with("Private_Clean:") {
            if let Some(value) = parse_kb_line(line) {
                private_clean_bytes = private_clean_bytes.saturating_add(value);
            }
        } else if line.starts_with("Private_Dirty:") {
            if let Some(value) = parse_kb_line(line) {
                private_dirty_bytes = private_dirty_bytes.saturating_add(value);
            }
        }
    }

    let uss_bytes = private_clean_bytes.saturating_add(private_dirty_bytes);
    Some((uss_bytes, pss_bytes))
}

fn read_smaps(pid: u32) -> Option<(u64, u64)> {
    let path = format!("/proc/{pid}/smaps");
    let content = read_to_string(&path)?;
    let mut pss_bytes = 0u64;
    let mut private_clean_bytes = 0u64;
    let mut private_dirty_bytes = 0u64;

    for line in content.lines() {
        if line.starts_with("Pss:") {
            if let Some(value) = parse_kb_line(line) {
                pss_bytes = pss_bytes.saturating_add(value);
            }
        } else if line.starts_with("Private_Clean:") {
            if let Some(value) = parse_kb_line(line) {
                private_clean_bytes = private_clean_bytes.saturating_add(value);
            }
        } else if line.starts_with("Private_Dirty:") {
            if let Some(value) = parse_kb_line(line) {
                private_dirty_bytes = private_dirty_bytes.saturating_add(value);
            }
        }
    }

    let uss_bytes = private_clean_bytes.saturating_add(private_dirty_bytes);
    Some((uss_bytes, pss_bytes))
}

fn parse_kb_line(line: &str) -> Option<u64> {
    let mut parts = line.split_whitespace();
    let _label = parts.next()?;
    let value = parts.next()?;
    let kb = value.parse::<u64>().ok()?;
    Some(kb.saturating_mul(1024))
}

fn is_system_process(name: &str, cmdline: &str) -> bool {
    const EXCLUDES: [&str; 7] = [
        "pilot-agent",
        "envoy",
        "ttyd",
        "jupyter-lab",
        "code-server",
        "timeout.py",
        "listener.py",
    ];

    EXCLUDES
        .iter()
        .any(|item| name == *item || cmdline.contains(item))
        || name == "sh"
}

pub fn read_disk_io(pid: u32) -> (u64, u64) {
    let path = format!("/proc/{pid}/io");
    let content = match read_to_string(&path) {
        Some(c) => c,
        None => return (0, 0),
    };

    let mut read_bytes = 0u64;
    let mut write_bytes = 0u64;

    for line in content.lines() {
        if line.starts_with("read_bytes:") {
            if let Some(value) = parse_io_line(line) {
                read_bytes = value;
            }
        } else if line.starts_with("write_bytes:") {
            if let Some(value) = parse_io_line(line) {
                write_bytes = value;
            }
        }
    }

    (read_bytes, write_bytes)
}

fn parse_io_line(line: &str) -> Option<u64> {
    let mut parts = line.split_whitespace();
    let _label = parts.next()?;
    let value = parts.next()?;
    value.parse::<u64>().ok()
}

fn compute_disk_rates(
    pid: u32,
    read_bytes: u64,
    write_bytes: u64,
    now: Instant,
    samples: &mut HashMap<u32, DiskSample>,
) -> (Option<f64>, Option<f64>) {
    let (read_rate, write_rate) = if let Some(previous) = samples.get(&pid) {
        let elapsed_seconds = now.duration_since(previous.timestamp).as_secs_f64();
        if elapsed_seconds > 0.0 {
            let read_delta = read_bytes.saturating_sub(previous.read_bytes) as f64;
            let write_delta = write_bytes.saturating_sub(previous.write_bytes) as f64;
            let read_mb_per_sec = read_delta / 1024.0 / 1024.0 / elapsed_seconds;
            let write_mb_per_sec = write_delta / 1024.0 / 1024.0 / elapsed_seconds;
            (Some(read_mb_per_sec), Some(write_mb_per_sec))
        } else {
            (None, None)
        }
    } else {
        (None, None)
    };

    samples.insert(
        pid,
        DiskSample {
            read_bytes,
            write_bytes,
            timestamp: now,
        },
    );

    (read_rate, write_rate)
}

fn compute_cpu_percent(
    pid: u32,
    total_time: u64,
    now: Instant,
    ticks_per_second: Option<f64>,
    samples: &mut HashMap<u32, CpuSample>,
) -> f64 {
    let mut cpu_percent = 0.0;

    if let Some(ticks_per_second) = ticks_per_second {
        if let Some(previous) = samples.get(&pid) {
            let elapsed_seconds = now.duration_since(previous.timestamp).as_secs_f64();
            if elapsed_seconds > 0.0 {
                let delta_ticks = total_time.saturating_sub(previous.total_time) as f64;
                let cpu_seconds = delta_ticks / ticks_per_second;
                cpu_percent = (cpu_seconds / elapsed_seconds) * 100.0;
            }
        }
    }

    samples.insert(
        pid,
        CpuSample {
            total_time,
            timestamp: now,
        },
    );

    cpu_percent
}

fn ticks_per_second() -> Option<f64> {
    let stat_content = read_to_string("/proc/stat")?;
    let first_line = stat_content.lines().next()?;
    if !first_line.starts_with("cpu ") {
        return None;
    }
    let mut total_jiffies = 0u64;
    for value in first_line.split_whitespace().skip(1) {
        if let Ok(parsed) = value.parse::<u64>() {
            total_jiffies = total_jiffies.saturating_add(parsed);
        }
    }

    let uptime_content = read_to_string("/proc/uptime")?;
    let uptime_value = uptime_content.split_whitespace().next()?;
    let uptime_seconds = uptime_value.parse::<f64>().ok()?;

    let cpu_count = match std::thread::available_parallelism() {
        Ok(count) => count.get() as f64,
        Err(_) => 1.0,
    };

    if uptime_seconds <= 0.0 || cpu_count <= 0.0 {
        return None;
    }

    Some(total_jiffies as f64 / uptime_seconds / cpu_count)
}

fn page_size_bytes() -> Option<u64> {
    let smaps_content = read_to_string("/proc/self/smaps")?;
    for line in smaps_content.lines() {
        if line.starts_with("KernelPageSize:") || line.starts_with("MMUPageSize:") {
            if let Some(value) = parse_kb_line(line) {
                return Some(value);
            }
        }
    }
    None
}

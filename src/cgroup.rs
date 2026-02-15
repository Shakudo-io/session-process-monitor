use std::env;
use std::fs;
use std::path::Path;

use crate::app::PodMemorySnapshot;

pub fn read_pod_memory() -> PodMemorySnapshot {
    let threshold = read_threshold_percent();

    let (usage, limit) = if Path::new("/sys/fs/cgroup/memory.max").exists() {
        read_cgroup_v2()
    } else if Path::new("/sys/fs/cgroup/memory/memory.limit_in_bytes").exists() {
        read_cgroup_v1()
    } else {
        (0, None)
    };

    PodMemorySnapshot {
        cgroup_usage: usage,
        cgroup_limit: limit,
        rss_sum: 0,
        terminator_threshold_percent: threshold,
    }
}

#[derive(Clone, Debug)]
pub struct CpuQuota {
    pub cores: Option<f64>,
}

pub fn read_cpu_quota() -> CpuQuota {
    if Path::new("/sys/fs/cgroup/cpu.max").exists() {
        read_cpu_cgroup_v2()
    } else if Path::new("/sys/fs/cgroup/cpu/cpu.cfs_quota_us").exists() {
        read_cpu_cgroup_v1()
    } else {
        CpuQuota { cores: None }
    }
}

fn read_cpu_cgroup_v2() -> CpuQuota {
    let content = match read_string("/sys/fs/cgroup/cpu.max") {
        Some(c) => c,
        None => return CpuQuota { cores: None },
    };

    let parts: Vec<&str> = content.trim().split_whitespace().collect();
    if parts.len() != 2 {
        return CpuQuota { cores: None };
    }

    let quota_str = parts[0];
    let period_str = parts[1];

    if quota_str == "max" {
        return CpuQuota { cores: None };
    }

    let quota: f64 = match quota_str.parse() {
        Ok(v) => v,
        Err(_) => return CpuQuota { cores: None },
    };
    let period: f64 = match period_str.parse() {
        Ok(v) => v,
        Err(_) => return CpuQuota { cores: None },
    };

    if period <= 0.0 {
        return CpuQuota { cores: None };
    }

    CpuQuota {
        cores: Some(quota / period),
    }
}

fn read_cpu_cgroup_v1() -> CpuQuota {
    let quota = match read_i64("/sys/fs/cgroup/cpu/cpu.cfs_quota_us") {
        Some(v) => v,
        None => return CpuQuota { cores: None },
    };
    let period = match read_i64("/sys/fs/cgroup/cpu/cpu.cfs_period_us") {
        Some(v) => v,
        None => return CpuQuota { cores: None },
    };

    if quota <= 0 || period <= 0 {
        return CpuQuota { cores: None };
    }

    CpuQuota {
        cores: Some(quota as f64 / period as f64),
    }
}

fn read_threshold_percent() -> u8 {
    match env::var("HYPERPLANE_SESSION_PROCESS_TERMINATOR_THRESHOLD_PERCENT") {
        Ok(value) => value.parse::<u8>().unwrap_or(80),
        Err(_) => 80,
    }
}

fn read_cgroup_v2() -> (u64, Option<u64>) {
    let usage = read_u64("/sys/fs/cgroup/memory.current").unwrap_or(0);
    let limit = match read_string("/sys/fs/cgroup/memory.max") {
        Some(value) => {
            let trimmed = value.trim();
            if trimmed == "max" {
                None
            } else {
                trimmed.parse::<u64>().ok()
            }
        }
        None => None,
    };
    (usage, limit)
}

fn read_cgroup_v1() -> (u64, Option<u64>) {
    let usage = read_u64("/sys/fs/cgroup/memory/memory.usage_in_bytes").unwrap_or(0);
    let limit = match read_u64("/sys/fs/cgroup/memory/memory.limit_in_bytes") {
        Some(value) => {
            if value > (1u64 << 62) {
                None
            } else {
                Some(value)
            }
        }
        None => None,
    };
    (usage, limit)
}

fn read_string(path: &str) -> Option<String> {
    match fs::read_to_string(path) {
        Ok(content) => Some(content),
        Err(_) => None,
    }
}

fn read_u64(path: &str) -> Option<u64> {
    let content = read_string(path)?;
    content.trim().parse::<u64>().ok()
}

fn read_i64(path: &str) -> Option<i64> {
    let content = read_string(path)?;
    content.trim().parse::<i64>().ok()
}

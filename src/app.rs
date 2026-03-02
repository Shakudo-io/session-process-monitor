use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::time::{Duration, Instant};

use crate::{cgroup, guard, health, proc, supervisor};

#[derive(Clone, Debug)]
pub struct ProcessSnapshot {
    pub pid: u32,
    pub name: String,
    pub cmdline: String,
    pub cpu_percent: f64,
    pub uss: u64,
    pub pss: u64,
    pub rss: u64,
    pub is_system: bool,
    pub growth_rate: Option<f64>,
    pub disk_read_rate: Option<f64>,
    pub disk_write_rate: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct PodMemorySnapshot {
    pub cgroup_usage: u64,
    pub cgroup_limit: Option<u64>,
    pub rss_sum: u64,
    pub terminator_threshold_percent: u8,
}

#[derive(Clone, Debug)]
pub struct ViewState {
    pub sort_column: SortColumn,
    pub sort_ascending: bool,
    pub filter: String,
    pub selected: usize,
    pub filter_active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortColumn {
    Uss,
    Pss,
    Rss,
    Cpu,
    GrowthRate,
    Name,
    Pid,
    Cmdline,
    DiskRead,
    DiskWrite,
}

#[derive(Clone, Debug)]
pub struct StatusMessage {
    pub text: String,
    pub expires_at: Instant,
}

#[derive(Clone, Debug)]
pub struct KillConfirmation {
    pub target: KillTarget,
}

#[derive(Clone, Debug)]
pub enum KillTarget {
    Process {
        pid: u32,
        name: String,
        is_system: bool,
    },
    Managed {
        index: usize,
        command: String,
        pid: Option<u32>,
        pgid: Option<u32>,
    },
}

#[derive(Clone, Debug)]
pub enum GuardAlert {
    Triggered { percent: f64, ticks_remaining: u8 },
    Exhausted { percent: f64 },
}

#[derive(Clone, Debug)]
pub struct App {
    pub processes: Vec<ProcessSnapshot>,
    pub pod_memory: PodMemorySnapshot,
    pub cpu_cores: Option<f64>,
    pub view_state: ViewState,
    pub growth_windows: HashMap<u32, VecDeque<(Instant, u64)>>,
    pub running: bool,
    pub status_message: Option<StatusMessage>,
    pub confirm_kill: Option<KillConfirmation>,
    pub managed_children: Vec<crate::supervisor::ManagedChild>,
    pub guard: Option<crate::guard::Guard>,
    pub guard_alert: Option<GuardAlert>,
    pub supervisor_mode: bool,
    pub local_supervisor: bool,
}

impl App {
    pub fn new() -> Self {
        let threshold = match env::var("HYPERPLANE_SESSION_PROCESS_TERMINATOR_THRESHOLD_PERCENT") {
            Ok(value) => value.parse::<u8>().unwrap_or(80),
            Err(_) => 80,
        };

        Self {
            processes: Vec::new(),
            pod_memory: PodMemorySnapshot {
                cgroup_usage: 0,
                cgroup_limit: None,
                rss_sum: 0,
                terminator_threshold_percent: threshold,
            },
            cpu_cores: None,
            view_state: ViewState {
                sort_column: SortColumn::Uss,
                sort_ascending: false,
                filter: String::new(),
                selected: 0,
                filter_active: false,
            },
            growth_windows: HashMap::new(),
            running: true,
            status_message: None,
            confirm_kill: None,
            managed_children: Vec::new(),
            guard: None,
            guard_alert: None,
            supervisor_mode: false,
            local_supervisor: false,
        }
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        if let Some(message) = &self.status_message {
            if now >= message.expires_at {
                self.status_message = None;
            }
        }

        if !self.local_supervisor {
            self.read_shared_state();
        }

        let mut processes = proc::collect_processes();
        let mut pod_memory = cgroup::read_pod_memory();
        pod_memory.rss_sum = processes.iter().map(|process| process.rss).sum();
        let cpu_quota = cgroup::read_cpu_quota();
        self.cpu_cores = cpu_quota.cores;
        let mut seen_pids: HashSet<u32> = HashSet::new();
        for process in processes.iter_mut() {
            seen_pids.insert(process.pid);
            let window = self
                .growth_windows
                .entry(process.pid)
                .or_insert_with(VecDeque::new);
            window.push_back((now, process.uss));
            while window.len() > 10 {
                window.pop_front();
            }
            process.growth_rate = compute_growth_rate(window);
        }
        self.growth_windows.retain(|pid, _| seen_pids.contains(pid));

        processes.sort_by(|left, right| match self.view_state.sort_column {
            SortColumn::Uss => left.uss.cmp(&right.uss),
            SortColumn::Pss => left.pss.cmp(&right.pss),
            SortColumn::Rss => left.rss.cmp(&right.rss),
            SortColumn::Cpu => left.cpu_percent.total_cmp(&right.cpu_percent),
            SortColumn::GrowthRate => left
                .growth_rate
                .unwrap_or(0.0)
                .total_cmp(&right.growth_rate.unwrap_or(0.0)),
            SortColumn::Name => left.name.cmp(&right.name),
            SortColumn::Pid => left.pid.cmp(&right.pid),
            SortColumn::Cmdline => left.cmdline.cmp(&right.cmdline),
            SortColumn::DiskRead => left
                .disk_read_rate
                .unwrap_or(0.0)
                .total_cmp(&right.disk_read_rate.unwrap_or(0.0)),
            SortColumn::DiskWrite => left
                .disk_write_rate
                .unwrap_or(0.0)
                .total_cmp(&right.disk_write_rate.unwrap_or(0.0)),
        });

        if !self.view_state.sort_ascending {
            processes.reverse();
        }

        let filter = self.view_state.filter.trim().to_lowercase();
        if !filter.is_empty() {
            processes.retain(|process| {
                process.name.to_lowercase().contains(&filter)
                    || process.cmdline.to_lowercase().contains(&filter)
            });
        }

        if processes.is_empty() {
            self.view_state.selected = 0;
        } else if self.view_state.selected >= processes.len() {
            self.view_state.selected = processes.len().saturating_sub(1);
        }

        self.processes = processes;
        self.pod_memory = pod_memory;
    }

    pub fn read_shared_state(&mut self) {
        let path = "/tmp/spm-state.json";
        let content = match std::fs::read_to_string(path) {
            Ok(content) => content,
            Err(_) => {
                self.managed_children.clear();
                self.guard = None;
                self.supervisor_mode = false;
                return;
            }
        };

        let timestamp = match extract_json_string(&content, "timestamp") {
            Some(value) => value,
            None => {
                self.managed_children.clear();
                self.guard = None;
                self.supervisor_mode = false;
                return;
            }
        };

        if !is_timestamp_fresh(&timestamp, 5) {
            self.managed_children.clear();
            self.guard = None;
            self.supervisor_mode = false;
            return;
        }

        self.managed_children = parse_child_snapshots(&content);
        self.guard = parse_guard_snapshot(&content);
        self.supervisor_mode = !self.managed_children.is_empty();
    }

    pub fn set_status_message(&mut self, text: String) {
        self.set_status_message_with_duration(text, Duration::from_secs(3));
    }

    pub fn set_status_message_with_duration(&mut self, text: String, duration: Duration) {
        let expires_at = Instant::now() + duration;
        self.status_message = Some(StatusMessage { text, expires_at });
    }

    pub fn selected_process(&self) -> Option<&ProcessSnapshot> {
        if self.processes.is_empty() {
            return None;
        }
        self.processes.get(self.view_state.selected)
    }
}

fn is_timestamp_fresh(timestamp: &str, max_age_seconds: i64) -> bool {
    let ts_epoch = match parse_iso8601_epoch(timestamp) {
        Some(value) => value,
        None => return false,
    };

    let now_epoch = unsafe {
        let mut tv: libc::timeval = std::mem::zeroed();
        libc::gettimeofday(&mut tv, std::ptr::null_mut());
        tv.tv_sec as i64
    };

    if now_epoch < ts_epoch {
        return false;
    }

    now_epoch.saturating_sub(ts_epoch) <= max_age_seconds
}

fn parse_iso8601_epoch(timestamp: &str) -> Option<i64> {
    let year: i32 = timestamp.get(0..4)?.parse().ok()?;
    let month: i32 = timestamp.get(5..7)?.parse().ok()?;
    let day: i32 = timestamp.get(8..10)?.parse().ok()?;
    let hour: i32 = timestamp.get(11..13)?.parse().ok()?;
    let minute: i32 = timestamp.get(14..16)?.parse().ok()?;
    let second: i32 = timestamp.get(17..19)?.parse().ok()?;

    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = year - 1900;
    tm.tm_mon = month - 1;
    tm.tm_mday = day;
    tm.tm_hour = hour;
    tm.tm_min = minute;
    tm.tm_sec = second;
    tm.tm_isdst = 0;

    let epoch = unsafe { libc::timegm(&mut tm as *mut libc::tm) };
    if epoch < 0 {
        return None;
    }
    Some(epoch as i64)
}

fn parse_child_snapshots(content: &str) -> Vec<supervisor::ManagedChild> {
    let children = match extract_json_section(content, "children", '[', ']') {
        Some(value) => value,
        None => return Vec::new(),
    };

    split_json_objects(&children)
        .into_iter()
        .filter_map(|object| parse_child_snapshot(&object))
        .collect()
}

fn parse_child_snapshot(content: &str) -> Option<supervisor::ManagedChild> {
    let index = extract_json_number::<u64>(content, "index")?;
    let index = usize::try_from(index).ok()?;
    let command = extract_json_string(content, "command")?;
    let state_raw = extract_json_string(content, "state")?;
    let total_uss = extract_json_number::<u64>(content, "total_uss")?;
    let health_raw = extract_json_string(content, "health_status")?;
    let pid = extract_json_optional_u32(content, "pid");
    let port = extract_json_optional_u16(content, "health_port");
    let restart_count = extract_json_number::<u32>(content, "restart_count").unwrap_or(0);

    let mut child = supervisor::ManagedChild::new(index, command);
    child.pid = pid;
    child.pgid = pid;
    child.state = parse_child_state(&state_raw);
    child.total_uss = total_uss;
    child.restart_count = restart_count;
    child.health.status = parse_health_status(&health_raw);
    child.health.port = port;
    Some(child)
}

fn parse_guard_snapshot(content: &str) -> Option<guard::Guard> {
    let guard_content = extract_json_section(content, "guard", '{', '}')?;
    let kill_threshold_percent =
        extract_json_number::<u8>(&guard_content, "kill_threshold_percent")?;
    let consecutive_ticks_above =
        extract_json_number::<u8>(&guard_content, "consecutive_ticks_above").unwrap_or(0);
    let total_kills = extract_json_number::<u32>(&guard_content, "total_kills").unwrap_or(0);
    let enabled = extract_json_bool(&guard_content, "enabled").unwrap_or(false);

    let mut config = guard::GuardConfig::default();
    config.kill_threshold_percent = kill_threshold_percent;
    config.enabled = enabled;

    let mut guard = guard::Guard::new(config);
    guard.consecutive_ticks_above = consecutive_ticks_above;
    guard.total_kills = total_kills;
    Some(guard)
}

fn parse_child_state(value: &str) -> supervisor::ChildState {
    if value.starts_with("Running") {
        supervisor::ChildState::Running
    } else if value.starts_with("Stopping") {
        let emergency = value.contains("emergency: true");
        supervisor::ChildState::Stopping { emergency }
    } else if value.starts_with("Restarting") {
        supervisor::ChildState::Restarting
    } else if value.starts_with("Completed") {
        supervisor::ChildState::Completed
    } else if value.starts_with("Failed") {
        supervisor::ChildState::Failed
    } else {
        supervisor::ChildState::Stopped
    }
}

fn parse_health_status(value: &str) -> health::HealthStatus {
    match value {
        "Healthy" => health::HealthStatus::Healthy,
        "Unhealthy" => health::HealthStatus::Unhealthy,
        "Discovering" => health::HealthStatus::Discovering,
        "Probing" => health::HealthStatus::Probing,
        "NotApplicable" => health::HealthStatus::NotApplicable,
        _ => health::HealthStatus::Discovering,
    }
}

fn extract_json_string(content: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\":\"", key);
    let start = content.find(&needle)? + needle.len();
    let mut out = String::new();
    let mut escaped = false;
    let mut found_end = false;
    for ch in content[start..].chars() {
        if escaped {
            match ch {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                other => out.push(other),
            }
            escaped = false;
            continue;
        }
        if ch == '\\' {
            escaped = true;
            continue;
        }
        if ch == '"' {
            found_end = true;
            break;
        }
        out.push(ch);
    }
    if found_end {
        Some(out)
    } else {
        None
    }
}

fn extract_json_number<T: std::str::FromStr>(content: &str, key: &str) -> Option<T> {
    let raw = extract_json_raw_value(content, key)?;
    raw.parse::<T>().ok()
}

fn extract_json_bool(content: &str, key: &str) -> Option<bool> {
    let raw = extract_json_raw_value(content, key)?;
    match raw.as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn extract_json_optional_u32(content: &str, key: &str) -> Option<u32> {
    let raw = extract_json_raw_value(content, key)?;
    if raw == "null" {
        None
    } else {
        raw.parse::<u32>().ok()
    }
}

fn extract_json_optional_u16(content: &str, key: &str) -> Option<u16> {
    let raw = extract_json_raw_value(content, key)?;
    if raw == "null" {
        None
    } else {
        raw.parse::<u16>().ok()
    }
}

fn extract_json_raw_value(content: &str, key: &str) -> Option<String> {
    let needle = format!("\"{}\":", key);
    let start = content.find(&needle)? + needle.len();
    let tail = content[start..].trim_start();

    if tail.starts_with('"') {
        return extract_json_string(content, key);
    }

    let end = tail
        .find(|ch| ch == ',' || ch == '}' || ch == ']')
        .unwrap_or(tail.len());
    Some(tail[..end].trim().to_string())
}

fn extract_json_section(content: &str, key: &str, open: char, close: char) -> Option<String> {
    let needle = format!("\"{}\":", key);
    let key_index = content.find(&needle)? + needle.len();
    let mut index = key_index;
    let bytes = content.as_bytes();
    while index < content.len() && bytes[index].is_ascii_whitespace() {
        index += 1;
    }

    if bytes.get(index).copied()? != open as u8 {
        if let Some(offset) = content[index..].find(open) {
            index += offset;
        } else {
            return None;
        }
    }

    let start = index;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, ch) in content[start..].char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
            continue;
        }

        if ch == open {
            depth += 1;
        } else if ch == close {
            depth -= 1;
            if depth == 0 {
                let end = start + offset;
                return Some(content[start + 1..end].to_string());
            }
        }
    }
    None
}

fn split_json_objects(content: &str) -> Vec<String> {
    let mut objects = Vec::new();
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaped = false;
    let mut start: Option<usize> = None;

    for (index, ch) in content.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }

        if ch == '"' {
            in_string = true;
            continue;
        }

        if ch == '{' {
            if depth == 0 {
                start = Some(index);
            }
            depth += 1;
        } else if ch == '}' {
            depth -= 1;
            if depth == 0 {
                if let Some(start_index) = start {
                    objects.push(content[start_index + 1..index].to_string());
                }
                start = None;
            }
        }
    }

    objects
}

fn compute_growth_rate(samples: &VecDeque<(Instant, u64)>) -> Option<f64> {
    if samples.len() < 3 {
        return None;
    }
    let first = samples.front()?;
    let last = samples.back()?;
    let elapsed_seconds = last.0.duration_since(first.0).as_secs_f64();
    if elapsed_seconds <= 0.0 {
        return None;
    }
    let elapsed_minutes = elapsed_seconds / 60.0;
    if elapsed_minutes <= 0.0 {
        return None;
    }
    let delta_bytes = last.1 as f64 - first.1 as f64;
    Some(delta_bytes / 1024.0 / 1024.0 / elapsed_minutes)
}

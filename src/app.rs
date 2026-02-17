use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::replay::AppMode;
use crate::{cgroup, proc, recording};

#[derive(Clone, Debug, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
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
    pub pid: u32,
    pub name: String,
    pub is_system: bool,
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
    pub mode: AppMode,
    pub recording_manager: recording::RecordingManager,
    pub watched_pids: HashSet<u32>,
    pub show_cmdline: Option<(u32, String, String)>,
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
            mode: AppMode::Live,
            recording_manager: recording::RecordingManager::new(),
            watched_pids: HashSet::new(),
            show_cmdline: None,
        }
    }

    pub fn toggle_watch(&mut self) {
        if let Some(process) = self.selected_process() {
            let pid = process.pid;
            let name = process.name.clone();
            if self.watched_pids.contains(&pid) {
                self.watched_pids.remove(&pid);
                self.set_status_message(format!("Unwatched: {} (PID {})", name, pid));
            } else {
                self.watched_pids.insert(pid);
                self.set_status_message(format!("Watching: {} (PID {})", name, pid));
            }
        }
    }

    pub fn watched_count(&self) -> usize {
        self.watched_pids.len()
    }

    pub fn tick(&mut self) {
        let now = Instant::now();
        if let Some(message) = &self.status_message {
            if now >= message.expires_at {
                self.status_message = None;
            }
        }

        let mut processes = proc::collect_processes();
        let mut pod_memory = cgroup::read_pod_memory();
        pod_memory.rss_sum = processes.iter().map(|process| process.rss).sum();
        let cpu_quota = cgroup::read_cpu_quota();
        self.cpu_cores = cpu_quota.cores;
        let mut seen_pids: HashSet<u32> = HashSet::new();
        for process in processes.iter_mut() {
            seen_pids.insert(process.pid);
            let window = self.growth_windows.entry(process.pid).or_default();
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

        let prev_pids: HashSet<u32> = self.processes.iter().map(|process| process.pid).collect();
        let curr_pids: HashSet<u32> = processes.iter().map(|process| process.pid).collect();
        if self.mode == AppMode::Live {
            for pid in prev_pids.difference(&curr_pids) {
                if !self.watched_pids.contains(pid) {
                    continue;
                }
                let name = self
                    .processes
                    .iter()
                    .find(|process| process.pid == *pid)
                    .map(|process| process.name.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                if let Some(count) = self.recording_manager.save_recording(*pid, name.clone()) {
                    self.set_status_message(format!(
                        "Recording saved: {} ({} snapshots)",
                        name, count
                    ));
                }
                self.watched_pids.remove(pid);
            }
        }

        self.processes = processes;
        self.pod_memory = pod_memory;

        if self.mode == AppMode::Live {
            let rec_snapshot = recording::RecordingSnapshot {
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
                processes: self.processes.clone(),
                pod_memory: self.pod_memory.clone(),
                cpu_cores: self.cpu_cores,
            };
            self.recording_manager.add_snapshot(rec_snapshot);
        }
    }

    pub fn set_status_message(&mut self, text: String) {
        let expires_at = Instant::now() + Duration::from_secs(3);
        self.status_message = Some(StatusMessage { text, expires_at });
    }

    pub fn selected_process(&self) -> Option<&ProcessSnapshot> {
        if self.processes.is_empty() {
            return None;
        }
        self.processes.get(self.view_state.selected)
    }
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

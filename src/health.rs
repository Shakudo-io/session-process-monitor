use std::collections::HashSet;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct HealthState {
    pub status: HealthStatus,
    pub port: Option<u16>,
    pub endpoint: Option<String>,
    pub last_check: Option<Instant>,
    pub consecutive_failures: u8,
    pub failure_threshold: u8,
    pub discovering_since: Option<Instant>,
    pub baseline_ports: HashSet<u16>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum HealthStatus {
    Discovering,
    Probing,
    Healthy,
    Unhealthy,
    NotApplicable,
}

const HEALTH_PATHS: &[&str] = &["/healthz", "/health", "/ready", "/"];
static TCP_READ_WARNED: AtomicBool = AtomicBool::new(false);

/// Detect TCP LISTEN ports for a process by scanning /proc/{pid}/net/tcp
pub fn detect_listening_ports(pid: u32) -> Vec<u16> {
    let mut owned_inodes = get_owned_socket_inodes(pid);
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries.flatten() {
            if let Some(child_pid) = entry
                .file_name()
                .to_str()
                .and_then(|value| value.parse::<u32>().ok())
            {
                if child_pid == pid {
                    continue;
                }
                if is_descendant(child_pid, pid) {
                    owned_inodes.extend(get_owned_socket_inodes(child_pid));
                }
            }
        }
    }
    let listen_entries = get_listen_entries(pid);
    let mut ports = Vec::new();

    for (port, inode) in &listen_entries {
        if owned_inodes.contains(inode) && !ports.contains(port) {
            ports.push(*port);
        }
    }

    ports
}

pub fn detect_listening_ports_excluding_baseline(pid: u32, baseline: &HashSet<u16>) -> Vec<u16> {
    let all = get_all_listen_ports(pid);
    all.into_iter().filter(|p| !baseline.contains(p)).collect()
}

fn get_all_listen_ports(pid: u32) -> Vec<u16> {
    get_listen_entries(pid)
        .into_iter()
        .map(|(port, _)| port)
        .collect()
}

fn get_owned_socket_inodes(pid: u32) -> HashSet<u64> {
    let mut inodes = HashSet::new();
    let fd_dir = format!("/proc/{pid}/fd");
    if let Ok(entries) = std::fs::read_dir(&fd_dir) {
        for entry in entries.flatten() {
            if let Ok(link) = std::fs::read_link(entry.path()) {
                let link_str = link.to_string_lossy();
                if link_str.starts_with("socket:[") {
                    if let Some(inode_str) = link_str
                        .strip_prefix("socket:[")
                        .and_then(|value| value.strip_suffix(']'))
                    {
                        if let Ok(inode) = inode_str.parse::<u64>() {
                            inodes.insert(inode);
                        }
                    }
                }
            }
        }
    }
    inodes
}

fn get_listen_entries(pid: u32) -> Vec<(u16, u64)> {
    let mut entries = Vec::new();
    for tcp_file in &[
        format!("/proc/{pid}/net/tcp"),
        format!("/proc/{pid}/net/tcp6"),
    ] {
        match std::fs::read_to_string(tcp_file) {
            Ok(content) => {
                for line in content.lines().skip(1) {
                    let fields: Vec<&str> = line.split_whitespace().collect();
                    if fields.len() < 10 {
                        continue;
                    }
                    if fields[3] != "0A" {
                        continue;
                    }

                    let port = fields[1]
                        .split(':')
                        .nth(1)
                        .and_then(|value| u16::from_str_radix(value, 16).ok())
                        .unwrap_or(0);
                    let inode = fields[9].parse::<u64>().unwrap_or(0);
                    if port > 0 && inode > 0 {
                        entries.push((port, inode));
                    }
                }
            }
            Err(_) => {}
        }
    }
    entries
}

fn is_descendant(mut child_pid: u32, ancestor_pid: u32) -> bool {
    loop {
        if child_pid == ancestor_pid {
            return false;
        }
        let ppid = match read_ppid(child_pid) {
            Some(ppid) => ppid,
            None => return false,
        };
        if ppid == ancestor_pid {
            return true;
        }
        if ppid == 0 || ppid == child_pid {
            return false;
        }
        child_pid = ppid;
    }
}

fn read_ppid(pid: u32) -> Option<u32> {
    let stat_path = format!("/proc/{pid}/stat");
    let content = std::fs::read_to_string(&stat_path).ok()?;
    let end = content.rfind(')')?;
    let rest = content[end + 1..].trim();
    let fields: Vec<&str> = rest.split_whitespace().collect();
    fields.get(1)?.parse::<u32>().ok()
}

/// Probe a health endpoint. Returns true if HTTP 2xx received.
pub fn probe_health(port: u16, path: &str) -> bool {
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();

    let stream = match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
        Ok(stream) => stream,
        Err(_) => return false,
    };

    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n\r\n",
        path, port
    );

    let mut stream = stream;
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }

    let mut buf = [0u8; 512];
    match stream.read(&mut buf) {
        Ok(n) if n > 12 => {
            if let Ok(response) = std::str::from_utf8(&buf[..n.min(20)]) {
                response.starts_with("HTTP/1.1 2") || response.starts_with("HTTP/1.0 2")
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Try health endpoints in order, return the first that responds 2xx
pub fn discover_health_endpoint(port: u16) -> Option<String> {
    for path in HEALTH_PATHS {
        if probe_health(port, path) {
            return Some(path.to_string());
        }
    }
    None
}

impl HealthState {
    pub fn new() -> Self {
        Self {
            status: HealthStatus::Discovering,
            port: None,
            endpoint: None,
            last_check: None,
            consecutive_failures: 0,
            failure_threshold: 3,
            discovering_since: None,
            baseline_ports: HashSet::new(),
        }
    }

    pub fn new_with_baseline(pid: u32) -> Self {
        let baseline: HashSet<u16> = get_all_listen_ports(pid).into_iter().collect();
        Self {
            baseline_ports: baseline,
            ..Self::new()
        }
    }

    /// Called every tick. Handles state transitions.
    pub fn tick(&mut self, pid: Option<u32>) {
        match self.status {
            HealthStatus::Discovering => {
                if self.discovering_since.is_none() {
                    self.discovering_since = Some(Instant::now());
                }

                if let Some(since) = self.discovering_since {
                    if since.elapsed() > Duration::from_secs(30) {
                        self.status = HealthStatus::NotApplicable;
                        return;
                    }
                }

                if let Some(pid) = pid {
                    let new_ports = if !self.baseline_ports.is_empty() {
                        detect_listening_ports_excluding_baseline(pid, &self.baseline_ports)
                    } else {
                        detect_listening_ports(pid)
                    };
                    if let Some(&port) = new_ports.first() {
                        self.port = Some(port);
                        self.status = HealthStatus::Probing;
                    }
                }
            }
            HealthStatus::Probing => {
                if let Some(port) = self.port {
                    if let Some(endpoint) = discover_health_endpoint(port) {
                        self.endpoint = Some(endpoint);
                        self.status = HealthStatus::Healthy;
                        self.consecutive_failures = 0;
                    } else {
                        self.status = HealthStatus::Probing;
                    }
                }
            }
            HealthStatus::Healthy => {
                let should_check = match self.last_check {
                    Some(last) => last.elapsed() >= Duration::from_secs(5),
                    None => true,
                };

                if should_check {
                    self.last_check = Some(Instant::now());
                    if let (Some(port), Some(endpoint)) = (self.port, self.endpoint.as_ref()) {
                        if probe_health(port, endpoint) {
                            self.consecutive_failures = 0;
                        } else {
                            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
                            if self.consecutive_failures >= self.failure_threshold {
                                self.status = HealthStatus::Unhealthy;
                            }
                        }
                    }
                }
            }
            HealthStatus::Unhealthy | HealthStatus::NotApplicable => {}
        }
    }

    /// Reset for restart (re-discover port)
    pub fn reset(&mut self) {
        *self = HealthState::new();
    }
}

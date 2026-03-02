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
    let mut ports = Vec::new();

    for tcp_file in &[
        format!("/proc/{pid}/net/tcp"),
        format!("/proc/{pid}/net/tcp6"),
    ] {
        match std::fs::read_to_string(tcp_file) {
            Ok(content) => {
                for line in content.lines().skip(1) {
                    let fields: Vec<&str> = line.split_whitespace().collect();
                    if fields.len() < 4 {
                        continue;
                    }

                    if fields[3] != "0A" {
                        continue;
                    }

                    if let Some(port_hex) = fields[1].split(':').nth(1) {
                        if let Ok(port) = u16::from_str_radix(port_hex, 16) {
                            if port > 0 && !ports.contains(&port) {
                                ports.push(port);
                            }
                        }
                    }
                }
            }
            Err(error) => {
                if !TCP_READ_WARNED.swap(true, Ordering::SeqCst) {
                    eprintln!("[spm] Warning: failed to read {}: {}", tcp_file, error);
                }
            }
        }
    }

    ports
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
                    let ports = detect_listening_ports(pid);
                    if let Some(&port) = ports.first() {
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

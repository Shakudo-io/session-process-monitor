use std::fs::File;
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct ManagedChild {
    pub index: usize,
    pub command: String,
    pub pid: Option<u32>,
    pub pgid: Option<u32>,
    pub state: ChildState,
    pub restart_count: u32,
    pub backoff: BackoffState,
    pub total_uss: u64,
    pub health: crate::health::HealthState,
    pub log_path: Option<PathBuf>,
    pub started_at: Option<Instant>,
    pub last_exit: Option<ExitInfo>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ChildState {
    Running,
    Stopping { emergency: bool },
    Stopped,
    Restarting,
    Completed,
    Failed,
}

#[derive(Clone, Debug)]
pub struct BackoffState {
    pub current_delay: Duration,
    pub max_delay: Duration,
    pub restart_at: Option<Instant>,
    pub last_delay: Duration,
    pub stable_since: Option<Instant>,
    pub stability_threshold: Duration,
}

#[derive(Clone, Debug)]
pub struct ExitInfo {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub killed_by_guard: bool,
    pub killed_by_health: bool,
    pub exited_at: Instant,
}

pub struct SpawnedChild {
    pub pid: u32,
    pub stdout: Option<std::process::ChildStdout>,
    pub stderr: Option<std::process::ChildStderr>,
}

impl BackoffState {
    pub fn new() -> Self {
        Self {
            current_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            restart_at: None,
            last_delay: Duration::from_secs(1),
            stable_since: None,
            stability_threshold: Duration::from_secs(60),
        }
    }

    pub fn next_delay(&mut self) -> Duration {
        let delay = self.current_delay;
        self.last_delay = delay;
        self.current_delay = (self.current_delay * 2).min(self.max_delay);
        delay
    }

    pub fn schedule_restart(&mut self) -> Duration {
        let delay = self.current_delay;
        self.restart_at = Some(Instant::now() + delay);
        self.last_delay = delay;
        self.current_delay = (self.current_delay * 2).min(self.max_delay);
        delay
    }

    pub fn reset_if_stable(&mut self) {
        if let Some(stable_since) = self.stable_since {
            if stable_since.elapsed() >= self.stability_threshold {
                self.current_delay = Duration::from_secs(1);
                self.last_delay = self.current_delay;
            }
        }
    }

    pub fn should_restart(&self, max_restarts: u32, current_count: u32) -> bool {
        current_count <= max_restarts
    }
}

impl ManagedChild {
    pub fn new(index: usize, command: String) -> Self {
        Self {
            index,
            command,
            pid: None,
            pgid: None,
            state: ChildState::Stopped,
            restart_count: 0,
            backoff: BackoffState::new(),
            total_uss: 0,
            health: crate::health::HealthState::new(),
            log_path: None,
            started_at: None,
            last_exit: None,
        }
    }
}

pub fn spawn_child(child: &mut ManagedChild, headless: bool) -> Result<SpawnedChild, String> {
    let (stdout_cfg, stderr_cfg, log_path) = if headless {
        (Stdio::piped(), Stdio::piped(), None)
    } else {
        let name = extract_command_name(&child.command);
        let tmp_path = format!("/tmp/spm-{}-{}.log", child.index, name);
        let file = File::create(&tmp_path).map_err(|e| format!("Failed to create log: {e}"))?;
        let file2 = file
            .try_clone()
            .map_err(|e| format!("Failed to clone log: {e}"))?;
        (
            Stdio::from(file),
            Stdio::from(file2),
            Some(PathBuf::from(tmp_path)),
        )
    };

    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(&child.command);
    cmd.stdout(stdout_cfg).stderr(stderr_cfg);
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    let mut process = cmd.spawn().map_err(|e| format!("Failed to spawn: {e}"))?;
    let pid = process.id();
    let stdout = process.stdout.take();
    let stderr = process.stderr.take();

    child.pid = Some(pid);
    child.pgid = Some(pid);
    child.state = ChildState::Running;
    child.started_at = Some(Instant::now());
    child.backoff.stable_since = Some(Instant::now());
    child.log_path = log_path;
    child.health = crate::health::HealthState::new_with_baseline(pid);

    Ok(SpawnedChild {
        pid,
        stdout,
        stderr,
    })
}

pub fn spawn_output_reader(
    name: String,
    reader: impl std::io::Read + Send + 'static,
    is_stderr: bool,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let buf = BufReader::new(reader);
        for line in buf.lines().flatten() {
            if is_stderr {
                eprintln!("[{}] {}", name, line);
            } else {
                println!("[{}] {}", name, line);
            }
        }
    })
}

pub fn signal_process_group(pgid: u32, signal: i32) -> Result<(), String> {
    let neg_pgid = -(pgid as i32);
    let result = unsafe { libc::kill(neg_pgid, signal) };
    if result != 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        return Err(format!(
            "Failed to signal process group {pgid} with {signal}: {error}"
        ));
    }
    Ok(())
}

pub fn compute_group_uss(pid: u32) -> u64 {
    let mut total = read_pid_uss(pid);
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
                    total = total.saturating_add(read_pid_uss(child_pid));
                }
            }
        }
    }
    total
}

fn read_pid_uss(pid: u32) -> u64 {
    let path = format!("/proc/{pid}/smaps_rollup");
    let content = match std::fs::read_to_string(&path) {
        Ok(content) => content,
        Err(_) => return 0,
    };

    let mut private_clean = 0u64;
    let mut private_dirty = 0u64;
    for line in content.lines() {
        if line.starts_with("Private_Clean:") {
            if let Some(kb) = parse_kb_value(line) {
                private_clean = private_clean.saturating_add(kb.saturating_mul(1024));
            }
        } else if line.starts_with("Private_Dirty:") {
            if let Some(kb) = parse_kb_value(line) {
                private_dirty = private_dirty.saturating_add(kb.saturating_mul(1024));
            }
        }
    }

    private_clean.saturating_add(private_dirty)
}

fn parse_kb_value(line: &str) -> Option<u64> {
    line.split_whitespace().nth(1)?.parse::<u64>().ok()
}

fn is_descendant(child_pid: u32, ancestor_pid: u32) -> bool {
    let stat_path = format!("/proc/{child_pid}/stat");
    if let Ok(content) = std::fs::read_to_string(&stat_path) {
        if let Some(ppid) = parse_ppid(&content) {
            return ppid == ancestor_pid;
        }
    }
    false
}

fn parse_ppid(stat_content: &str) -> Option<u32> {
    let end = stat_content.rfind(')')?;
    let rest = stat_content[end + 1..].trim();
    let fields: Vec<&str> = rest.split_whitespace().collect();
    fields.get(1)?.parse::<u32>().ok()
}

pub fn command_name(command: &str) -> String {
    extract_command_name(command)
}

fn extract_command_name(command: &str) -> String {
    command
        .split_whitespace()
        .find(|value| *value != "sh" && *value != "-c")
        .unwrap_or("unknown")
        .rsplit('/')
        .next()
        .unwrap_or("unknown")
        .chars()
        .take(20)
        .collect()
}

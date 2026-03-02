use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::guard::KillReason;
use crate::{cgroup, guard, health, policy, process, supervisor};

static SHUTDOWN_REQUESTED: AtomicBool = AtomicBool::new(false);

extern "C" fn signal_handler(_sig: libc::c_int) {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

pub fn request_shutdown() {
    SHUTDOWN_REQUESTED.store(true, Ordering::SeqCst);
}

#[derive(Clone, Debug)]
pub enum MonitorEvent {
    Spawn {
        index: usize,
        cmd: String,
        pid: u32,
        log_path: Option<PathBuf>,
    },
    Exit {
        index: usize,
        pid: u32,
        exit_code: Option<i32>,
        signal: Option<i32>,
    },
    GuardWarning {
        pod_percent: f64,
        ticks_remaining: u8,
    },
    GuardKill {
        index: usize,
        pid: u32,
        cmd: String,
        uss: u64,
        pod_percent: f64,
        reason: KillReason,
        emergency: bool,
    },
    GuardExhausted {
        pod_percent: f64,
    },
    HealthOk {
        index: usize,
        port: u16,
        endpoint: String,
    },
    HealthFail {
        index: usize,
        endpoint: String,
        consecutive: u8,
    },
    HealthKill {
        index: usize,
        pid: u32,
        cmd: String,
        endpoint: String,
    },
    Restart {
        index: usize,
        cmd: String,
        new_pid: u32,
        restart_count: u32,
        backoff_secs: f64,
    },
    Completed {
        index: usize,
        cmd: String,
    },
    Failed {
        index: usize,
        cmd: String,
        restart_count: u32,
    },
    StateUpdate,
}

pub fn event_to_json(event: &MonitorEvent) -> Option<String> {
    let ts = chrono_like_timestamp();
    match event {
        MonitorEvent::Spawn {
            index,
            cmd,
            pid,
            log_path,
        } => {
            let log = log_path
                .as_ref()
                .map(|p| format!(",\"log_path\":\"{}\"", p.display()))
                .unwrap_or_default();
            Some(format!(
                "{{\"ts\":\"{ts}\",\"event\":\"spawn\",\"index\":{index},\"cmd\":\"{}\",\"pid\":{pid}{log}}}",
                escape_json(cmd)
            ))
        }
        MonitorEvent::Exit {
            index,
            pid,
            exit_code,
            signal,
        } => {
            let ec = exit_code
                .map(|c| format!(",\"exit_code\":{c}"))
                .unwrap_or_default();
            let sig = signal
                .map(|s| format!(",\"signal\":{s}"))
                .unwrap_or_default();
            Some(format!(
                "{{\"ts\":\"{ts}\",\"event\":\"exit\",\"index\":{index},\"pid\":{pid}{ec}{sig}}}"
            ))
        }
        MonitorEvent::GuardWarning {
            pod_percent,
            ticks_remaining,
        } => Some(format!(
            "{{\"ts\":\"{ts}\",\"event\":\"guard_warning\",\"pod_percent\":{pod_percent:.1},\"ticks_remaining\":{ticks_remaining}}}"
        )),
        MonitorEvent::GuardKill {
            index,
            pid,
            cmd,
            uss,
            pod_percent,
            reason,
            emergency,
        } => {
            let reason_str = match reason {
                crate::guard::KillReason::ThresholdExceeded { pod_percent: p } => {
                    format!("threshold_exceeded({:.1}%)", p)
                }
                crate::guard::KillReason::HealthCheckFailed => {
                    "health_check_failed".to_string()
                }
            };
            Some(format!(
                "{{\"ts\":\"{ts}\",\"event\":\"guard_kill\",\"index\":{index},\"pid\":{pid},\"cmd\":\"{}\",\"uss\":{uss},\"pod_percent\":{pod_percent:.1},\"reason\":\"{reason_str}\",\"emergency\":{emergency}}}",
                escape_json(cmd)
            ))
        }
        MonitorEvent::GuardExhausted { pod_percent } => Some(format!(
            "{{\"ts\":\"{ts}\",\"event\":\"guard_exhausted\",\"pod_percent\":{pod_percent:.1}}}"
        )),
        MonitorEvent::HealthOk {
            index,
            port,
            endpoint,
        } => Some(format!(
            "{{\"ts\":\"{ts}\",\"event\":\"health_ok\",\"index\":{index},\"port\":{port},\"endpoint\":\"{}\"}}",
            escape_json(endpoint)
        )),
        MonitorEvent::HealthFail {
            index,
            endpoint,
            consecutive,
        } => Some(format!(
            "{{\"ts\":\"{ts}\",\"event\":\"health_fail\",\"index\":{index},\"endpoint\":\"{}\",\"consecutive\":{consecutive}}}",
            escape_json(endpoint)
        )),
        MonitorEvent::HealthKill {
            index,
            pid,
            cmd,
            endpoint,
        } => Some(format!(
            "{{\"ts\":\"{ts}\",\"event\":\"health_kill\",\"index\":{index},\"pid\":{pid},\"cmd\":\"{}\",\"endpoint\":\"{}\"}}",
            escape_json(cmd),
            escape_json(endpoint)
        )),
        MonitorEvent::Restart {
            index,
            cmd,
            new_pid,
            restart_count,
            backoff_secs,
        } => Some(format!(
            "{{\"ts\":\"{ts}\",\"event\":\"restart\",\"index\":{index},\"cmd\":\"{}\",\"new_pid\":{new_pid},\"restart_count\":{restart_count},\"backoff_secs\":{backoff_secs:.1}}}",
            escape_json(cmd)
        )),
        MonitorEvent::Completed { index, cmd } => Some(format!(
            "{{\"ts\":\"{ts}\",\"event\":\"completed\",\"index\":{index},\"cmd\":\"{}\"}}",
            escape_json(cmd)
        )),
        MonitorEvent::Failed {
            index,
            cmd,
            restart_count,
        } => Some(format!(
            "{{\"ts\":\"{ts}\",\"event\":\"failed\",\"index\":{index},\"cmd\":\"{}\",\"restart_count\":{restart_count}}}",
            escape_json(cmd)
        )),
        MonitorEvent::StateUpdate => None,
    }
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

pub fn chrono_like_timestamp() -> String {
    unsafe {
        let mut tv: libc::timeval = std::mem::zeroed();
        libc::gettimeofday(&mut tv, std::ptr::null_mut());
        let mut tm: libc::tm = std::mem::zeroed();
        libc::gmtime_r(&tv.tv_sec, &mut tm);
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

#[derive(Clone, Debug)]
pub struct SharedState {
    pub timestamp: String,
    pub spm_pid: u32,
    pub guard: GuardSnapshot,
    pub children: Vec<ChildSnapshot>,
}

#[derive(Clone, Debug)]
pub struct GuardSnapshot {
    pub kill_threshold_percent: u8,
    pub consecutive_ticks_above: u8,
    pub total_kills: u32,
    pub enabled: bool,
}

#[derive(Clone, Debug)]
pub struct ChildSnapshot {
    pub index: usize,
    pub command: String,
    pub pid: Option<u32>,
    pub state: String,
    pub total_uss: u64,
    pub health_status: String,
    pub health_port: Option<u16>,
    pub restart_count: u32,
}

fn write_shared_state(children: &[supervisor::ManagedChild], guard: &guard::Guard) {
    let state = build_shared_state(children, guard);
    let json = shared_state_to_json(&state);

    let tmp_path = "/tmp/spm-state.json.tmp";
    let final_path = "/tmp/spm-state.json";

    if let Ok(mut file) = std::fs::File::create(tmp_path) {
        use std::io::Write;
        let _ = file.write_all(json.as_bytes());
        let _ = std::fs::rename(tmp_path, final_path);
    }
}

fn build_shared_state(children: &[supervisor::ManagedChild], guard: &guard::Guard) -> SharedState {
    SharedState {
        timestamp: chrono_like_timestamp(),
        spm_pid: std::process::id(),
        guard: GuardSnapshot {
            kill_threshold_percent: guard.config.kill_threshold_percent,
            consecutive_ticks_above: guard.consecutive_ticks_above,
            total_kills: guard.total_kills,
            enabled: guard.config.enabled,
        },
        children: children
            .iter()
            .map(|child| ChildSnapshot {
                index: child.index,
                command: child.command.clone(),
                pid: child.pid,
                state: format!("{:?}", child.state),
                total_uss: child.total_uss,
                health_status: format!("{:?}", child.health.status),
                health_port: child.health.port,
                restart_count: child.restart_count,
            })
            .collect(),
    }
}

fn shared_state_to_json(state: &SharedState) -> String {
    let children_json: Vec<String> = state
        .children
        .iter()
        .map(|child| {
            let pid_str = child.pid.map(|pid| pid.to_string()).unwrap_or("null".into());
            let port_str = child
                .health_port
                .map(|port| port.to_string())
                .unwrap_or("null".into());
            format!(
                r#"{{"index":{},"command":"{}","pid":{},"state":"{}","total_uss":{},"health_status":"{}","health_port":{},"restart_count":{}}}"#,
                child.index,
                escape_json(&child.command),
                pid_str,
                escape_json(&child.state),
                child.total_uss,
                escape_json(&child.health_status),
                port_str,
                child.restart_count
            )
        })
        .collect();

    format!(
        r#"{{"timestamp":"{}","spm_pid":{},"guard":{{"kill_threshold_percent":{},"consecutive_ticks_above":{},"total_kills":{},"enabled":{}}},"children":[{}]}}"#,
        state.timestamp,
        state.spm_pid,
        state.guard.kill_threshold_percent,
        state.guard.consecutive_ticks_above,
        state.guard.total_kills,
        state.guard.enabled,
        children_json.join(",")
    )
}

pub fn spawn_monitor_thread(
    managed: Arc<Mutex<Vec<supervisor::ManagedChild>>>,
    guard: Arc<Mutex<guard::Guard>>,
    policy: policy::ProtectionPolicy,
    headless: bool,
    tx: mpsc::Sender<MonitorEvent>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        unsafe {
            libc::signal(libc::SIGINT, signal_handler as libc::sighandler_t);
            libc::signal(libc::SIGTERM, signal_handler as libc::sighandler_t);
        }

        loop {
            if SHUTDOWN_REQUESTED.load(Ordering::SeqCst) {
                let max_restarts = guard
                    .lock()
                    .map(|guard| guard.config.max_restarts)
                    .unwrap_or(10);
                if let Ok(mut children) = managed.lock() {
                    shutdown_children(&mut children, max_restarts, &tx);
                }
                break;
            }

            thread::sleep(Duration::from_secs(1));

            let mut children = match managed.lock() {
                Ok(guard) => guard,
                Err(_) => continue,
            };
            for child in children.iter_mut() {
                if child.state == supervisor::ChildState::Running {
                    if let Some(pid) = child.pid {
                        child.total_uss = supervisor::compute_group_uss(pid);
                    }
                }
            }

            let max_restarts = guard
                .lock()
                .map(|guard| guard.config.max_restarts)
                .unwrap_or(10);
            reap_zombies(&mut children, max_restarts, true, &tx);

            // Health check tick (runs every iteration, but health.tick() handles 5s interval)
            for child in children.iter_mut() {
                if matches!(child.state, supervisor::ChildState::Running) {
                    let prev_status = child.health.status.clone();
                    child.health.tick(child.pid);

                    match (&prev_status, &child.health.status) {
                        (_, health::HealthStatus::Healthy)
                            if prev_status != health::HealthStatus::Healthy =>
                        {
                            if let (Some(port), Some(endpoint)) =
                                (child.health.port, child.health.endpoint.clone())
                            {
                                let _ = tx.send(MonitorEvent::HealthOk {
                                    index: child.index,
                                    port,
                                    endpoint,
                                });
                            }
                        }
                        (_, health::HealthStatus::Unhealthy) => {
                            if let (Some(pid), Some(pgid)) = (child.pid, child.pgid) {
                                let endpoint = child.health.endpoint.clone().unwrap_or_default();
                                let _ = tx.send(MonitorEvent::HealthKill {
                                    index: child.index,
                                    pid,
                                    cmd: child.command.clone(),
                                    endpoint,
                                });
                                let _ = process::kill_process_group(pgid, false);
                                child.state = supervisor::ChildState::Stopping { emergency: false };
                                child.last_exit = Some(supervisor::ExitInfo {
                                    exit_code: None,
                                    signal: Some(libc::SIGTERM),
                                    killed_by_guard: false,
                                    killed_by_health: true,
                                    exited_at: Instant::now(),
                                });
                            }
                        }
                        _ => {
                            if child.health.consecutive_failures > 0
                                && prev_status == health::HealthStatus::Healthy
                            {
                                if let Some(endpoint) = child.health.endpoint.clone() {
                                    let _ = tx.send(MonitorEvent::HealthFail {
                                        index: child.index,
                                        endpoint,
                                        consecutive: child.health.consecutive_failures,
                                    });
                                }
                            }
                        }
                    }
                }
            }

            let pod_memory = cgroup::read_pod_memory();
            let action = {
                let mut guard = match guard.lock() {
                    Ok(guard) => guard,
                    Err(_) => continue,
                };
                guard.evaluate(&pod_memory, &children, &policy)
            };

            match action {
                guard::GuardAction::Kill {
                    victim_index,
                    reason,
                    emergency,
                } => {
                    if let Some(child) = children.get_mut(victim_index) {
                        let pid = child.pid.unwrap_or(0);
                        let cmd = child.command.clone();
                        let uss = child.total_uss;
                        let pod_percent = pod_memory
                            .cgroup_limit
                            .map(|limit| (pod_memory.cgroup_usage as f64 / limit as f64) * 100.0)
                            .unwrap_or(0.0);

                        let _ = tx.send(MonitorEvent::GuardKill {
                            index: victim_index,
                            pid,
                            cmd: cmd.clone(),
                            uss,
                            pod_percent,
                            reason: reason.clone(),
                            emergency,
                        });

                        if let Some(pgid) = child.pgid {
                            let _ = process::kill_process_group(pgid, emergency);
                        }
                        child.state = supervisor::ChildState::Stopping { emergency };

                        if let Ok(mut guard) = guard.lock() {
                            guard.total_kills = guard.total_kills.saturating_add(1);
                            guard.consecutive_ticks_above = 0;
                            guard.last_kill = Some(guard::KillEvent {
                                victim_index,
                                reason,
                                at: Instant::now(),
                            });
                        }
                    }
                }
                guard::GuardAction::Warning {
                    percent,
                    ticks_remaining,
                } => {
                    let _ = tx.send(MonitorEvent::GuardWarning {
                        pod_percent: percent,
                        ticks_remaining,
                    });
                }
                guard::GuardAction::Exhausted { percent } => {
                    let _ = tx.send(MonitorEvent::GuardExhausted {
                        pod_percent: percent,
                    });
                    eprintln!(
                    "[spm] CRITICAL: guard exhausted; no eligible processes to kill (pod={:.2}%)",
                    percent
                );
                }
                guard::GuardAction::None => {}
            }

            for child in children.iter_mut() {
                if child.state == supervisor::ChildState::Restarting {
                    if let Some(restart_at) = child.backoff.restart_at {
                        if Instant::now() >= restart_at {
                            child.backoff.restart_at = None;
                            match supervisor::spawn_child(child, headless) {
                                Ok(spawned) => {
                                    child.health.reset();
                                    if headless {
                                        let name = supervisor::command_name(&child.command);
                                        if let Some(stdout) = spawned.stdout {
                                            supervisor::spawn_output_reader(
                                                name.clone(),
                                                stdout,
                                                false,
                                            );
                                        }
                                        if let Some(stderr) = spawned.stderr {
                                            supervisor::spawn_output_reader(name, stderr, true);
                                        }
                                    }
                                    let _ = tx.send(MonitorEvent::Restart {
                                        index: child.index,
                                        cmd: child.command.clone(),
                                        new_pid: spawned.pid,
                                        restart_count: child.restart_count,
                                        backoff_secs: child.backoff.current_delay.as_secs_f64(),
                                    });
                                }
                                Err(error) => {
                                    eprintln!(
                                        "[spm] Failed to restart '{}': {}",
                                        child.command, error
                                    );
                                    child.state = supervisor::ChildState::Failed;
                                    let _ = tx.send(MonitorEvent::Failed {
                                        index: child.index,
                                        cmd: child.command.clone(),
                                        restart_count: child.restart_count,
                                    });
                                }
                            }
                        }
                    }
                }

                if child.state == supervisor::ChildState::Running {
                    child.backoff.reset_if_stable();
                }
            }

            if let Ok(guard_lock) = guard.lock() {
                write_shared_state(&children, &guard_lock);
            }

            let _ = tx.send(MonitorEvent::StateUpdate);
        }
    })
}

fn shutdown_children(
    children: &mut Vec<supervisor::ManagedChild>,
    max_restarts: u32,
    tx: &mpsc::Sender<MonitorEvent>,
) {
    for child in children.iter() {
        if matches!(
            child.state,
            supervisor::ChildState::Running | supervisor::ChildState::Stopping { .. }
        ) {
            if let Some(pgid) = child.pgid {
                let _ = supervisor::signal_process_group(pgid, libc::SIGTERM);
            }
        }
    }

    for _ in 0..50 {
        thread::sleep(Duration::from_millis(100));
        reap_zombies(children, max_restarts, false, tx);
        let all_dead = children.iter().all(|child| {
            !matches!(
                child.state,
                supervisor::ChildState::Running | supervisor::ChildState::Stopping { .. }
            )
        });
        if all_dead {
            break;
        }
    }

    for child in children.iter_mut() {
        if matches!(
            child.state,
            supervisor::ChildState::Running | supervisor::ChildState::Stopping { .. }
        ) {
            if let Some(pgid) = child.pgid {
                let _ = supervisor::signal_process_group(pgid, libc::SIGKILL);
            }
            child.state = supervisor::ChildState::Stopped;
            child.pid = None;
            child.pgid = None;
        }
    }

    let _ = std::fs::remove_file("/tmp/spm-state.json");
}

fn reap_zombies(
    children: &mut Vec<supervisor::ManagedChild>,
    max_restarts: u32,
    allow_restart: bool,
    tx: &mpsc::Sender<MonitorEvent>,
) {
    loop {
        let mut status: i32 = 0;
        let result = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
        if result <= 0 {
            break;
        }

        let pid = result as u32;
        if let Some(child) = children.iter_mut().find(|child| child.pid == Some(pid)) {
            handle_child_exit(child, pid, status, max_restarts, allow_restart, tx);
        }
    }
}

fn handle_child_exit(
    child: &mut supervisor::ManagedChild,
    pid: u32,
    status: i32,
    max_restarts: u32,
    allow_restart: bool,
    tx: &mpsc::Sender<MonitorEvent>,
) {
    let (exit_code, signal) = if libc::WIFEXITED(status) {
        (Some(libc::WEXITSTATUS(status)), None)
    } else if libc::WIFSIGNALED(status) {
        (None, Some(libc::WTERMSIG(status)))
    } else {
        return;
    };

    child.last_exit = Some(supervisor::ExitInfo {
        exit_code,
        signal,
        killed_by_guard: matches!(child.state, supervisor::ChildState::Stopping { .. }),
        killed_by_health: false,
        exited_at: Instant::now(),
    });

    let _ = tx.send(MonitorEvent::Exit {
        index: child.index,
        pid,
        exit_code,
        signal,
    });

    if exit_code == Some(0) && !matches!(child.state, supervisor::ChildState::Stopping { .. }) {
        child.state = supervisor::ChildState::Completed;
        child.pid = None;
        child.pgid = None;
        child.backoff.restart_at = None;
        let _ = tx.send(MonitorEvent::Completed {
            index: child.index,
            cmd: child.command.clone(),
        });
        return;
    }

    child.state = supervisor::ChildState::Stopped;
    child.pid = None;
    child.pgid = None;
    child.restart_count = child.restart_count.saturating_add(1);

    if allow_restart
        && child
            .backoff
            .should_restart(max_restarts, child.restart_count)
    {
        let _ = child.backoff.schedule_restart();
        child.state = supervisor::ChildState::Restarting;
    } else {
        child.state = supervisor::ChildState::Failed;
        child.backoff.restart_at = None;
        let _ = tx.send(MonitorEvent::Failed {
            index: child.index,
            cmd: child.command.clone(),
            restart_count: child.restart_count,
        });
    }
}

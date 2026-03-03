use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub struct GuardConfig {
    pub kill_threshold_percent: u8,
    pub emergency_threshold_percent: u8,
    pub grace_ticks: u8,
    pub max_restarts: u32,
    pub enabled: bool,
    pub post_kill_cooldown: Duration,
}

#[derive(Clone, Debug)]
pub struct Guard {
    pub config: GuardConfig,
    pub consecutive_ticks_above: u8,
    pub total_kills: u32,
    pub last_kill: Option<KillEvent>,
    pub logged_unlimited: bool,
}

#[derive(Clone, Debug)]
pub struct KillEvent {
    pub victim_index: usize,
    pub reason: KillReason,
    pub at: Instant,
}

#[derive(Clone, Debug)]
pub enum GuardAction {
    None,
    Warning {
        percent: f64,
        ticks_remaining: u8,
    },
    Kill {
        victim_index: usize,
        reason: KillReason,
        emergency: bool,
    },
    Exhausted {
        percent: f64,
    },
}

#[derive(Clone, Debug)]
pub enum KillReason {
    ThresholdExceeded { pod_percent: f64 },
    HealthCheckFailed,
}

impl GuardConfig {
    pub fn default() -> Self {
        Self {
            kill_threshold_percent: 75,
            emergency_threshold_percent: 78,
            grace_ticks: 3,
            max_restarts: 10,
            enabled: true,
            post_kill_cooldown: Duration::from_secs(5),
        }
    }
}

impl Guard {
    pub fn new(config: GuardConfig) -> Self {
        Self {
            config,
            consecutive_ticks_above: 0,
            total_kills: 0,
            last_kill: None,
            logged_unlimited: false,
        }
    }

    pub fn evaluate(
        &mut self,
        pod_memory: &crate::app::PodMemorySnapshot,
        managed: &[crate::supervisor::ManagedChild],
        policy: &crate::policy::ProtectionPolicy,
    ) -> GuardAction {
        if !self.config.enabled {
            return GuardAction::None;
        }

        let limit = match pod_memory.cgroup_limit {
            Some(limit) if limit > 0 => limit,
            Some(_) => return GuardAction::None,
            None => {
                if !self.logged_unlimited {
                    eprintln!("[spm] Guard disabled: cgroup memory limit is unlimited");
                    self.logged_unlimited = true;
                }
                return GuardAction::None;
            }
        };

        let percent = (pod_memory.cgroup_usage as f64 / limit as f64) * 100.0;
        if percent < self.config.kill_threshold_percent as f64 {
            self.consecutive_ticks_above = 0;
            return GuardAction::None;
        }

        if let Some(ref last_kill) = self.last_kill {
            if last_kill.at.elapsed() < self.config.post_kill_cooldown {
                return GuardAction::Warning {
                    percent,
                    ticks_remaining: 0,
                };
            }
        }

        self.consecutive_ticks_above = self.consecutive_ticks_above.saturating_add(1);
        if self.consecutive_ticks_above < self.config.grace_ticks {
            return GuardAction::Warning {
                percent,
                ticks_remaining: self.config.grace_ticks - self.consecutive_ticks_above,
            };
        }

        let emergency = percent >= self.config.emergency_threshold_percent as f64;
        match policy.select_victim(managed) {
            Some(victim_index) => GuardAction::Kill {
                victim_index,
                reason: KillReason::ThresholdExceeded {
                    pod_percent: percent,
                },
                emergency,
            },
            None => GuardAction::Exhausted { percent },
        }
    }
}

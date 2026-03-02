#[derive(Clone, Debug)]
pub struct ProtectionPolicy {
    pub self_pid: u32,
}

impl ProtectionPolicy {
    pub fn new() -> Self {
        Self {
            self_pid: std::process::id(),
        }
    }

    pub fn is_protected(&self, pid: u32) -> bool {
        pid == 1 || pid == self.self_pid
    }

    pub fn select_victim(&self, managed: &[crate::supervisor::ManagedChild]) -> Option<usize> {
        managed
            .iter()
            .enumerate()
            .filter(|(_, child)| child.state == crate::supervisor::ChildState::Running)
            .filter(|(_, child)| {
                if let Some(pid) = child.pid {
                    !self.is_protected(pid)
                } else {
                    false
                }
            })
            .max_by(|(_, left), (_, right)| {
                left.total_uss
                    .cmp(&right.total_uss)
                    .then_with(|| left.pid.unwrap_or(0).cmp(&right.pid.unwrap_or(0)))
                    .then_with(|| left.index.cmp(&right.index))
            })
            .map(|(index, _)| index)
    }
}

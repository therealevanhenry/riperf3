use std::time::Instant;

use nix::sys::resource::{getrusage, UsageWho};

/// A snapshot of process resource usage for computing CPU utilization.
#[derive(Clone)]
pub struct CpuSnapshot {
    wall_time: Instant,
    user_usec: i64,
    system_usec: i64,
}

impl CpuSnapshot {
    /// Take a snapshot of current wall time and process CPU usage.
    pub fn now() -> Self {
        match getrusage(UsageWho::RUSAGE_SELF) {
            Ok(usage) => {
                let user = usage.user_time();
                let system = usage.system_time();
                Self {
                    wall_time: Instant::now(),
                    user_usec: user.tv_sec() * 1_000_000 + user.tv_usec(),
                    system_usec: system.tv_sec() * 1_000_000 + system.tv_usec(),
                }
            }
            Err(_) => Self {
                wall_time: Instant::now(),
                user_usec: 0,
                system_usec: 0,
            },
        }
    }

    /// Compute CPU utilization between this snapshot and an earlier one.
    pub fn utilization_since(&self, earlier: &CpuSnapshot) -> CpuUtilization {
        let wall_usec = self
            .wall_time
            .duration_since(earlier.wall_time)
            .as_micros() as f64;

        if wall_usec == 0.0 {
            return CpuUtilization::default();
        }

        let user_diff = (self.user_usec - earlier.user_usec) as f64;
        let system_diff = (self.system_usec - earlier.system_usec) as f64;

        CpuUtilization {
            host_total: (user_diff + system_diff) / wall_usec * 100.0,
            host_user: user_diff / wall_usec * 100.0,
            host_system: system_diff / wall_usec * 100.0,
            remote_total: 0.0,
            remote_user: 0.0,
            remote_system: 0.0,
        }
    }
}

/// CPU utilization percentages for both local and remote hosts.
/// Remote values are filled in from the peer's results JSON after exchange.
#[derive(Debug, Clone, Default)]
pub struct CpuUtilization {
    pub host_total: f64,
    pub host_user: f64,
    pub host_system: f64,
    pub remote_total: f64,
    pub remote_user: f64,
    pub remote_system: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_returns_non_negative() {
        let snap = CpuSnapshot::now();
        assert!(snap.user_usec >= 0);
        assert!(snap.system_usec >= 0);
    }

    #[test]
    fn utilization_over_interval() {
        let before = CpuSnapshot::now();
        let mut x: u64 = 0;
        for i in 0..1_000_000u64 {
            x = x.wrapping_add(i);
        }
        std::hint::black_box(x);
        let after = CpuSnapshot::now();

        let util = after.utilization_since(&before);
        assert!(util.host_user >= 0.0);
        assert!(util.host_total >= 0.0);
        assert_eq!(util.remote_total, 0.0);
    }

    #[test]
    fn utilization_zero_interval() {
        let snap = CpuSnapshot::now();
        let util = snap.utilization_since(&snap);
        assert_eq!(util.host_total, 0.0);
    }
}

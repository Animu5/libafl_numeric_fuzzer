use libafl::monitors::{stats::manager::ClientStatsManager, Monitor};
use libafl::Error;
use libafl_core::ClientId;
use std::{
    fmt,
    time::{Duration, Instant},
};

/// A headless monitor that writes periodic fuzzer statistics into the log file
/// via `tracing`, for runs without a terminal.
#[derive(Debug)]
pub struct LoggingMonitor {
    interval: Duration,
    last: Option<Instant>,
}

impl LoggingMonitor {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last: None,
        }
    }
}

impl Monitor for LoggingMonitor {
    fn display(
        &mut self,
        stats: &mut ClientStatsManager,
        name: &str,
        client_id: ClientId,
    ) -> Result<(), Error> {
        let now = Instant::now();
        if self
            .last
            .is_some_and(|last| now.duration_since(last) < self.interval)
        {
            return Ok(());
        }
        self.last = Some(now);
        let Ok(client) = stats.get(client_id) else {
            return Ok(());
        };
        let elapsed = client.start_time();
        let execs = client.executions();
        let execs_per_sec = if elapsed.is_zero() {
            0.0
        } else {
            execs as f64 / elapsed.as_secs_f64()
        };
        tracing::info!(
            "[{name}] execs={execs} ({execs_per_sec:.0}/s), corpus={}, objectives={}, \
             last new corpus {:?} ago, last objective {:?} ago",
            client.corpus_size(),
            client.objective_size(),
            client.last_corpus_time(),
            client.last_objective_time(),
        );
        Ok(())
    }
}

/// Boxes any monitor behind one type so the CLI can select it at runtime.
pub struct DynMonitor(pub Box<dyn Monitor>);

impl fmt::Debug for DynMonitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DynMonitor")
    }
}

impl Monitor for DynMonitor {
    fn display(
        &mut self,
        stats: &mut ClientStatsManager,
        name: &str,
        client_id: ClientId,
    ) -> Result<(), Error> {
        self.0.display(stats, name, client_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logging_monitor_tolerates_missing_clients_and_throttles() {
        let mut stats = ClientStatsManager::new();
        let mut monitor = LoggingMonitor::new(Duration::from_secs(60));
        let id = ClientId::default();
        // No client registered yet: nothing to report, but not an error.
        monitor.display(&mut stats, "test", id).unwrap();
        // Immediately again: throttled, still fine.
        monitor.display(&mut stats, "test", id).unwrap();
    }
}

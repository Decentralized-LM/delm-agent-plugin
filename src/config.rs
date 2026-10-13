pub const MAX_REPO_SIZE_BYTES: u64 = 10_000_000_000;
pub const DEFAULT_RUN_SECONDS: u64 = 30 * 60;
pub const PREPARATION_SECONDS: u64 = 60;

/// Codex and an unselected Claude run use two workers.
pub const DEFAULT_WORKER_COUNT: usize = 2;
/// Upper bound for workspaces, board identities and recovery bundles. Hosts
/// choose which counts they offer within it.
pub const MAX_WORKER_COUNT: usize = 4;
/// Counts a Claude Code user can choose with the picker or `--agents`.
pub const CLAUDE_WORKER_COUNTS: [usize; 2] = [2, 4];

/// Validate a runtime worker count before any workspace is prepared.
pub fn validate_worker_count(count: usize) -> anyhow::Result<usize> {
    anyhow::ensure!(
        (DEFAULT_WORKER_COUNT..=MAX_WORKER_COUNT).contains(&count),
        "DeLM supports {DEFAULT_WORKER_COUNT} to {MAX_WORKER_COUNT} workers, not {count}"
    );
    Ok(count)
}

/// Validate a count requested for a Claude Code run.
pub fn validate_claude_worker_count(count: usize) -> anyhow::Result<usize> {
    anyhow::ensure!(
        CLAUDE_WORKER_COUNTS.contains(&count),
        "Choose 2 or 4 agents for a Claude Code run, not {count}"
    );
    validate_worker_count(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worker_counts_stay_within_the_supported_bounds() {
        assert!(validate_worker_count(1).is_err());
        assert_eq!(validate_worker_count(2).unwrap(), 2);
        assert_eq!(validate_worker_count(4).unwrap(), 4);
        assert!(validate_worker_count(5).is_err());
        assert!(validate_claude_worker_count(3).is_err());
        assert_eq!(validate_claude_worker_count(4).unwrap(), 4);
    }
}

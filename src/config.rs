pub const MAX_REPO_SIZE_BYTES: u64 = 10_000_000_000;
pub const DEFAULT_RUN_SECONDS: u64 = 30 * 60;
pub const PREPARATION_SECONDS: u64 = 60;

pub const DEFAULT_WORKER_COUNT: usize = 2;
pub const MAX_WORKER_COUNT: usize = 4;

pub fn default_worker_count() -> usize {
    DEFAULT_WORKER_COUNT
}

pub fn validate_worker_count(count: usize) -> anyhow::Result<()> {
    anyhow::ensure!(
        (DEFAULT_WORKER_COUNT..=MAX_WORKER_COUNT).contains(&count),
        "Choose 2, 3, or 4 agents"
    );
    Ok(())
}

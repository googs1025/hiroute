use std::env;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use hiroute_gateway::replay::{ReplayConfig, ReplayManager};
use hiroute_gateway_core::runtime::body::{BudgetTree, MemoryRole};

const MIN_ROUNDS: usize = 5;
const RECORD_BYTES: usize = 16 * 1024;

fn main() -> Result<(), Box<dyn Error>> {
    let rounds = env_usize("HIROUTE_GATEWAY_REPLAY_BENCH_ROUNDS", MIN_ROUNDS).max(MIN_ROUNDS);
    let payload_bytes =
        env_usize("HIROUTE_GATEWAY_REPLAY_BENCH_BYTES", 4 * 1024 * 1024).max(RECORD_BYTES);
    println!("resource\tvariant\tround\tpayload_bytes\telapsed_ns\tns_per_operation\tdetail");
    println!("release\ttrue");
    println!("rounds\t{rounds}");

    for (variant, memory_threshold_bytes) in [
        (
            "memory-retained",
            payload_bytes.saturating_add(RECORD_BYTES),
        ),
        ("disk-spill", RECORD_BYTES),
    ] {
        for round in 1..=rounds {
            let result =
                measure_replay_path(variant, round, payload_bytes, memory_threshold_bytes)?;
            println!(
                "gateway_replay\t{}\t{}\t{}\t{}\t{}\tmemory_threshold_bytes={};copy_bytes={};scan_bytes={};peak_retained_bytes={};peak_spill_bytes={};disk_backed={}",
                variant,
                round,
                payload_bytes,
                result.elapsed.as_nanos(),
                result.elapsed.as_nanos(),
                memory_threshold_bytes,
                result.copy_bytes,
                result.scan_bytes,
                result.peak_retained_bytes,
                result.peak_spill_bytes,
                result.disk_backed,
            );
        }
    }
    Ok(())
}

struct ReplayPathMeasurement {
    elapsed: Duration,
    copy_bytes: usize,
    scan_bytes: usize,
    peak_retained_bytes: usize,
    peak_spill_bytes: u64,
    disk_backed: bool,
}

fn measure_replay_path(
    variant: &str,
    round: usize,
    payload_bytes: usize,
    memory_threshold_bytes: usize,
) -> Result<ReplayPathMeasurement, Box<dyn Error>> {
    let root = replay_root(variant, round)?;
    let payload = deterministic_payload(payload_bytes);
    let budget_limit = payload_bytes
        .checked_mul(4)
        .and_then(|bytes| bytes.checked_add(2 * 1024 * 1024))
        .ok_or("gateway replay benchmark budget overflow")?;

    let measurement = {
        let manager = ReplayManager::open(ReplayConfig {
            root: root.clone(),
            memory_threshold_bytes,
            record_bytes: RECORD_BYTES,
            orphan_ttl: Duration::from_secs(60),
        })?;
        let tree = BudgetTree::new(budget_limit, budget_limit)?;
        let budget = tree.stream(budget_limit)?;
        let store = manager.begin_request(budget.clone())?;
        let started = Instant::now();
        let mut writer = store.begin_raw()?;
        writer.append(&payload)?;
        let reference = writer.seal()?;
        let after_write = store.snapshot();
        let peak_spill_bytes = allocated_file_bytes(manager.root())?;

        let mut reader = store.reader(&reference)?;
        let mut scan_bytes = 0_usize;
        while let Some(chunk) = reader.next_charged(&budget, MemoryRole::Retry, RECORD_BYTES)? {
            scan_bytes = scan_bytes
                .checked_add(chunk.bytes().len())
                .ok_or("gateway replay benchmark scan overflow")?;
        }
        if scan_bytes != payload.len() {
            return Err("gateway replay benchmark reader did not replay the exact payload".into());
        }
        let peak_retained_bytes = budget.snapshot()?.peak;
        let disk_backed = after_write.disk_backed;
        if disk_backed != (variant == "disk-spill") {
            return Err("gateway replay benchmark selected an unexpected backing mode".into());
        }
        if disk_backed && peak_spill_bytes == 0 {
            return Err("gateway replay benchmark spill mode created no disk backing bytes".into());
        }
        if !disk_backed && peak_spill_bytes != 0 {
            return Err("gateway replay benchmark memory mode created backing files".into());
        }
        store.release_stream(&reference)?;
        store.mark_terminal();
        drop(reader);
        drop(store);
        if tree.snapshot().process_live != 0 {
            return Err(
                "gateway replay benchmark retained charged bytes after terminal cleanup".into(),
            );
        }
        ReplayPathMeasurement {
            elapsed: started.elapsed(),
            copy_bytes: payload.len(),
            scan_bytes,
            peak_retained_bytes,
            peak_spill_bytes,
            disk_backed,
        }
    };

    // The benchmark created this exact fresh root and only removes it after
    // ReplayStore dropped its owner-only request directory and all handles.
    fs::remove_dir(&root)?;
    Ok(measurement)
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn replay_root(variant: &str, round: usize) -> Result<std::path::PathBuf, Box<dyn Error>> {
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let root = env::temp_dir().join(format!(
        "hiroute-gateway-replay-bench-{}-{}-{}-{}",
        std::process::id(),
        variant,
        round,
        nonce,
    ));
    if root.exists() {
        return Err("gateway replay benchmark root unexpectedly already exists".into());
    }
    Ok(root)
}

fn deterministic_payload(bytes: usize) -> Vec<u8> {
    (0..bytes).map(|index| (index % 251) as u8).collect()
}

fn allocated_file_bytes(path: &Path) -> Result<u64, Box<dyn Error>> {
    let mut total = 0_u64;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            total = total
                .checked_add(allocated_file_bytes(&entry.path())?)
                .ok_or("gateway replay benchmark spill byte overflow")?;
        } else if metadata.is_file() {
            total = total
                .checked_add(metadata.len())
                .ok_or("gateway replay benchmark spill byte overflow")?;
        } else {
            return Err("gateway replay benchmark found a non-regular backing entry".into());
        }
    }
    Ok(total)
}

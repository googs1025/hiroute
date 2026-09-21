use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::accounts::AccountSnapshotRecord;
use crate::config::{private_atomic_write, validate_private_file};
use crate::errors::CpaLifecycleError;

const ACCOUNT_STATE_SCHEMA: &str = "hiroute.cpa-account-state/v1";
const MAX_ACCOUNT_STATE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AccountStateEnvelope {
    schema: String,
    accounts: Vec<AccountSnapshotRecord>,
}

pub(crate) fn load_account_state(
    path: &Path,
) -> Result<Vec<AccountSnapshotRecord>, CpaLifecycleError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    validate_private_file(path)?;
    let file = fs::File::open(path).map_err(CpaLifecycleError::AccountStateIo)?;
    let mut bytes = Vec::new();
    file.take(MAX_ACCOUNT_STATE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(CpaLifecycleError::AccountStateIo)?;
    if bytes.len() as u64 > MAX_ACCOUNT_STATE_BYTES {
        return Err(CpaLifecycleError::InvalidAccountState);
    }
    let envelope: AccountStateEnvelope =
        serde_json::from_slice(&bytes).map_err(|_| CpaLifecycleError::InvalidAccountState)?;
    if envelope.schema != ACCOUNT_STATE_SCHEMA {
        return Err(CpaLifecycleError::InvalidAccountState);
    }
    validate_accounts(&envelope.accounts)?;
    Ok(envelope.accounts)
}

pub(crate) fn save_account_state(
    path: &Path,
    accounts: &[AccountSnapshotRecord],
) -> Result<(), CpaLifecycleError> {
    validate_accounts(accounts)?;
    let bytes = serde_json::to_vec(&AccountStateEnvelope {
        schema: ACCOUNT_STATE_SCHEMA.to_owned(),
        accounts: accounts.to_vec(),
    })
    .map_err(|_| CpaLifecycleError::InvalidAccountState)?;
    private_atomic_write(path, &bytes)?;
    Ok(())
}

pub(crate) fn merge_accounts(
    old: &[AccountSnapshotRecord],
    fresh: Vec<AccountSnapshotRecord>,
) -> Result<Vec<AccountSnapshotRecord>, CpaLifecycleError> {
    validate_accounts(old)?;
    validate_accounts(&fresh)?;
    let old = old
        .iter()
        .map(|account| (account.account_digest.as_str(), account))
        .collect::<BTreeMap<_, _>>();
    let fresh_digests = fresh
        .iter()
        .map(|account| account.account_digest.clone())
        .collect::<BTreeSet<_>>();
    let mut merged = Vec::with_capacity(old.len().max(fresh.len()));
    for mut account in fresh {
        if let Some(previous) = old.get(account.account_digest.as_str()) {
            account.generation = if previous.active || !account.active {
                previous.generation.max(account.generation)
            } else {
                previous
                    .generation
                    .checked_add(1)
                    .ok_or(CpaLifecycleError::InvalidAccountState)?
                    .max(account.generation)
            };
        }
        merged.push(account);
    }
    for previous in old.values() {
        if !fresh_digests.contains(previous.account_digest.as_str()) {
            let mut inactive = (*previous).clone();
            inactive.active = false;
            merged.push(inactive);
        }
    }
    merged.sort_by(|left, right| left.account_digest.cmp(&right.account_digest));
    validate_accounts(&merged)?;
    Ok(merged)
}

fn validate_accounts(accounts: &[AccountSnapshotRecord]) -> Result<(), CpaLifecycleError> {
    if accounts.len() > 10_000 {
        return Err(CpaLifecycleError::InvalidAccountState);
    }
    let mut digests = BTreeSet::new();
    for account in accounts {
        if account.validate_persisted().is_err() || !digests.insert(account.account_digest.as_str())
        {
            return Err(CpaLifecycleError::InvalidAccountState);
        }
    }
    Ok(())
}

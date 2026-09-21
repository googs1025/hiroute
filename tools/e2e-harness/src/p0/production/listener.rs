use std::collections::BTreeSet;
use std::net::SocketAddr;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

use super::types::ProductionError;

#[cfg(target_os = "macos")]
pub(super) fn owner(
    child_pid: u32,
    address: SocketAddr,
) -> Result<(u32, &'static str), ProductionError> {
    let pid = child_pid.to_string();
    let output = std::process::Command::new("/usr/sbin/lsof")
        .args(["-nP", "-a", "-p", &pid])
        .arg(format!("-iTCP@{address}"))
        .args(["-sTCP:LISTEN", "-FpnT"])
        .output()
        .map_err(|error| {
            ProductionError::Readiness(format!(
                "cannot inspect listener ownership with lsof: {error}"
            ))
        })?;
    let stdout = String::from_utf8(output.stdout).map_err(|_| {
        ProductionError::Readiness("lsof listener ownership output is not UTF-8".into())
    })?;
    let pids = stdout
        .lines()
        .filter_map(|line| line.strip_prefix('p'))
        .filter_map(|value| value.parse::<u32>().ok())
        .collect::<BTreeSet<_>>();
    let address = address.to_string();
    let has_address = stdout
        .lines()
        .any(|line| line.strip_prefix('n') == Some(address.as_str()));
    let is_listener = stdout.lines().any(|line| line == "TST=LISTEN");
    if !output.status.success()
        || pids != BTreeSet::from([child_pid])
        || !has_address
        || !is_listener
    {
        return Err(not_owned());
    }
    Ok((child_pid, "darwin_lsof_tcp_listener/v1"))
}

#[cfg(target_os = "linux")]
pub(super) fn owner(
    child_pid: u32,
    address: SocketAddr,
) -> Result<(u32, &'static str), ProductionError> {
    let std::net::IpAddr::V4(ip) = address.ip() else {
        return Err(ProductionError::Readiness(
            "the production Oracle requires an IPv4 loopback listener".into(),
        ));
    };
    let fd_root = PathBuf::from(format!("/proc/{child_pid}/fd"));
    let owned_sockets = std::fs::read_dir(&fd_root)
        .map_err(|error| {
            ProductionError::Readiness(format!("cannot inspect child socket handles: {error}"))
        })?
        .filter_map(Result::ok)
        .filter_map(|entry| std::fs::read_link(entry.path()).ok())
        .filter_map(|target| target.to_str().map(str::to_owned))
        .filter(|target| target.starts_with("socket:[") && target.ends_with(']'))
        .collect::<BTreeSet<_>>();
    let expected_address = format!(
        "{:08X}:{:04X}",
        u32::from_le_bytes(ip.octets()),
        address.port()
    );
    let tcp = std::fs::read_to_string(format!("/proc/{child_pid}/net/tcp")).map_err(|error| {
        ProductionError::Readiness(format!("cannot inspect child TCP listeners: {error}"))
    })?;
    let owns_listener = tcp.lines().skip(1).any(|line| {
        let columns = line.split_ascii_whitespace().collect::<Vec<_>>();
        columns.len() > 9
            && columns[1] == expected_address
            && columns[3] == "0A"
            && owned_sockets.contains(&format!("socket:[{}]", columns[9]))
    });
    if !owns_listener {
        return Err(not_owned());
    }
    Ok((child_pid, "linux_procfs_tcp_listener/v1"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(super) fn owner(
    _child_pid: u32,
    _address: SocketAddr,
) -> Result<(u32, &'static str), ProductionError> {
    Err(ProductionError::Readiness(
        "no fail-closed listener ownership verifier exists for this platform".into(),
    ))
}

fn not_owned() -> ProductionError {
    ProductionError::Readiness("the launched child does not own the exact listening socket".into())
}

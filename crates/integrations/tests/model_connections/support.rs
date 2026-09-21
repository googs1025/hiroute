use std::{
    collections::BTreeMap,
    io::{Read, Write},
    net::TcpListener,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

use hiroute_application::compute_management::{
    ComputeCandidateFactsV2, ComputeCandidatePort, ComputeCredentialBindingV2,
};
use hiroute_application_api::{
    ComputeCandidateInputStateV2, ComputeCandidateIssueV2, ComputeCandidateRefV2,
    ComputeCandidateViewV2,
};
use hiroute_domain::{PortError, PortErrorCode, PortResult};
use hiroute_integrations::{
    ModelConnectionProbeCredentialV1, ModelDirectoryHttpResponseV1, ModelDirectoryTransportErrorV1,
    ModelDirectoryTransportV1, NormalizedModelConnectionTargetV1,
};

pub type ControlledResponse = (u16, Vec<(String, String)>, String);

#[derive(Clone)]
struct TestCandidate {
    facts: ComputeCandidateFactsV2,
    view: ComputeCandidateViewV2,
}

/// Test-target-only stand-in for the coordinator-owned MVP-11 candidate registry.
///
/// This fake exists only to observe producer output through `ComputeCandidatePort`. It is not
/// exported by any library and is not proof of production registry behavior. The MVP-11 central
/// regression handoff remains responsible for stale/same-revision and trusted-resolution cases.
#[derive(Default)]
pub(super) struct TestOnlyComputeCandidatePort {
    entries: Mutex<BTreeMap<String, TestCandidate>>,
}

impl TestOnlyComputeCandidatePort {
    pub(super) fn new() -> Self {
        Self::default()
    }
}

impl ComputeCandidatePort for TestOnlyComputeCandidatePort {
    fn register_compute_candidate(
        &self,
        facts: ComputeCandidateFactsV2,
    ) -> PortResult<ComputeCandidateViewV2> {
        facts.validate_shape()?;
        let candidate_ref = facts.candidate.candidate_ref.clone();
        let mut entries = self.entries.lock().map_err(|_| test_port_unavailable())?;
        if let Some(current) = entries.get(&candidate_ref) {
            if facts.candidate.candidate_revision < current.facts.candidate.candidate_revision {
                return Err(test_port_conflict("test-candidate-stale"));
            }
            if facts.candidate.candidate_revision == current.facts.candidate.candidate_revision {
                return if facts == current.facts {
                    Ok(current.view.clone())
                } else {
                    Err(test_port_conflict("test-candidate-revision-reused"))
                };
            }
        }
        let input_state = match &facts.credential_binding {
            ComputeCredentialBindingV2::None
            | ComputeCredentialBindingV2::CpaPendingApproval { .. }
            | ComputeCredentialBindingV2::CpaOwned { .. } => {
                ComputeCandidateInputStateV2::NotRequired
            }
            ComputeCredentialBindingV2::NativePendingInput => ComputeCandidateInputStateV2::Missing,
            ComputeCredentialBindingV2::NativeProtected { .. }
            | ComputeCredentialBindingV2::NativeSaved { .. } => {
                ComputeCandidateInputStateV2::Provided
            }
        };
        let issues = match &facts.credential_binding {
            ComputeCredentialBindingV2::NativePendingInput => vec![ComputeCandidateIssueV2 {
                code: "NEEDS_CREDENTIAL".into(),
                message_key: "model_connections.needs_credential".into(),
                retryable: true,
            }],
            ComputeCredentialBindingV2::CpaPendingApproval { .. } => {
                vec![ComputeCandidateIssueV2 {
                    code: "NEEDS_APPROVAL".into(),
                    message_key: "model_connections.needs_subscription_approval".into(),
                    retryable: true,
                }]
            }
            _ => Vec::new(),
        };
        let view = facts.public_view(input_state, issues)?;
        entries.insert(
            candidate_ref,
            TestCandidate {
                facts,
                view: view.clone(),
            },
        );
        Ok(view)
    }

    fn get_compute_candidate(
        &self,
        candidate: &ComputeCandidateRefV2,
    ) -> PortResult<ComputeCandidateViewV2> {
        candidate
            .validate_shape()
            .map_err(|_| test_port_conflict("test-candidate-query"))?;
        let entries = self.entries.lock().map_err(|_| test_port_unavailable())?;
        let current = entries
            .get(&candidate.candidate_ref)
            .ok_or_else(|| PortError::new(PortErrorCode::NotFound, "test-candidate-missing"))?;
        if current.facts.candidate.candidate_revision != candidate.candidate_revision {
            return Err(test_port_conflict("test-candidate-revision-changed"));
        }
        Ok(current.view.clone())
    }

    fn resolve_compute_candidate(
        &self,
        candidate: &ComputeCandidateRefV2,
    ) -> PortResult<ComputeCandidateFactsV2> {
        candidate
            .validate_shape()
            .map_err(|_| test_port_conflict("test-candidate-query"))?;
        let entries = self.entries.lock().map_err(|_| test_port_unavailable())?;
        let current = entries
            .get(&candidate.candidate_ref)
            .ok_or_else(|| PortError::new(PortErrorCode::NotFound, "test-candidate-missing"))?;
        if current.facts.candidate.candidate_revision != candidate.candidate_revision {
            return Err(test_port_conflict("test-candidate-revision-changed"));
        }
        Ok(current.facts.clone())
    }
}

fn test_port_conflict(context: &'static str) -> PortError {
    PortError::new(PortErrorCode::Conflict, context)
}

fn test_port_unavailable() -> PortError {
    PortError::new(PortErrorCode::Unavailable, "test-candidate-port-lock")
}

pub struct ControlledServer {
    pub base_url: String,
    requests: mpsc::Receiver<String>,
    worker: thread::JoinHandle<()>,
}

impl ControlledServer {
    pub fn start(responses: Vec<ControlledResponse>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (tx, requests) = mpsc::channel();
        let worker = thread::spawn(move || {
            for (status, headers, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|value| value == b"\r\n\r\n") {
                    let count = stream.read(&mut chunk).unwrap();
                    if count == 0 {
                        break;
                    }
                    request.extend_from_slice(&chunk[..count]);
                    assert!(request.len() < 32 * 1024);
                }
                let header_end = request
                    .windows(4)
                    .position(|value| value == b"\r\n\r\n")
                    .unwrap()
                    + 4;
                let body_len = std::str::from_utf8(&request[..header_end])
                    .unwrap()
                    .lines()
                    .filter_map(|line| line.split_once(':'))
                    .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                    .map_or(0, |(_, value)| value.trim().parse::<usize>().unwrap());
                assert!(body_len < 32 * 1024);
                while request.len() < header_end + body_len {
                    let count = stream.read(&mut chunk).unwrap();
                    assert_ne!(count, 0, "incomplete request body");
                    request.extend_from_slice(&chunk[..count]);
                }
                tx.send(String::from_utf8(request).unwrap()).unwrap();
                let reason = match status {
                    200 => "OK",
                    301 => "Moved Permanently",
                    401 => "Unauthorized",
                    403 => "Forbidden",
                    404 => "Not Found",
                    429 => "Too Many Requests",
                    _ => "Result",
                };
                let mut response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
                    body.len()
                );
                for (name, value) in headers {
                    response.push_str(&name);
                    response.push_str(": ");
                    response.push_str(&value);
                    response.push_str("\r\n");
                }
                response.push_str("\r\n");
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            }
        });
        Self {
            base_url: format!("http://{address}/v1"),
            requests,
            worker,
        }
    }

    pub fn finish(self) -> Vec<String> {
        self.worker.join().unwrap();
        self.requests.try_iter().collect()
    }
}

#[derive(Clone, Default)]
pub struct CountingTransport(Arc<AtomicUsize>);

impl CountingTransport {
    pub fn calls(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.0)
    }
}

impl ModelDirectoryTransportV1 for CountingTransport {
    fn get(
        &self,
        _target: &NormalizedModelConnectionTargetV1,
        _query: Option<&str>,
        _credential: Option<ModelConnectionProbeCredentialV1<'_>>,
        _timeout: Duration,
        _response_limit: usize,
    ) -> Result<ModelDirectoryHttpResponseV1, ModelDirectoryTransportErrorV1> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(ModelDirectoryHttpResponseV1 {
            status: 200,
            body: br#"{"data":[]}"#.to_vec(),
            truncated: false,
        })
    }
}

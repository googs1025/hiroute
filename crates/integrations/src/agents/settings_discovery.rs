//! Targeted native facts for settings writes; unrelated Agent diagnostics are not a save gate.
use super::*;

impl FilesystemAgentScannerV1 {
    pub fn codex_settings_discovery(&self) -> FilesystemAgentDiscoveryV1 {
        let mut discovery = self.scan_codex();
        if let AgentDiscoveryOutcomeV1::Supported { installation } = &mut discovery.outcome {
            super::super::observed_capabilities::attach_target_file_capabilities(
                installation,
                &self.layout.codex_user_config,
            );
            #[cfg(unix)]
            if let Ok(cache) = self.codex_ingress.lock()
                && let Some(evidence) = cache.as_ref()
            {
                evidence.attach(installation);
            }
        }
        discovery
    }

    /// Settings writes need the current configuration and a launchable path, not a fresh
    /// `claude --version` subprocess. The ordinary scan still reports diagnostic probe results;
    /// this path retains file, precedence, path and authentication checks.
    pub fn claude_settings_discovery(&self) -> FilesystemAgentDiscoveryV1 {
        match super::super::executable::resolve(&self.layout.claude_executable) {
            Ok(Some(path)) => {
                let Some(canonical_path) = path.to_str().map(str::to_owned) else {
                    return probe_report(
                        "agent_claude_default",
                        AgentKindV1::ClaudeCode,
                        ExecutableProbe::Unknown(
                            super::super::executable::ProbeFailure::Unavailable,
                        ),
                    );
                };
                let mut discovery = self.scan_claude(
                    Some(ExecutableObservationV1 {
                        version: "not-probed".into(),
                        canonical_path,
                    }),
                    None,
                    true,
                );
                if let AgentDiscoveryOutcomeV1::Supported { installation } = &mut discovery.outcome
                {
                    super::super::observed_capabilities::attach_target_file_capabilities(
                        installation,
                        &self.layout.claude_user_settings,
                    );
                    #[cfg(unix)]
                    if let Ok(cache) = self.claude_ingress.lock()
                        && let Some(evidence) = cache.as_ref()
                    {
                        evidence.attach(&path, installation);
                    }
                }
                discovery
            }
            Ok(None) => probe_report(
                "agent_claude_default",
                AgentKindV1::ClaudeCode,
                ExecutableProbe::NotFound,
            ),
            Err(reason) => probe_report(
                "agent_claude_default",
                AgentKindV1::ClaudeCode,
                ExecutableProbe::Unknown(reason),
            ),
        }
    }
}

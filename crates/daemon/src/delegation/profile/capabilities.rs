use super::*;

pub(super) fn identity_contract(
    input: &ProfileInput<'_>,
) -> Result<AcpNativeIdentityContract, DelegationErrorV1> {
    // Codex's advertised read-only ACP mode can still select workspaceWrite internally.
    if input.harness == WorkerHarnessV1::CodexCli
        && input.permission_policy != WorkerPermissionPolicyV1::ApproveAll
    {
        return Err(DelegationErrorV1::CapabilityUnavailable);
    }
    Ok(match input.harness {
        WorkerHarnessV1::CodexCli => AcpNativeIdentityContract::CodexThreadV1,
        WorkerHarnessV1::ClaudeCode => AcpNativeIdentityContract::ClaudeSessionV1,
    })
}

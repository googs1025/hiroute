use std::sync::Arc;

use hiroute_domain::WorkspaceId;
use hiroute_domain::delegation::DelegationErrorV1;
use tokio::runtime::Builder;

use super::DelegationRunExecutor;

impl DelegationRunExecutor {
    /// Wake the existing durable cancellation dispatcher after intent has committed. Replays may
    /// create another bounded wake, but never another prompt or cancellation receipt.
    pub(crate) fn wake_cancellation(
        self: &Arc<Self>,
        workspace: &WorkspaceId,
    ) -> Result<(), DelegationErrorV1> {
        let executor = Arc::clone(self);
        let workspace = workspace.clone();
        std::thread::Builder::new()
            .name("hiroute-delegation-cancel".into())
            .spawn(move || {
                let Ok(runtime) = Builder::new_current_thread().enable_all().build() else {
                    return;
                };
                let _ = runtime.block_on(executor.cancellation.dispatch_pending(
                    executor.runtime.as_ref(),
                    &workspace,
                    executor.platform.as_ref(),
                ));
            })
            .map(|_| ())
            .map_err(|_| DelegationErrorV1::StorageUnavailable)
    }
}

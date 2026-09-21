//! Resident login-item establishment for the first managed connection, host-side only.
//!
//! Registration, permission status, and removal extend the existing native confirmation
//! boundary: the WebView never drives the system directly, and the daemon only journals the
//! host's declaration. A creation by this apply is compensated here when the apply never
//! commits; a pre-existing user login item is never touched.
use hiroute_application_api::AgentLoginItemDeclarationV2;

#[cfg(target_os = "macos")]
use hiroute_application_api::AgentLoginItemStatusV2;

const SERVICE_UNAVAILABLE: &str = "SERVICE_UNAVAILABLE";

/// Performs the real status check and, when needed, registration. `Ok` carries the
/// declaration proving an active login item; `Err` means the resident service stays
/// service_unavailable and the connection must not be shown as complete.
#[cfg(target_os = "macos")]
pub(crate) fn establish_resident_login_item() -> Result<AgentLoginItemDeclarationV2, &'static str> {
    use hiroute_desktop_host_effects as host;

    fn wire(status: host::LoginItemStatus) -> Option<AgentLoginItemStatusV2> {
        match status {
            host::LoginItemStatus::NotRegistered => Some(AgentLoginItemStatusV2::NotRegistered),
            host::LoginItemStatus::Enabled => Some(AgentLoginItemStatusV2::Enabled),
            host::LoginItemStatus::RequiresApproval => {
                Some(AgentLoginItemStatusV2::RequiresApproval)
            }
            host::LoginItemStatus::NotFound => Some(AgentLoginItemStatusV2::NotFound),
            // An unrecognized system state cannot prove an active item.
            host::LoginItemStatus::Unknown => None,
        }
    }

    let before = host::main_app_login_item_status().map_err(|_| SERVICE_UNAVAILABLE)?;
    let before = wire(before).ok_or(SERVICE_UNAVAILABLE)?;
    match before {
        AgentLoginItemStatusV2::Enabled => Ok(AgentLoginItemDeclarationV2 {
            before,
            after: AgentLoginItemStatusV2::Enabled,
            created: false,
        }),
        AgentLoginItemStatusV2::NotRegistered | AgentLoginItemStatusV2::NotFound => {
            let after = host::register_main_app_login_item().map_err(|_| SERVICE_UNAVAILABLE)?;
            let after = wire(after).ok_or(SERVICE_UNAVAILABLE)?;
            if after == AgentLoginItemStatusV2::Enabled {
                return Ok(AgentLoginItemDeclarationV2 {
                    before,
                    after,
                    created: true,
                });
            }
            // The registration exists but is not active (for example pending approval); undo
            // this apply's creation so no orphan survives a rejected apply.
            let _ = host::unregister_main_app_login_item();
            Err(SERVICE_UNAVAILABLE)
        }
        AgentLoginItemStatusV2::RequiresApproval => Err(SERVICE_UNAVAILABLE),
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn establish_resident_login_item() -> Result<AgentLoginItemDeclarationV2, &'static str> {
    Err(SERVICE_UNAVAILABLE)
}

/// Unregisters the login item this feature owns when the last managed connection is restored.
/// The backend only requires this with journal proof of ownership, so a pre-existing user
/// item never reaches this path; an item the user already removed manually is already absent.
#[cfg(target_os = "macos")]
pub(crate) fn remove_resident_login_item() -> Result<AgentLoginItemDeclarationV2, &'static str> {
    use hiroute_application_api::AgentLoginItemStatusV2;
    use hiroute_desktop_host_effects as host;

    fn wire(status: host::LoginItemStatus) -> Option<AgentLoginItemStatusV2> {
        match status {
            host::LoginItemStatus::NotRegistered => Some(AgentLoginItemStatusV2::NotRegistered),
            host::LoginItemStatus::Enabled => Some(AgentLoginItemStatusV2::Enabled),
            host::LoginItemStatus::RequiresApproval => {
                Some(AgentLoginItemStatusV2::RequiresApproval)
            }
            host::LoginItemStatus::NotFound => Some(AgentLoginItemStatusV2::NotFound),
            host::LoginItemStatus::Unknown => None,
        }
    }

    let before = wire(host::main_app_login_item_status().map_err(|_| SERVICE_UNAVAILABLE)?)
        .ok_or(SERVICE_UNAVAILABLE)?;
    match before {
        AgentLoginItemStatusV2::Enabled | AgentLoginItemStatusV2::RequiresApproval => {
            host::unregister_main_app_login_item().map_err(|_| SERVICE_UNAVAILABLE)?;
            let after = wire(host::main_app_login_item_status().map_err(|_| SERVICE_UNAVAILABLE)?)
                .ok_or(SERVICE_UNAVAILABLE)?;
            if after == AgentLoginItemStatusV2::Enabled
                || after == AgentLoginItemStatusV2::RequiresApproval
            {
                return Err(SERVICE_UNAVAILABLE);
            }
            Ok(AgentLoginItemDeclarationV2 {
                before,
                after,
                created: false,
            })
        }
        AgentLoginItemStatusV2::NotRegistered | AgentLoginItemStatusV2::NotFound => {
            Ok(AgentLoginItemDeclarationV2 {
                before,
                after: before,
                created: false,
            })
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn remove_resident_login_item() -> Result<AgentLoginItemDeclarationV2, &'static str> {
    Err(SERVICE_UNAVAILABLE)
}

/// Removes the login item this apply created when the apply definitively failed to commit.
/// A pre-existing user item is never removed, and an uncertain outcome never triggers it.
#[cfg(target_os = "macos")]
pub(crate) fn compensate_resident_login_item(created_by_this_apply: bool) {
    if created_by_this_apply {
        let _ = hiroute_desktop_host_effects::unregister_main_app_login_item();
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn compensate_resident_login_item(_created_by_this_apply: bool) {}

/// Re-registers the owned login item when a removal's restore definitively failed: the
/// rolled-back connection still needs the resident service at the next login.
#[cfg(target_os = "macos")]
pub(crate) fn compensate_resident_login_item_removal() {
    let _ = hiroute_desktop_host_effects::register_main_app_login_item();
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn compensate_resident_login_item_removal() {}

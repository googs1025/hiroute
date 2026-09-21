//! macOS ServiceManagement boundary. All Objective-C interop stays inside this module.
#![allow(unsafe_code)]

use objc2_service_management::{SMAppService, SMAppServiceStatus};

use crate::{LoginItemError, LoginItemStatus};

pub(super) fn formal_installation() -> bool {
    own_bundle().is_some()
}

/// The absolute .app bundle containing this executable, when the layout is a formally
/// installed bundle. Worktree, debug, and relative executables return None.
pub(super) fn own_bundle() -> Option<std::path::PathBuf> {
    let executable = std::env::current_exe().ok()?;
    if !executable.is_absolute() {
        return None;
    }
    let macos = executable.parent()?;
    if !macos.ends_with("MacOS") || !macos.is_dir() {
        return None;
    }
    let contents = macos.parent()?;
    if !contents.ends_with("Contents") {
        return None;
    }
    let bundle = contents.parent()?;
    if !bundle
        .extension()
        .is_some_and(|extension| extension == "app")
        || !bundle
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.len() > 4)
    {
        return None;
    }
    Some(bundle.to_path_buf())
}

pub(super) fn status() -> Result<LoginItemStatus, LoginItemError> {
    with_main_app_service(|service| Ok(map_status(unsafe { service.status() })))
}

pub(super) fn register() -> Result<LoginItemStatus, LoginItemError> {
    with_main_app_service(|service| {
        if unsafe { service.registerAndReturnError() }.is_err() {
            return Err(LoginItemError::RegistrationFailed);
        }
        Ok(map_status(unsafe { service.status() }))
    })
}

pub(super) fn unregister() -> Result<(), LoginItemError> {
    with_main_app_service(|service| {
        if unsafe { service.unregisterAndReturnError() }.is_err() {
            return Err(LoginItemError::UnregistrationFailed);
        }
        Ok(())
    })
}

fn with_main_app_service<T>(
    action: impl FnOnce(&SMAppService) -> Result<T, LoginItemError>,
) -> Result<T, LoginItemError> {
    if !formal_installation() {
        return Err(LoginItemError::NotFormalInstallation);
    }
    let service = unsafe { SMAppService::mainAppService() };
    action(&service)
}

fn map_status(status: SMAppServiceStatus) -> LoginItemStatus {
    if status == SMAppServiceStatus::Enabled {
        LoginItemStatus::Enabled
    } else if status == SMAppServiceStatus::RequiresApproval {
        LoginItemStatus::RequiresApproval
    } else if status == SMAppServiceStatus::NotFound {
        LoginItemStatus::NotFound
    } else if status == SMAppServiceStatus::NotRegistered {
        LoginItemStatus::NotRegistered
    } else {
        LoginItemStatus::Unknown
    }
}

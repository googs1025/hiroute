use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle};
use std::path::Path;
use std::ptr::{addr_of, null, null_mut};

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_ALL, GENERIC_READ, GENERIC_WRITE, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    EXPLICIT_ACCESS_W, GetNamedSecurityInfoW, GetSecurityInfo, NO_MULTIPLE_TRUSTEE, SE_FILE_OBJECT,
    SET_ACCESS, SetEntriesInAclW, SetNamedSecurityInfoW, TRUSTEE_IS_SID, TRUSTEE_IS_USER,
    TRUSTEE_W,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION,
    EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl, GetTokenInformation,
    InitializeSecurityDescriptor, NO_INHERITANCE, OWNER_SECURITY_INFORMATION,
    PROTECTED_DACL_SECURITY_INFORMATION, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES,
    SECURITY_DESCRIPTOR, SUB_CONTAINERS_AND_OBJECTS_INHERIT, SetSecurityDescriptorControl,
    SetSecurityDescriptorDacl, SetSecurityDescriptorOwner, TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, CREATE_NEW, CreateDirectoryW, CreateFileW, DELETE, FILE_ALL_ACCESS,
    FILE_ATTRIBUTE_NORMAL, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_NONE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FileDispositionInfo, GetFileInformationByHandle, SetFileInformationByHandle,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

// WinNT.h defines ACCESS_ALLOWED_ACE_TYPE as zero. windows-sys exposes the
// constant behind an unrelated SystemServices feature, while the ACE itself
// correctly lives in Security.
const ACCESS_ALLOWED_ACE_KIND: u8 = 0;

pub(crate) fn create_owner_only_directory(path: &Path) -> io::Result<File> {
    let mut security = OwnerSecurity::new(true)?;
    let attributes = security.attributes()?;
    let wide = wide_path(path)?;
    // SAFETY: the path and owner-only security descriptor remain live for the
    // complete creation call. The DACL is present before the directory can be
    // observed by another process.
    if unsafe { CreateDirectoryW(wide.as_ptr(), &attributes) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let mut options = OpenOptions::new();
    options
        .access_mode(GENERIC_READ | GENERIC_WRITE | DELETE)
        // A second read handle is required for child enumeration, but delete
        // sharing remains denied so the directory identity stays pinned.
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    let file = options.open(path)?;
    verify_owner_only_handle(&file, true)?;
    Ok(file)
}

pub(crate) fn create_owner_only_file(path: &Path) -> io::Result<File> {
    let mut security = OwnerSecurity::new(false)?;
    let attributes = security.attributes()?;
    let wide = wide_path(path)?;
    // SAFETY: every pointer is live, the descriptor is attached atomically,
    // and CREATE_NEW prevents replacement of an existing reparse point.
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ | GENERIC_WRITE | DELETE,
            FILE_SHARE_NONE,
            &attributes,
            CREATE_NEW,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OPEN_REPARSE_POINT,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned a unique owned handle.
    let file = unsafe { File::from_raw_handle(handle.cast()) };
    verify_owner_only_handle(&file, false)?;
    Ok(file)
}

pub(crate) fn delete_by_handle(file: &File) -> io::Result<()> {
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // SAFETY: `file` owns a DELETE-capable handle and the buffer exactly
    // matches FILE_DISPOSITION_INFO for FileDispositionInfo.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle().cast(),
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            size_of_u32::<FILE_DISPOSITION_INFO>()?,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn file_identity(file: &File) -> io::Result<(u32, u64)> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` owns a live handle and `information` is the exact output
    // structure required by GetFileInformationByHandle.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        information.dwVolumeSerialNumber,
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    ))
}

pub(crate) fn set_owner_only(path: &Path, directory: bool) -> io::Result<()> {
    let user = CurrentUser::load()?;
    let wide = wide_path(path)?;
    let mut acl: *mut ACL = null_mut();
    let access = EXPLICIT_ACCESS_W {
        grfAccessPermissions: GENERIC_ALL,
        grfAccessMode: SET_ACCESS,
        grfInheritance: if directory {
            SUB_CONTAINERS_AND_OBJECTS_INHERIT
        } else {
            NO_INHERITANCE
        },
        Trustee: TRUSTEE_W {
            pMultipleTrustee: null_mut(),
            MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
            TrusteeForm: TRUSTEE_IS_SID,
            TrusteeType: TRUSTEE_IS_USER,
            ptstrName: user.sid().cast(),
        },
    };
    // SAFETY: `access` and `acl` are valid for this call; the returned ACL is
    // released with LocalFree after SetNamedSecurityInfoW consumes it.
    let status = unsafe { SetEntriesInAclW(1, &access, null(), &mut acl) };
    if status != 0 {
        return Err(win32_error(status));
    }
    // SAFETY: `wide` is NUL terminated, `user` and `acl` remain live, and all
    // other optional security components are intentionally null.
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            acl,
            null(),
        )
    };
    // SAFETY: SetEntriesInAclW allocated `acl` with LocalAlloc.
    unsafe { LocalFree(acl.cast()) };
    if status != 0 {
        return Err(win32_error(status));
    }
    verify_owner_only(path, directory)
}

struct OwnerSecurity {
    user: CurrentUser,
    acl: *mut ACL,
    descriptor: SECURITY_DESCRIPTOR,
}

impl OwnerSecurity {
    fn new(directory: bool) -> io::Result<Self> {
        let user = CurrentUser::load()?;
        let mut acl: *mut ACL = null_mut();
        let access = EXPLICIT_ACCESS_W {
            grfAccessPermissions: GENERIC_ALL,
            grfAccessMode: SET_ACCESS,
            grfInheritance: if directory {
                SUB_CONTAINERS_AND_OBJECTS_INHERIT
            } else {
                NO_INHERITANCE
            },
            Trustee: TRUSTEE_W {
                pMultipleTrustee: null_mut(),
                MultipleTrusteeOperation: NO_MULTIPLE_TRUSTEE,
                TrusteeForm: TRUSTEE_IS_SID,
                TrusteeType: TRUSTEE_IS_USER,
                ptstrName: user.sid().cast(),
            },
        };
        // SAFETY: `access` is fully initialized and `acl` is a live out ptr.
        let status = unsafe { SetEntriesInAclW(1, &access, null(), &mut acl) };
        if status != 0 {
            return Err(win32_error(status));
        }
        let mut descriptor = SECURITY_DESCRIPTOR::default();
        // SAFETY: the descriptor, current SID, and ACL all outlive creation.
        let initialized = unsafe {
            InitializeSecurityDescriptor((&mut descriptor as *mut SECURITY_DESCRIPTOR).cast(), 1)
                != 0
                && SetSecurityDescriptorOwner(
                    (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast(),
                    user.sid(),
                    0,
                ) != 0
                && SetSecurityDescriptorDacl(
                    (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast(),
                    1,
                    acl,
                    0,
                ) != 0
                && SetSecurityDescriptorControl(
                    (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast(),
                    SE_DACL_PROTECTED,
                    SE_DACL_PROTECTED,
                ) != 0
        };
        if !initialized {
            // SAFETY: SetEntriesInAclW allocated this ACL with LocalAlloc.
            unsafe { LocalFree(acl.cast()) };
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            user,
            acl,
            descriptor,
        })
    }

    fn attributes(&mut self) -> io::Result<SECURITY_ATTRIBUTES> {
        Ok(SECURITY_ATTRIBUTES {
            nLength: size_of_u32::<SECURITY_ATTRIBUTES>()?,
            lpSecurityDescriptor: (&mut self.descriptor as *mut SECURITY_DESCRIPTOR).cast(),
            bInheritHandle: 0,
        })
    }
}

impl Drop for OwnerSecurity {
    fn drop(&mut self) {
        // Keep the SID owner alive until after the descriptor is no longer in
        // use, then release the ACL allocated by SetEntriesInAclW.
        let _ = self.user.words.len();
        if !self.acl.is_null() {
            // SAFETY: this allocation is uniquely owned by `self`.
            unsafe { LocalFree(self.acl.cast()) };
        }
    }
}

pub(crate) fn verify_owner_only(path: &Path, directory: bool) -> io::Result<()> {
    let user = CurrentUser::load()?;
    let wide = wide_path(path)?;
    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: all out-pointers are valid and `wide` is NUL terminated. The
    // returned descriptor owns the owner/DACL views and is freed below.
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(win32_error(status));
    }
    validate_security_descriptor(&user, owner, dacl, descriptor, directory)
}

pub(crate) fn verify_owner_only_handle(file: &File, directory: bool) -> io::Result<()> {
    let user = CurrentUser::load()?;
    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: the file owns a live kernel handle and every out-pointer remains
    // valid until the returned descriptor is released below.
    let status = unsafe {
        GetSecurityInfo(
            file.as_raw_handle().cast(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(win32_error(status));
    }
    validate_security_descriptor(&user, owner, dacl, descriptor, directory)
}

fn validate_security_descriptor(
    user: &CurrentUser,
    owner: PSID,
    dacl: *mut ACL,
    descriptor: *mut c_void,
    directory: bool,
) -> io::Result<()> {
    let descriptor = LocalAllocation(descriptor);
    if owner.is_null() || dacl.is_null() {
        return Err(permission_error());
    }
    let mut control = 0_u16;
    let mut revision = 0_u32;
    // SAFETY: `descriptor` owns the live security descriptor returned above.
    if unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
    {
        return Err(permission_error());
    }
    // SAFETY: both SIDs are live for the duration of this function.
    if unsafe { EqualSid(owner, user.sid()) } == 0 {
        return Err(permission_error());
    }
    let mut information = ACL_SIZE_INFORMATION::default();
    // SAFETY: `dacl` points into the live security descriptor and the output
    // buffer has exactly the declared ACL_SIZE_INFORMATION size.
    if unsafe {
        GetAclInformation(
            dacl,
            (&mut information as *mut ACL_SIZE_INFORMATION).cast(),
            size_of_u32::<ACL_SIZE_INFORMATION>()?,
            AclSizeInformation,
        )
    } == 0
        || information.AceCount != 1
    {
        return Err(permission_error());
    }
    let mut ace: *mut c_void = null_mut();
    // SAFETY: the DACL reports one ACE and index zero is therefore valid.
    if unsafe { GetAce(dacl, 0, &mut ace) } == 0 || ace.is_null() {
        return Err(permission_error());
    }
    // SAFETY: the ACE type is checked before fields specific to an
    // ACCESS_ALLOWED_ACE are interpreted.
    let ace = unsafe { &*(ace.cast::<ACCESS_ALLOWED_ACE>()) };
    let expected_flags = if directory {
        SUB_CONTAINERS_AND_OBJECTS_INHERIT as u8
    } else {
        NO_INHERITANCE as u8
    };
    if ace.Header.AceType != ACCESS_ALLOWED_ACE_KIND
        || ace.Header.AceFlags != expected_flags
        || (ace.Mask != GENERIC_ALL && ace.Mask != FILE_ALL_ACCESS)
    {
        return Err(permission_error());
    }
    let ace_sid = addr_of!(ace.SidStart).cast_mut().cast::<c_void>();
    // SAFETY: ACCESS_ALLOWED_ACE stores its variable-length SID at SidStart.
    if unsafe { EqualSid(ace_sid, user.sid()) } == 0 {
        return Err(permission_error());
    }
    drop(descriptor);
    Ok(())
}

struct CurrentUser {
    words: Vec<usize>,
}

impl CurrentUser {
    fn load() -> io::Result<Self> {
        let mut token: HANDLE = null_mut();
        // SAFETY: `token` is a valid output pointer and the pseudo process
        // handle is valid in the current process.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = OwnedHandle(token);
        let mut bytes = 0_u32;
        // SAFETY: the documented sizing call accepts a null buffer and writes
        // the required byte count.
        unsafe {
            GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut bytes);
        }
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let word = std::mem::size_of::<usize>();
        let words = (bytes as usize)
            .checked_add(word - 1)
            .ok_or_else(permission_error)?
            / word;
        let mut buffer = vec![0_usize; words];
        // SAFETY: the word-aligned buffer has at least the byte size returned
        // by the sizing call.
        if unsafe {
            GetTokenInformation(
                token.0,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                bytes,
                &mut bytes,
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(Self { words: buffer })
    }

    fn sid(&self) -> PSID {
        // SAFETY: `words` contains a TOKEN_USER populated by Windows and stays
        // immovable while its embedded SID pointer is used.
        unsafe { (*(self.words.as_ptr().cast::<TOKEN_USER>())).User.Sid }
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: this handle was returned by OpenProcessToken and is owned.
        unsafe { CloseHandle(self.0) };
    }
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: GetNamedSecurityInfoW allocated this descriptor.
            unsafe { LocalFree(self.0) };
        }
    }
}

fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
    if wide.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "replay path contains an embedded NUL",
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn size_of_u32<T>() -> io::Result<u32> {
    u32::try_from(std::mem::size_of::<T>()).map_err(|_| permission_error())
}

fn win32_error(code: u32) -> io::Error {
    io::Error::from_raw_os_error(code as i32)
}

fn permission_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "replay DACL is not owner-only",
    )
}

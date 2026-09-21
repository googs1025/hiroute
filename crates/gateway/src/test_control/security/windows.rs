use std::ffi::c_void;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::ptr::{addr_of, null_mut};

use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_READ, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{GetSecurityInfo, SE_FILE_OBJECT};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACL, ACL_SIZE_INFORMATION, AclSizeInformation, DACL_SECURITY_INFORMATION,
    EqualSid, GetAce, GetAclInformation, GetSecurityDescriptorControl, GetTokenInformation,
    NO_INHERITANCE, OWNER_SECURITY_INFORMATION, PSID, SE_DACL_PROTECTED, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ALL_ACCESS, FILE_ATTRIBUTE_DIRECTORY,
    FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_DELETE_ON_CLOSE, FILE_FLAG_OPEN_REPARSE_POINT,
    GetFileInformationByHandle,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use windows_sys::Win32::Foundation::GENERIC_ALL;

// WinNT.h defines ACCESS_ALLOWED_ACE_TYPE as zero; windows-sys exposes the
// constant behind an unrelated feature while the ACE type itself is present.
const ACCESS_ALLOWED_ACE_KIND: u8 = 0;

pub(super) fn open_owner_only_file(path: &std::path::Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .access_mode(GENERIC_READ | DELETE)
        .share_mode(0)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_DELETE_ON_CLOSE);
    let file = options.open(path)?;
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` owns a live handle and the output structure is exact.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    if information.dwFileAttributes & (FILE_ATTRIBUTE_REPARSE_POINT | FILE_ATTRIBUTE_DIRECTORY) != 0
    {
        return Err(permission_error());
    }
    verify_owner_only_handle(&file)?;
    Ok(file)
}

fn verify_owner_only_handle(file: &File) -> io::Result<()> {
    let user = CurrentUser::load()?;
    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor = null_mut();
    // SAFETY: the handle and all output pointers remain valid through the
    // call; the returned descriptor owns its owner and DACL views.
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
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    validate_security_descriptor(&user, owner, dacl, descriptor)
}

fn validate_security_descriptor(
    user: &CurrentUser,
    owner: PSID,
    dacl: *mut ACL,
    descriptor: *mut c_void,
) -> io::Result<()> {
    let descriptor = LocalAllocation(descriptor);
    if owner.is_null() || dacl.is_null() {
        return Err(permission_error());
    }
    let mut control = 0_u16;
    let mut revision = 0_u32;
    // SAFETY: all security views are owned by the live descriptor.
    if unsafe { GetSecurityDescriptorControl(descriptor.0, &mut control, &mut revision) } == 0
        || control & SE_DACL_PROTECTED == 0
        || unsafe { EqualSid(owner, user.sid()) } == 0
    {
        return Err(permission_error());
    }
    let mut information = ACL_SIZE_INFORMATION::default();
    // SAFETY: `dacl` is a live view and the output buffer has the exact type.
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
    // SAFETY: the DACL reports exactly one ACE, so index zero is valid.
    if unsafe { GetAce(dacl, 0, &mut ace) } == 0 || ace.is_null() {
        return Err(permission_error());
    }
    // SAFETY: the ACE type is checked before its type-specific fields matter.
    let ace = unsafe { &*ace.cast::<ACCESS_ALLOWED_ACE>() };
    if ace.Header.AceType != ACCESS_ALLOWED_ACE_KIND
        || ace.Header.AceFlags != NO_INHERITANCE as u8
        || (ace.Mask != GENERIC_ALL && ace.Mask != FILE_ALL_ACCESS)
        || unsafe { EqualSid(addr_of!(ace.SidStart).cast_mut().cast(), user.sid()) } == 0
    {
        return Err(permission_error());
    }
    Ok(())
}

struct CurrentUser {
    words: Vec<usize>,
}

impl CurrentUser {
    fn load() -> io::Result<Self> {
        let mut token: HANDLE = null_mut();
        // SAFETY: `token` is a valid output pointer for the current process.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = OwnedHandle(token);
        let mut bytes = 0_u32;
        // SAFETY: the documented sizing call accepts a null buffer.
        unsafe { GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut bytes) };
        if bytes == 0 {
            return Err(io::Error::last_os_error());
        }
        let word = std::mem::size_of::<usize>();
        let words = (bytes as usize)
            .checked_add(word - 1)
            .ok_or_else(permission_error)?
            / word;
        let mut buffer = vec![0_usize; words];
        // SAFETY: the word-aligned buffer has the requested byte capacity.
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
        // SAFETY: the buffer contains TOKEN_USER and remains live.
        unsafe { (*self.words.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
}

struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: OpenProcessToken returned this uniquely owned handle.
        unsafe { CloseHandle(self.0) };
    }
}

struct LocalAllocation(*mut c_void);

impl Drop for LocalAllocation {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: GetSecurityInfo allocated this descriptor.
            unsafe { LocalFree(self.0) };
        }
    }
}

fn size_of_u32<T>() -> io::Result<u32> {
    u32::try_from(std::mem::size_of::<T>()).map_err(|_| permission_error())
}

fn permission_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "E2E control nonce DACL is not owner-only",
    )
}

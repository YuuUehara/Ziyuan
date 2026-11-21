//! Task-private filesystem ACL admission, low-integrity labeling, and path canonicalization.

use std::{
    fs, io,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};

use windows_sys::Win32::{
    Foundation::{ERROR_SUCCESS, LocalFree},
    Security::{
        Authorization::{
            ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1, SE_FILE_OBJECT,
            SetNamedSecurityInfoW,
        },
        DACL_SECURITY_INFORMATION, GetSecurityDescriptorDacl, GetSecurityDescriptorSacl,
        LABEL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, PSID, TOKEN_USER,
        TokenUser,
    },
    Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, GetFileAttributesW, INVALID_FILE_ATTRIBUTES,
    },
};

use super::{checked, sid_text, token_information, wide};

/// Rejects any path where the target or any non-root ancestor is a Win32 reparse point
/// (symlink, junction, or mount point). Fails closed if attributes cannot be queried.
pub(super) fn reject_reparse_point(path: &Path) -> io::Result<()> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path must be absolute: {}", path.display()),
        ));
    }
    for ancestor in path.ancestors() {
        if ancestor.parent().is_none() {
            break;
        }
        // SAFETY: `wide(ancestor)` produces a NUL-terminated UTF-16 path living through the call.
        let attrs = unsafe { GetFileAttributesW(wide(ancestor).as_ptr()) };
        if attrs == INVALID_FILE_ATTRIBUTES {
            return Err(io::Error::last_os_error());
        }
        if attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "refusing reparse point in sandbox path: {}",
                    ancestor.display()
                ),
            ));
        }
    }
    Ok(())
}

pub(crate) fn admit_dependency(root: &Path, app_sid: PSID, writable: bool) -> io::Result<()> {
    reject_reparse_point(root)?;
    let storage = token_information(TokenUser)?;
    // SAFETY: exact API class, aligned live storage including the pointed-to SID.
    let user = unsafe { sid_text((*storage.as_ptr().cast::<TOKEN_USER>()).User.Sid)? };
    let app = sid_text(app_sid)?;
    // Only this exclusively created dependency directory is changed. Protect
    // against parent inheritance, keep host cleanup rights, and suppress implicit
    // owner WRITE_DAC: the LPAC still carries the host user's owner identity.
    // Install object-specific masks, not generic bits, in existing file ACLs.
    let access = if writable { "FA" } else { "FRFX" };
    let sddl = wide(format!(
        "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;{user})(A;OICI;RC;;;OW)(A;OICI;{access};;;{app})"
    ));
    let mut descriptor = null_mut();
    // SAFETY: converted descriptor and embedded DACL remain live until the
    // setting call ends. Its inheritable ACEs also protect child files.
    unsafe {
        checked(ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        ))?;
        let result = (|| {
            let mut present = 0;
            let mut defaulted = 0;
            let mut dacl = null_mut();
            checked(GetSecurityDescriptorDacl(
                descriptor,
                &mut present,
                &mut dacl,
                &mut defaulted,
            ))?;
            if present == 0 || dacl.is_null() {
                return Err(io::Error::other("dependency DACL missing"));
            }
            let error = SetNamedSecurityInfoW(
                wide(root).as_ptr(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                dacl,
                null(),
            );
            if error != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            Ok(())
        })();
        LocalFree(descriptor);
        result
    }
}

pub(crate) fn low_integrity(path: &Path) -> io::Result<()> {
    let mut descriptor = null_mut();
    // SAFETY: label only our owned synthetic objects. No audit/host SACL is
    // replaced. All pointers live through the call and use matching LocalFree.
    unsafe {
        checked(ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide("S:(ML;;NW;;;LW)").as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            null_mut(),
        ))?;
        let result = (|| {
            let mut present = 0;
            let mut defaulted = 0;
            let mut sacl = null_mut();
            checked(GetSecurityDescriptorSacl(
                descriptor,
                &mut present,
                &mut sacl,
                &mut defaulted,
            ))?;
            if present == 0 || sacl.is_null() {
                return Err(io::Error::other("dependency integrity label missing"));
            }
            let error = SetNamedSecurityInfoW(
                wide(path).as_ptr(),
                SE_FILE_OBJECT,
                LABEL_SECURITY_INFORMATION,
                null_mut(),
                null_mut(),
                null(),
                sacl,
            );
            if error != ERROR_SUCCESS {
                return Err(io::Error::from_raw_os_error(error as i32));
            }
            Ok(())
        })();
        LocalFree(descriptor);
        result
    }
}

/// Admits a task-private directory tree with protected DACL + explicit low IL,
/// rejecting any reparse point encountered within the tree.
fn admit_tree(root: &Path, app_sid: PSID, writable: bool) -> io::Result<()> {
    admit_dependency(root, app_sid, writable)?;
    low_integrity(root)?;
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    let mut visited = 0usize;
    while let Some((dir, depth)) = stack.pop() {
        if depth > 16 {
            return Err(io::Error::other(
                "admitted directory tree exceeds max depth",
            ));
        }
        for entry in fs::read_dir(&dir)? {
            visited += 1;
            if visited > 8192 {
                return Err(io::Error::other(
                    "admitted directory tree exceeds max entry count",
                ));
            }
            let path = entry?.path();
            reject_reparse_point(&path)?;
            low_integrity(&path)?;
            if path.is_dir() {
                stack.push((path, depth + 1));
            }
        }
    }
    Ok(())
}

/// Admits read-only and writable task-private directories in deterministic outer-before-inner order
/// so a task-private writable subdirectory inside a read-only workspace snapshot keeps its own
/// protected `FA` DACL without being overwritten by the outer `FRFX` DACL.
pub(super) fn admit_directories(
    readonly_dirs: &[PathBuf],
    writable_dirs: &[PathBuf],
    app_sid: PSID,
) -> io::Result<()> {
    let mut entries: Vec<(PathBuf, bool, String)> =
        Vec::with_capacity(readonly_dirs.len() + writable_dirs.len());
    let mut seen_ro = std::collections::BTreeSet::new();
    for dir in readonly_dirs {
        let norm = normalize_win32_long_path(dir)?;
        let key = norm.to_string_lossy().to_ascii_lowercase();
        if seen_ro.insert(key.clone()) {
            entries.push((norm, false, key));
        }
    }
    let mut seen_rw = std::collections::BTreeSet::new();
    for dir in writable_dirs {
        let norm = normalize_win32_long_path(dir)?;
        let key = norm.to_string_lossy().to_ascii_lowercase();
        if seen_ro.contains(&key) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "directory cannot be both read-only and writable: {}",
                    norm.display()
                ),
            ));
        }
        if seen_rw.insert(key.clone()) {
            entries.push((norm, true, key));
        }
    }
    entries.sort_by(|a, b| {
        a.0.components()
            .count()
            .cmp(&b.0.components().count())
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    for (dir, writable, _) in entries {
        admit_tree(&dir, app_sid, writable)?;
    }
    Ok(())
}

/// Expands 8.3 short path components (e.g. `RUNNER~1`) to their canonical Win32 long path
/// without keeping a `\\?\` prefix for standard drive-letter paths.
///
/// Inside an AppContainer, ancestor directories above the admitted workspace (such as
/// `C:\Users\<user>`) are not listable. Runtimes such as .NET / Windows PowerShell invoke
/// `GetLongPathNameW` whenever a path contains `~`, which enumerates ancestor directories
/// and fails with `ERROR_ACCESS_DENIED`. Expanding to the long path on the host after
/// `reject_reparse_point` prevents short-name expansion inside the sandbox.
pub(super) fn normalize_win32_long_path(path: &Path) -> io::Result<PathBuf> {
    let canonical = fs::canonicalize(path)?;
    if let Some(raw) = canonical.to_str()
        && let Some(stripped) = raw.strip_prefix(r"\\?\")
    {
        let bytes = stripped.as_bytes();
        if bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'\\'
        {
            return Ok(PathBuf::from(stripped));
        }
    }
    Ok(canonical)
}

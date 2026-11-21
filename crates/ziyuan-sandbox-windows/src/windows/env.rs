//! Command-line quoting, allowlisted environment construction, bounded output tailing, and pipes.

use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::File,
    io::{self, Read, Seek, SeekFrom},
    os::windows::io::OwnedHandle,
    path::{Path, PathBuf},
    ptr::{null, null_mut},
};

use windows_sys::Win32::System::Pipes::CreatePipe;

use super::{checked, own, wide};

pub(super) fn quote_windows_arg(arg: &str) -> io::Result<String> {
    if arg.contains('\0') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "argument contains NUL byte",
        ));
    }
    if !arg.is_empty() && !arg.chars().any(|c| matches!(c, ' ' | '\t' | '"')) {
        return Ok(arg.to_owned());
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for ch in arg.chars() {
        match ch {
            '\\' => backslashes += 1,
            '"' => {
                for _ in 0..(backslashes * 2 + 1) {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            other => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(other);
            }
        }
    }
    for _ in 0..(backslashes * 2) {
        out.push('\\');
    }
    out.push('"');
    Ok(out)
}

pub(super) fn build_environment(
    profile_dir: &Path,
    temp_dir: &Path,
    home_dir: &Path,
    appdata_dir: &Path,
    extra: &[(String, String)],
) -> io::Result<Vec<u16>> {
    let root = std::env::var_os("SystemRoot")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            io::Error::other("SystemRoot unavailable; refusing to inherit the host environment")
        })?;
    let mut map: BTreeMap<String, (String, OsString)> = BTreeMap::new();
    map.insert(
        "LOCALAPPDATA".into(),
        ("LOCALAPPDATA".into(), profile_dir.as_os_str().to_owned()),
    );
    map.insert(
        "APPDATA".into(),
        ("APPDATA".into(), appdata_dir.as_os_str().to_owned()),
    );
    map.insert(
        "USERPROFILE".into(),
        ("USERPROFILE".into(), home_dir.as_os_str().to_owned()),
    );
    map.insert(
        "HOME".into(),
        ("HOME".into(), home_dir.as_os_str().to_owned()),
    );
    let comspec = PathBuf::from(&root).join("System32").join("cmd.exe");
    map.insert("SYSTEMROOT".into(), ("SystemRoot".into(), root.clone()));
    map.insert("WINDIR".into(), ("WINDIR".into(), root.clone()));
    map.insert(
        "COMSPEC".into(),
        ("ComSpec".into(), comspec.into_os_string()),
    );
    if let Some(drive) = root.to_string_lossy().get(..2)
        && drive.as_bytes()[0].is_ascii_alphabetic()
        && drive.as_bytes()[1] == b':'
    {
        map.insert(
            "SYSTEMDRIVE".into(),
            ("SystemDrive".into(), OsString::from(drive)),
        );
    }
    map.insert(
        "PATHEXT".into(),
        ("PATHEXT".into(), OsString::from(".COM;.EXE;.BAT;.CMD")),
    );
    map.insert(
        "TEMP".into(),
        ("TEMP".into(), temp_dir.as_os_str().to_owned()),
    );
    map.insert(
        "TMP".into(),
        ("TMP".into(), temp_dir.as_os_str().to_owned()),
    );
    const RESERVED: [&str; 12] = [
        "SYSTEMROOT",
        "WINDIR",
        "COMSPEC",
        "SYSTEMDRIVE",
        "LOCALAPPDATA",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "HOME",
        "APPDATA",
        "HOMEDRIVE",
        "HOMEPATH",
    ];
    for (k, v) in extra {
        if k.is_empty() || k.contains('=') || k.contains('\0') || v.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid sandbox environment variable name/value: {k:?}"),
            ));
        }
        let upper = k.to_ascii_uppercase();
        if RESERVED.contains(&upper.as_str()) {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("refusing override of reserved sandbox env var: {k}"),
            ));
        }
        map.insert(upper, (k.clone(), OsString::from(v)));
    }
    let mut block = Vec::new();
    for (key, value) in map.into_values() {
        let mut entry = OsString::from(key);
        entry.push("=");
        entry.push(value);
        block.extend(wide(entry));
    }
    block.push(0);
    Ok(block)
}

pub(super) fn read_tail_bounded(path: &Path, max_bytes: u64) -> io::Result<(Vec<u8>, u64, bool)> {
    let mut file = File::open(path)?;
    let total = file.metadata()?.len();
    let start = total.saturating_sub(max_bytes);
    if start > 0 {
        file.seek(SeekFrom::Start(start))?;
    }
    let to_read = usize::try_from(total - start).unwrap_or(usize::MAX);
    let mut buf = Vec::with_capacity(to_read.min(64 * 1024));
    file.take(max_bytes).read_to_end(&mut buf)?;
    Ok((buf, total, total > max_bytes))
}

pub(super) fn create_anonymous_pipe() -> io::Result<(OwnedHandle, OwnedHandle)> {
    let mut read_raw = null_mut();
    let mut write_raw = null_mut();
    // SAFETY: null SECURITY_ATTRIBUTES creates both ends non-inheritable by default;
    // both newly created handles are immediately wrapped in OwnedHandle.
    unsafe {
        checked(CreatePipe(&mut read_raw, &mut write_raw, null(), 0))?;
        let read = own(read_raw)?;
        let write = own(write_raw)?;
        Ok((read, write))
    }
}

//! W1b's first file matrix: synthetic fixtures only, no ACL changes.
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const HOST_FILES: [(&str, &str); 4] = [
    ("source", "source.rs"),
    ("journal", ".ziyuan/court.log"),
    ("credential", ".ssh/id_ed25519"),
    ("vcs", ".git/config"),
];
const SENTINEL: &[u8] = b"synthetic-host-marker-v1\n";
const PRIVATE_INPUT: &[u8] = b"private-input-v1\n";
const PRIVATE_OUTPUT: &[u8] = b"private-output-v1\n";

pub(super) const CHILD_EVIDENCE: &str = "FILE_PRIVATE_READ=1 FILE_PRIVATE_WRITE=1 FILE_TEMP_WRITE=1\n\
FILE_DENIED=source READ_WIN32=5 WRITE_WIN32=5\n\
FILE_DENIED=journal READ_WIN32=5 WRITE_WIN32=5\n\
FILE_DENIED=credential READ_WIN32=5 WRITE_WIN32=5\n\
FILE_DENIED=vcs READ_WIN32=5 WRITE_WIN32=5\n\
FILE_DENIED=peer READ_WIN32=5 WRITE_WIN32=5";

struct Directory {
    path: PathBuf,
    cleanup: bool,
}

impl Directory {
    fn create(path: PathBuf) -> io::Result<Self> {
        // Never claim or clean up a directory that already existed.
        fs::create_dir(&path)?;
        Ok(Self {
            path,
            cleanup: true,
        })
    }

    fn cleanup(&mut self) -> io::Result<()> {
        if self.cleanup {
            fs::remove_dir_all(&self.path)?;
            if self.path.try_exists()? {
                return Err(io::Error::other("file fixture remains after removal"));
            }
            self.cleanup = false;
        }
        Ok(())
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("W1b fixture cleanup failed: {error}");
        }
    }
}

pub(super) struct Fixtures {
    host: Directory,
    private: Directory,
    peer: Directory,
}

impl Fixtures {
    pub(super) fn create(profile: &Path, peer_profile: &Path) -> io::Result<Self> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let fixtures = Self {
            host: Directory::create(std::env::temp_dir().join(format!(
                "ziyuan-w1b-{}-{stamp} 空格资料",
                std::process::id()
            )))?,
            private: Directory::create(profile.join("w1b-files"))?,
            peer: Directory::create(peer_profile.join("w1b-files"))?,
        };
        for (_, relative) in HOST_FILES {
            let path = fixtures.host.path.join(relative);
            fs::create_dir_all(path.parent().expect("synthetic file parent"))?;
            fs::write(path, SENTINEL)?;
        }
        fs::write(fixtures.peer.path.join("peer.txt"), SENTINEL)?;
        fs::write(fixtures.private.path.join("input.txt"), PRIVATE_INPUT)?;
        fs::create_dir(fixtures.private.path.join("temp"))?;
        // Establish host read AND write-open controls with no handles retained
        // across creation. No sharing violation can masquerade as access denial.
        for path in fixtures.protected_files() {
            if fs::read(&path)? != SENTINEL {
                return Err(io::Error::other("host file control content mismatch"));
            }
            drop(OpenOptions::new().write(true).open(path)?);
        }
        Ok(fixtures)
    }

    fn protected_files(&self) -> Vec<PathBuf> {
        HOST_FILES
            .iter()
            .map(|(_, path)| self.host.path.join(path))
            .chain([self.peer.path.join("peer.txt")])
            .collect()
    }

    pub(super) fn host_path(&self) -> &Path {
        &self.host.path
    }

    pub(super) fn peer_path(&self) -> &Path {
        &self.peer.path
    }

    pub(super) fn verify(&self) -> io::Result<()> {
        for path in self.protected_files() {
            if fs::read(path)? != SENTINEL {
                return Err(io::Error::other("protected file changed"));
            }
        }
        if fs::read(self.private.path.join("input.txt"))? != PRIVATE_OUTPUT
            || fs::read(self.private.path.join("output.txt"))? != PRIVATE_OUTPUT
            || fs::read(self.private.path.join("temp/result.txt"))? != PRIVATE_OUTPUT
        {
            return Err(io::Error::other("private child file output mismatch"));
        }
        Ok(())
    }

    pub(super) fn cleanup(&mut self) -> io::Result<()> {
        // Run all cleanup attempts even if one fails; do not report success if
        // any of the three owned fixture trees remains.
        let host = self.host.cleanup();
        let private = self.private.cleanup();
        let peer = self.peer.cleanup();
        host.and(private).and(peer)
    }

    pub(super) fn retain(&mut self) {
        for directory in [&mut self.host, &mut self.private, &mut self.peer] {
            eprintln!("W1b retained fixture: {}", directory.path.display());
            directory.cleanup = false;
        }
    }
}

fn denied(label: &str, path: &Path) -> io::Result<()> {
    let read = fs::read(path);
    // Request write access but never truncate or modify a protected file,
    // even if the experiment unexpectedly grants the request.
    let write = OpenOptions::new().write(true).open(path);
    let read_code = read.as_ref().err().and_then(io::Error::raw_os_error);
    let write_code = write.as_ref().err().and_then(io::Error::raw_os_error);
    if read_code != Some(5) || write_code != Some(5) {
        return Err(io::Error::other(format!(
            "{label}: expected ERROR_ACCESS_DENIED; read={read_code:?}, write={write_code:?}"
        )));
    }
    println!("FILE_DENIED={label} READ_WIN32=5 WRITE_WIN32=5");
    Ok(())
}

pub(super) fn child(host: &Path, peer: &Path) -> io::Result<()> {
    // Caller has already checked AppContainer / LPAC and Job identity. All
    // private files are relative to the fresh task profile, never a caller's cwd.
    let private = std::env::current_dir()?.join("w1b-files");
    if fs::read(private.join("input.txt"))? != PRIVATE_INPUT {
        return Err(io::Error::other("private read control mismatch"));
    }
    // The admitted copy must be writable, not merely its parent directory.
    fs::write(private.join("input.txt"), PRIVATE_OUTPUT)?;
    if fs::read(private.join("input.txt"))? != PRIVATE_OUTPUT {
        return Err(io::Error::other("private copy write control mismatch"));
    }
    for path in [private.join("output.txt"), private.join("temp/result.txt")] {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(PRIVATE_OUTPUT)?;
        file.sync_all()?;
        drop(file);
        if fs::read(path)? != PRIVATE_OUTPUT {
            return Err(io::Error::other("private write/read control mismatch"));
        }
    }
    println!("FILE_PRIVATE_READ=1 FILE_PRIVATE_WRITE=1 FILE_TEMP_WRITE=1");
    for (label, path) in HOST_FILES {
        denied(label, &host.join(path))?;
    }
    denied("peer", &peer.join("peer.txt"))?;
    io::stdout().flush()
}

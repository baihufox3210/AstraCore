//! Atomic, private JSON persistence shared by configuration and daemon state.
use serde::Serialize;
use std::{
    fs,
    io::{self, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};

/// Write `value` as pretty JSON with mode 0600, then fsync the file and directory.
/// With `replace`, an existing file is atomically replaced; otherwise it is never overwritten.
/// Symlinked destinations are refused.
pub fn write_json(path: &Path, value: &impl Serialize, replace: bool) -> io::Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "refusing to replace a symlinked file",
        ));
    }
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let (temp, mut file) = loop {
        let id = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".astracore-{}-{id}.tmp", std::process::id()));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => break (path, file),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };
    let result = (|| {
        serde_json::to_writer_pretty(&mut file, value).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        if replace {
            fs::rename(&temp, path)
        } else {
            fs::hard_link(&temp, path)
        }
    })();
    let _ = fs::remove_file(&temp);
    if result.is_ok() {
        fs::File::open(parent)?.sync_all()?;
    }
    result
}

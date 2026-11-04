//! Establish private files before SQLite can consume credentials or sidecars.
use std::os::unix::{
    ffi::{OsStrExt, OsStringExt},
    fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
};
use std::{
    fs::{self, OpenOptions, Permissions},
    io,
    path::{Path, PathBuf},
};

/// Resolve the filesystem resource before opening the same SQLite path/URI.
pub(super) fn prepare(sqlite_path: &Path) -> io::Result<Option<PathBuf>> {
    let Some((path, create)) = storage_path(sqlite_path)? else {
        return Ok(None);
    };
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    if create {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    let metadata = fs::metadata(parent)?;
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o022 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "mail store requires an owned directory not writable by other users",
        ));
    }
    if !create {
        fs::symlink_metadata(&path)?;
    }
    private_file(&path, create)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = path.as_os_str().to_os_string();
        sidecar.push(suffix);
        private_file(Path::new(&sidecar), false)?;
    }
    // This subtree is owned by Store, unlike a caller's existing parent directory.
    let attachments = parent.join("attachments");
    match fs::symlink_metadata(&attachments) {
        Ok(metadata) => {
            if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "unsafe attachment directory",
                ));
            }
            fs::set_permissions(attachments, Permissions::from_mode(0o700))?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    Ok(Some(path))
}

fn storage_path(path: &Path) -> io::Result<Option<(PathBuf, bool)>> {
    if path.as_os_str().is_empty() || path == Path::new(":memory:") {
        return Ok(None);
    }
    if !path.as_os_str().as_bytes().starts_with(b"file:") {
        return Ok(Some((path.to_owned(), true)));
    }
    let uri = path
        .to_str()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "SQLite URI must be UTF-8"))?
        .strip_prefix("file:")
        .unwrap();
    let uri = uri.split('#').next().unwrap_or_default();
    let (name, query) = uri.split_once('?').unwrap_or((uri, ""));
    let mut mode = None;
    let mut cache = None;
    // Alternate VFS/locking options could make SQLite consume a different
    // resource than the one protected here. Reject them, never reinterpret.
    for item in query.split('&').filter(|p| !p.is_empty()) {
        let (key, value) = item.split_once('=').unwrap_or((item, ""));
        let decode = |s: &str| {
            percent_encoding::percent_decode_str(s)
                .decode_utf8()
                .map(|s| s.into_owned())
                .map_err(io::Error::other)
        };
        let (key, value) = (decode(key)?, decode(value)?);
        match key.as_str() {
            "mode" if mode.is_none() && ["ro", "rw", "rwc", "memory"].contains(&value.as_str()) => {
                mode = Some(value)
            }
            "cache" if cache.is_none() && ["shared", "private"].contains(&value.as_str()) => {
                cache = Some(value)
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "unsupported or duplicate SQLite URI option",
                ))
            }
        }
    }
    let name = if let Some(authority) = name.strip_prefix("//") {
        let (host, name) = authority.split_once('/').ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "SQLite file URI needs a path")
        })?;
        if host != "" && host != "localhost" {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "unsupported SQLite file URI host",
            ));
        }
        format!("/{name}")
    } else {
        name.to_owned()
    };
    let decoded = percent_encoding::percent_decode_str(&name).collect::<Vec<_>>();
    if decoded.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "NUL in SQLite file URI",
        ));
    }
    if mode.as_deref() == Some("memory") || decoded == b":memory:" || decoded.is_empty() {
        return Ok(None);
    }
    Ok(Some((
        PathBuf::from(std::ffi::OsString::from_vec(decoded)),
        !matches!(mode.as_deref(), Some("ro" | "rw")),
    )))
}

fn private_file(path: &Path, create: bool) -> io::Result<()> {
    let file = match OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if !create && e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "unsafe mail store file",
        ));
    }
    // Change the opened inode, never a possibly substituted pathname.
    file.set_permissions(Permissions::from_mode(0o600))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Store;

    #[test]
    fn sqlite_memory_temporary_and_file_uri_modes_are_preserved() {
        let root = std::env::temp_dir().join(format!("bang-uri-store-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let named = root.join("named-memory");
        let memory_uri = format!("file:{}?mode=memory&cache=shared", named.display());
        {
            let first = Store::open(Path::new(&memory_uri)).unwrap();
            first
                .conn
                .lock()
                .unwrap()
                .execute_batch(
                    "CREATE TABLE control(value TEXT); INSERT INTO control VALUES('shared');",
                )
                .unwrap();
            let second = Store::open(Path::new(&memory_uri)).unwrap();
            assert_eq!(
                second
                    .conn
                    .lock()
                    .unwrap()
                    .query_row("SELECT value FROM control", [], |r| r.get::<_, String>(0))
                    .unwrap(),
                "shared"
            );
            assert!(first.attachments_dir.is_none());
        }
        let empty = Store::open(Path::new(&memory_uri)).unwrap();
        assert!(empty
            .conn
            .lock()
            .unwrap()
            .prepare("SELECT * FROM control")
            .is_err());
        assert!(!named.exists());
        let temporary = Store::open(Path::new("")).unwrap();
        temporary
            .conn
            .lock()
            .unwrap()
            .execute_batch("CREATE TABLE control(value TEXT);")
            .unwrap();
        assert!(temporary.attachments_dir.is_none());
        let path = root.join("mail été + space.db");
        let mut file_uri = url::Url::from_file_path(&path).unwrap();
        file_uri.set_query(Some("mode=rwc"));
        {
            let store = Store::open(Path::new(file_uri.as_str())).unwrap();
            store
                .conn
                .lock()
                .unwrap()
                .execute_batch(
                    "CREATE TABLE control(value TEXT); INSERT INTO control VALUES('persistent');",
                )
                .unwrap();
            assert_eq!(store.attachments_dir, Some(root.join("attachments")));
            assert_private_files(&path);
        }
        file_uri.set_query(Some("mode=ro"));
        {
            let store = Store::open(Path::new(file_uri.as_str())).unwrap();
            assert_eq!(
                store
                    .conn
                    .lock()
                    .unwrap()
                    .query_row("SELECT value FROM control", [], |r| r.get::<_, String>(0))
                    .unwrap(),
                "persistent"
            );
            assert!(store
                .conn
                .lock()
                .unwrap()
                .execute("INSERT INTO control VALUES('wrong')", [])
                .is_err());
        }
        assert!(Store::open(Path::new("file:ignored?mode=memory&mode=rwc")).is_err());
        assert!(Store::open(Path::new("file:ignored?vfs=unix-none")).is_err());
        assert!(Store::open(Path::new("file:ignored%00name")).is_err());
        assert!(!Path::new("file:ignored%00name").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn store_permissions_cover_new_and_existing_sqlite_files() {
        const CHILD: &str = "BANG_PRIVATE_STORE_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            // umask is process-global: change it only in a dedicated child.
            let status = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "store::private_storage::tests::store_permissions_cover_new_and_existing_sqlite_files", "--nocapture"])
                .env(CHILD, "1").status().unwrap();
            assert!(status.success());
            return;
        }
        unsafe {
            libc::umask(0);
        }
        let root = std::env::temp_dir().join(format!("bang-private-store-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, Permissions::from_mode(0o755)).unwrap();
        let path = root.join("new/mail.db");
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(
                fs::metadata(path.parent().unwrap()).unwrap().mode() & 0o777,
                0o700
            );
            assert_private_files(&path);
            let stored = store
                .save_attachment(1, "note.txt", b"private mail")
                .unwrap();
            assert_eq!(
                fs::metadata(Path::new(&stored).parent().unwrap().parent().unwrap())
                    .unwrap()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        let legacy = root.join("legacy.db");
        let old = rusqlite::Connection::open(&legacy).unwrap();
        old.pragma_update(None, "journal_mode", "WAL").unwrap();
        old.execute_batch("CREATE TABLE legacy_control(value TEXT); INSERT INTO legacy_control VALUES('retained');").unwrap();
        for suffix in ["", "-wal", "-shm"] {
            let mut file = legacy.as_os_str().to_os_string();
            file.push(suffix);
            fs::set_permissions(Path::new(&file), Permissions::from_mode(0o666)).unwrap();
        }
        fs::create_dir(root.join("attachments")).unwrap();
        fs::set_permissions(root.join("attachments"), Permissions::from_mode(0o777)).unwrap();
        {
            let store = Store::open(&legacy).unwrap();
            assert_private_files(&legacy);
            assert_eq!(
                fs::metadata(root.join("attachments")).unwrap().mode() & 0o777,
                0o700
            );
            assert_eq!(
                store
                    .conn
                    .lock()
                    .unwrap()
                    .query_row("SELECT value FROM legacy_control", [], |r| r
                        .get::<_, String>(0))
                    .unwrap(),
                "retained"
            );
            assert_eq!(
                fs::metadata(&root).unwrap().mode() & 0o777,
                0o755,
                "do not chmod a caller's existing directory"
            );
        }
        drop(old);
        assert!(Store::open(Path::new(":memory:"))
            .unwrap()
            .attachments_dir
            .is_none());
        fs::remove_dir_all(root).unwrap();
    }

    fn assert_private_files(path: &Path) {
        for suffix in ["", "-wal", "-shm"] {
            let mut file = path.as_os_str().to_os_string();
            file.push(suffix);
            assert_eq!(
                fs::metadata(Path::new(&file)).unwrap().mode() & 0o777,
                0o600,
                "{file:?}"
            );
        }
    }

    #[test]
    fn store_rejects_symlinks_and_shared_writable_parents() {
        let root = std::env::temp_dir().join(format!("bang-private-paths-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let target = root.join("target");
        fs::write(&target, b"keep").unwrap();
        fs::set_permissions(&target, Permissions::from_mode(0o644)).unwrap();
        let db = root.join("mail.db");
        std::os::unix::fs::symlink(&target, &db).unwrap();
        assert!(prepare(&db).is_err());
        fs::remove_file(&db).unwrap();
        let mut wal = db.as_os_str().to_os_string();
        wal.push("-wal");
        std::os::unix::fs::symlink(&target, Path::new(&wal)).unwrap();
        assert!(prepare(&db).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"keep");
        assert_eq!(fs::metadata(&target).unwrap().mode() & 0o777, 0o644);
        fs::set_permissions(&root, Permissions::from_mode(0o777)).unwrap();
        assert!(prepare(&root.join("other.db")).is_err());
        assert_eq!(fs::metadata(&root).unwrap().mode() & 0o777, 0o777);
        fs::set_permissions(&root, Permissions::from_mode(0o700)).unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}

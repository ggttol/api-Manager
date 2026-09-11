use std::fs;
#[cfg(not(windows))]
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

#[cfg(windows)]
mod windows_acl {
    use std::ffi::c_void;
    use std::fs::File;
    use std::io;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use std::path::Path;
    use std::ptr;

    #[repr(C)]
    struct SecurityAttributes {
        length: u32,
        descriptor: *mut c_void,
        inherit_handle: i32,
    }

    #[link(name = "advapi32")]
    extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor: *const u16,
            revision: u32,
            output: *mut *mut c_void,
            size: *mut u32,
        ) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn CreateFileW(
            name: *const u16,
            access: u32,
            share: u32,
            security: *const SecurityAttributes,
            disposition: u32,
            attributes: u32,
            template: *mut c_void,
        ) -> *mut c_void;
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    pub fn open_private(path: &Path) -> io::Result<File> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let name = path.file_name().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Missing temporary file name")
        })?;
        // Canonicalizing the existing parent also supplies Windows' long-path prefix.
        let absolute = std::fs::canonicalize(parent)?.join(name);
        let mut path: Vec<u16> = absolute.as_os_str().encode_wide().collect();
        if path.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "File name contains NUL",
            ));
        }
        path.push(0);
        // Protect at creation, not afterward: a handle opened through an inherited
        // permissive DACL could otherwise retain access to subsequent secret writes.
        let sddl = (*b"D:P(A;;FA;;;OW)\0").map(u16::from);
        let mut descriptor = ptr::null_mut();
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                ptr::null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error());
        }
        let security = SecurityAttributes {
            length: std::mem::size_of::<SecurityAttributes>() as u32,
            descriptor,
            inherit_handle: 0,
        };
        let handle = unsafe {
            // GENERIC_WRITE, shared read/write/delete, CREATE_NEW, FILE_ATTRIBUTE_NORMAL.
            CreateFileW(
                path.as_ptr(),
                0x4000_0000,
                7,
                &security,
                1,
                0x80,
                ptr::null_mut(),
            )
        };
        let result = if handle == (-1isize) as *mut c_void {
            Err(io::Error::last_os_error())
        } else {
            Ok(unsafe { File::from_raw_handle(handle) })
        };
        unsafe {
            LocalFree(descriptor);
        }
        result
    }
}

static CLIENT_CONFIG_LOCK: Mutex<()> = Mutex::new(());
static TEMP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Serializes complete client configuration transactions within this process.
pub fn lock_client_config() -> MutexGuard<'static, ()> {
    CLIENT_CONFIG_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Atomically replaces a credential file without a readable-by-others temporary.
/// Unix retains existing owner permission bits, never group/other access.
/// Windows installs a protected owner-only DACL at file creation.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("Cannot write {} without a parent directory", path.display()))?;
    fs::create_dir_all(parent)
        .map_err(|error| format!("Failed to create {}: {error}", parent.display()))?;

    #[cfg(unix)]
    let permissions = path
        .metadata()
        .ok()
        .map(|metadata| fs::Permissions::from_mode(metadata.permissions().mode() & 0o700));

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Invalid file name: {}", path.display()))?;

    for _ in 0..128 {
        let unique = TEMP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".{file_name}.antigravity-{}-{unique}.tmp",
            std::process::id()
        ));
        #[cfg(not(windows))]
        let opened = {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            options.open(&temporary)
        };
        #[cfg(windows)]
        let opened = windows_acl::open_private(&temporary);

        let mut file = match opened {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Failed to create temporary file: {error}")),
        };

        if let Err(error) = file
            .write_all(bytes)
            .map_err(|error| format!("Failed to write temporary file: {error}"))
        {
            drop(file);
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        if let Err(error) = file
            .sync_all()
            .map_err(|error| format!("Failed to sync temporary file: {error}"))
        {
            drop(file);
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        #[cfg(unix)]
        if let Some(permissions) = permissions.as_ref() {
            if let Err(error) = file
                .set_permissions(permissions.clone())
                .map_err(|error| format!("Failed to preserve file permissions: {error}"))
            {
                drop(file);
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
        }
        drop(file);
        if let Err(error) = fs::rename(&temporary, path)
            .map_err(|error| format!("Failed to replace {}: {error}", path.display()))
        {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        #[cfg(unix)]
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("Failed to sync {}: {error}", parent.display()))?;
        return Ok(());
    }

    Err(format!(
        "Failed to allocate a unique temporary file for {}",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::write_atomic;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn replacement_keeps_owner_only_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth.json");
        fs::write(&path, "old-secret").unwrap();
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        write_atomic(&path, b"new-secret").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new-secret");
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn concurrent_replacements_never_publish_partial_bytes() {
        use std::sync::{Arc, Barrier};
        use std::thread;

        let dir = tempfile::tempdir().unwrap();
        let path = Arc::new(dir.path().join("config.json"));
        let barrier = Arc::new(Barrier::new(3));
        let left = vec![b'a'; 32 * 1024];
        let right = vec![b'b'; 32 * 1024];

        let first_path = Arc::clone(&path);
        let first_barrier = Arc::clone(&barrier);
        let first = thread::spawn(move || {
            first_barrier.wait();
            write_atomic(&first_path, &left)
        });
        let second_path = Arc::clone(&path);
        let second_barrier = Arc::clone(&barrier);
        let second = thread::spawn(move || {
            second_barrier.wait();
            write_atomic(&second_path, &right)
        });
        barrier.wait();
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();

        let content = fs::read(path.as_ref()).unwrap();
        assert!(
            content.iter().all(|byte| *byte == b'a') || content.iter().all(|byte| *byte == b'b')
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}

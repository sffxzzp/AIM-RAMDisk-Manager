use crate::profiles::DiskProfile;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(windows)]
use std::time::{Duration, Instant};
#[cfg(windows)]
const FORMAT_TIMEOUT: Duration = Duration::from_secs(45);
#[cfg(windows)]
const FORMAT_POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Clone, Copy, Debug)]
pub struct MemorySnapshot {
    pub total_physical_bytes: u64,
    pub available_physical_bytes: u64,
    pub total_commit_limit_bytes: u64,
    pub available_commit_bytes: u64,
}

impl MemorySnapshot {
    pub fn used_physical_bytes(self) -> u64 {
        self.total_physical_bytes
            .saturating_sub(self.available_physical_bytes)
    }

    pub fn used_fraction(self) -> f32 {
        if self.total_physical_bytes == 0 {
            0.0
        } else {
            self.used_physical_bytes() as f32 / self.total_physical_bytes as f32
        }
    }
}

pub fn image_file_size_bytes(path: &str) -> Result<u64, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| localized_format!("msg_0597", path = path, error = error))?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(localized_format!("msg_0598", path = path));
    }
    Ok(metadata.len())
}

pub fn sync_directory_to_mount(source: &Path, mount_root: &Path) -> Result<(), String> {
    let source = validate_persistent_sync_directory(source)?;
    ensure_disjoint_paths(&source, mount_root)?;
    copy_directory_contents(&source, mount_root)
        .map_err(|error| localized_format!("msg_0599", error = error))
}

pub fn sync_mount_to_directory(mount_root: &Path, destination: &Path) -> Result<(), String> {
    let destination = validate_persistent_sync_directory(destination)?;
    ensure_disjoint_paths(mount_root, &destination)?;

    let stage = unique_sibling_path(&destination, "sync");
    let backup = unique_sibling_path(&destination, "previous");
    fs::create_dir(&stage).map_err(|error| localized_format!("msg_0602", error = error))?;
    let staged_copy = copy_directory_contents(mount_root, &stage);
    if let Err(error) = staged_copy {
        let _ = fs::remove_dir_all(&stage);
        return Err(localized_format!("msg_0599", error = error));
    }

    if let Err(error) = fs::rename(&destination, &backup) {
        let _ = fs::remove_dir_all(&stage);
        return Err(localized_format!("msg_0603", error = error));
    }
    if let Err(error) = fs::rename(&stage, &destination) {
        let rollback = fs::rename(&backup, &destination);
        let _ = fs::remove_dir_all(&stage);
        return match rollback {
            Ok(()) => Err(localized_format!("msg_0604", error = error)),
            Err(rollback_error) => Err(localized_format!(
                "msg_0605",
                error = error,
                rollback = rollback_error,
                backup = backup.display()
            )),
        };
    }
    let _ = fs::remove_dir_all(backup);
    Ok(())
}

#[cfg(windows)]
pub fn flush_mounted_volume(mount_path: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FlushFileBuffers, GetVolumeNameForVolumeMountPointW, FILE_SHARE_READ,
        FILE_SHARE_WRITE, OPEN_EXISTING,
    };

    let trimmed = mount_path.trim_end_matches(['\\', '/']);
    let volume_device = if trimmed.len() == 2 && trimmed.as_bytes()[1] == b':' {
        format!("\\\\.\\{}", trimmed)
    } else {
        let mount_root: Vec<u16> = format!("{}\\", trimmed)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut volume_name = vec![0_u16; 128];
        if unsafe {
            GetVolumeNameForVolumeMountPointW(
                mount_root.as_ptr(),
                volume_name.as_mut_ptr(),
                volume_name.len() as u32,
            )
        } == 0
        {
            return Err(localized_format!(
                "msg_0635",
                error = std::io::Error::last_os_error()
            ));
        }
        let length = volume_name
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(volume_name.len());
        let guid_path = String::from_utf16_lossy(&volume_name[..length]);
        let Some(device_name) = guid_path.strip_prefix("\\\\?\\") else {
            return Err(localized_text!("msg_0636").to_owned());
        };
        format!("\\\\.\\{}", device_name.trim_end_matches('\\'))
    };
    let volume_wide: Vec<u16> = std::ffi::OsStr::new(&volume_device)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let handle = unsafe {
        CreateFileW(
            volume_wide.as_ptr(),
            0x8000_0000 | 0x4000_0000,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if handle.is_null() || handle == INVALID_HANDLE_VALUE {
        return Err(localized_format!(
            "msg_0635",
            error = std::io::Error::last_os_error()
        ));
    }
    let result = if unsafe { FlushFileBuffers(handle) } == 0 {
        Err(localized_format!(
            "msg_0635",
            error = std::io::Error::last_os_error()
        ))
    } else {
        Ok(())
    };
    unsafe { CloseHandle(handle) };
    result
}

#[cfg(not(windows))]
pub fn flush_mounted_volume(_mount_path: &str) -> Result<(), String> {
    Err(localized_text!("msg_0513").to_owned())
}

fn unique_sibling_path(path: &Path, purpose: &str) -> PathBuf {
    static NEXT_SYNC_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT_SYNC_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let file_name = path.file_name().unwrap_or_default().to_string_lossy();
    path.with_file_name(format!(
        ".{file_name}.aimtoolkit-{purpose}-{}-{stamp:x}-{sequence:x}",
        std::process::id()
    ))
}

fn ensure_disjoint_paths(left: &Path, right: &Path) -> Result<(), String> {
    let left = fs::canonicalize(left)
        .map_err(|error| localized_format!("msg_0606", path = left.display(), error = error))?;
    let right = fs::canonicalize(right)
        .map_err(|error| localized_format!("msg_0606", path = right.display(), error = error))?;
    let left = normalized_path_string(&left);
    let right = normalized_path_string(&right);
    if path_is_within(&left, &right) || path_is_within(&right, &left) {
        return Err(localized_text!("msg_0607").to_owned());
    }
    Ok(())
}

fn normalized_path_string(path: &Path) -> String {
    path.to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

fn path_is_within(path: &str, root: &str) -> bool {
    path == root
        || path
            .strip_prefix(root)
            .is_some_and(|suffix| suffix.starts_with('\\'))
}

fn copy_directory_contents(source: &Path, destination: &Path) -> std::io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let name = entry.file_name();
        if is_windows_system_sync_directory(&name) {
            continue;
        }
        ensure_not_reparse_point(&source_path)?;
        let destination_path = destination.join(&name);
        let metadata = fs::metadata(&source_path)?;
        if metadata.is_dir() {
            fs::create_dir_all(&destination_path)?;
            copy_directory_contents(&source_path, &destination_path)?;
            fs::set_permissions(&destination_path, metadata.permissions())?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &destination_path)?;
            File::open(&destination_path)?.sync_all()?;
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                localized_text!("msg_0608"),
            ));
        }
    }
    Ok(())
}

fn is_windows_system_sync_directory(name: &std::ffi::OsStr) -> bool {
    name.to_string_lossy()
        .eq_ignore_ascii_case("System Volume Information")
        || name.to_string_lossy().eq_ignore_ascii_case("$RECYCLE.BIN")
}

#[cfg(windows)]
fn ensure_not_reparse_point(path: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::{
        GetFileAttributesW, FILE_ATTRIBUTE_REPARSE_POINT, INVALID_FILE_ATTRIBUTES,
    };

    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let attributes = unsafe { GetFileAttributesW(path_wide.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES {
        return Err(std::io::Error::from_raw_os_error(
            unsafe { GetLastError() } as i32
        ));
    }
    if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            localized_text!("msg_0609"),
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
fn ensure_not_reparse_point(path: &Path) -> std::io::Result<()> {
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            localized_text!("msg_0609"),
        ))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
pub fn memory_snapshot() -> Result<MemorySnapshot, String> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    let ok = unsafe { GlobalMemoryStatusEx(&mut status) };
    if ok == 0 {
        return Err(localized_format!(
            "msg_0001",
            std::io::Error::last_os_error()
        ));
    }
    Ok(MemorySnapshot {
        total_physical_bytes: status.ullTotalPhys,
        available_physical_bytes: status.ullAvailPhys,
        total_commit_limit_bytes: status.ullTotalPageFile,
        available_commit_bytes: status.ullAvailPageFile,
    })
}

#[cfg(not(windows))]
pub fn memory_snapshot() -> Result<MemorySnapshot, String> {
    Err(localized_text!("msg_0386").to_owned())
}

pub fn require_x64_pe(path: &Path, component: &str) -> Result<(), String> {
    let mut file = File::open(path).map_err(|error| {
        localized_format!(
            "msg_0274",
            path.display(),
            component = component,
            error = error
        )
    })?;
    let file_length = file
        .metadata()
        .map_err(|error| localized_format!("msg_0340", component = component, error = error))?
        .len();
    if file_length < 64 {
        return Err(localized_format!("msg_0552", component = component));
    }

    let mut dos_header = [0_u8; 64];
    file.read_exact(&mut dos_header)
        .map_err(|error| localized_format!("msg_0428", component = component, error = error))?;
    if &dos_header[..2] != b"MZ" {
        return Err(localized_format!("msg_0552", component = component));
    }

    let pe_offset = u32::from_le_bytes([
        dos_header[0x3C],
        dos_header[0x3D],
        dos_header[0x3E],
        dos_header[0x3F],
    ]) as u64;
    if pe_offset.checked_add(6).is_none_or(|end| end > file_length) {
        return Err(localized_format!("msg_0035", component = component));
    }
    file.seek(SeekFrom::Start(pe_offset))
        .map_err(|error| localized_format!("msg_0030", component = component, error = error))?;

    let mut pe_header = [0_u8; 6];
    file.read_exact(&mut pe_header)
        .map_err(|error| localized_format!("msg_0556", component = component, error = error))?;
    if &pe_header[..4] != b"PE\0\0" {
        return Err(localized_format!("msg_0317", component = component));
    }
    let machine = u16::from_le_bytes([pe_header[4], pe_header[5]]);
    if machine != 0x8664 {
        return Err(localized_format!(
            "msg_0433",
            component = component,
            machine = machine
        ));
    }
    Ok(())
}

#[cfg(windows)]
pub fn available_drive_letters() -> Vec<char> {
    use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;

    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return Vec::new();
    }
    ('A'..='Z')
        .filter(|letter| {
            let bit = (*letter as u8 - b'A') as u32;
            mask & (1_u32 << bit) == 0
        })
        .collect()
}

#[cfg(not(windows))]
pub fn available_drive_letters() -> Vec<char> {
    ('A'..='Z').collect()
}

#[cfg(windows)]
pub fn drive_letter_is_available(letter: char) -> Result<bool, String> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;

    if !letter.is_ascii_alphabetic() {
        return Ok(false);
    }
    let index = letter.to_ascii_uppercase() as u8 - b'A';
    let mask = unsafe { GetLogicalDrives() };
    if mask == 0 {
        return Err(localized_format!("msg_0056", unsafe { GetLastError() }));
    }
    Ok(mask & (1_u32 << index) == 0)
}

#[cfg(not(windows))]
pub fn drive_letter_is_available(_letter: char) -> Result<bool, String> {
    Ok(true)
}

pub fn mount_target_is_available(profile: &DiskProfile) -> Result<bool, String> {
    match profile.mount_directory.as_deref() {
        None => drive_letter_is_available(profile.normalized_drive_letter()),
        Some(directory) => directory_mount_target_is_available(directory),
    }
}

pub fn ensure_volume_mount_target(volume_guid: &str, mount_target: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        ensure_volume_mount_target_windows(volume_guid, mount_target)
    }

    #[cfg(not(windows))]
    {
        let _ = (volume_guid, mount_target);
        Err(localized_text!("msg_0485").to_owned())
    }
}

#[cfg(windows)]
fn ensure_volume_mount_target_windows(volume_guid: &str, mount_target: &str) -> Result<(), String> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::{DeleteVolumeMountPointW, SetVolumeMountPointW};

    let target_path = normalize_volume_mount_path(mount_target);
    let volume_path = format!("{}\\", volume_guid.trim_end_matches(['\\', '/']));
    let mut mount_paths = volume_mount_paths(&volume_path)?;

    let target_is_mounted = mount_paths
        .iter()
        .any(|path| path.eq_ignore_ascii_case(&target_path));
    if !target_is_mounted {
        let target_available = if is_drive_letter_mount_path(&target_path) {
            let letter = target_path.chars().next().unwrap_or_default();
            drive_letter_is_available(letter)?
        } else {
            directory_mount_target_is_available(mount_target)?
        };
        if !target_available {
            return Err(localized_format!("msg_0653", target = mount_target));
        }

        let target_wide: Vec<u16> = target_path.encode_utf16().chain(Some(0)).collect();
        let volume_wide: Vec<u16> = volume_path.encode_utf16().chain(Some(0)).collect();

        // Windows/AIM may have auto-assigned a temporary drive letter (for
        // example G:\) before this reconciliation runs. Release that letter
        // before assigning another drive letter: SetVolumeMountPointW can
        // reject the same volume with ERROR_INVALID_PARAMETER while its old
        // letter is still attached.
        let mut released_mounts = Vec::new();
        if is_drive_letter_mount_path(&target_path) {
            for mount_path in &mount_paths {
                if !is_drive_letter_mount_path(mount_path)
                    || mount_path.eq_ignore_ascii_case(&target_path)
                {
                    continue;
                }
                let mount_path_wide: Vec<u16> = mount_path.encode_utf16().chain(Some(0)).collect();
                if unsafe { DeleteVolumeMountPointW(mount_path_wide.as_ptr()) } == 0 {
                    let error = unsafe { GetLastError() };
                    let mut message = localized_format!(
                        "msg_0655",
                        mount_point = mount_path,
                        error = std::io::Error::from_raw_os_error(error as i32)
                    );
                    for (released_path, rollback_error) in
                        restore_volume_mount_paths(&volume_path, &released_mounts)
                    {
                        message.push(' ');
                        message.push_str(&localized_format!(
                            "msg_0658",
                            mount_point = released_path,
                            error = std::io::Error::from_raw_os_error(rollback_error as i32)
                        ));
                    }
                    return Err(message);
                }
                released_mounts.push(mount_path.clone());
            }
        }

        if unsafe { SetVolumeMountPointW(target_wide.as_ptr(), volume_wide.as_ptr()) } == 0 {
            let error = unsafe { GetLastError() };
            let mut message = localized_format!(
                "msg_0654",
                target = mount_target,
                error = std::io::Error::from_raw_os_error(error as i32)
            );
            for (released_path, rollback_error) in
                restore_volume_mount_paths(&volume_path, &released_mounts)
            {
                message.push(' ');
                message.push_str(&localized_format!(
                    "msg_0658",
                    mount_point = released_path,
                    error = std::io::Error::from_raw_os_error(rollback_error as i32)
                ));
            }
            return Err(message);
        }
        mount_paths = volume_mount_paths(&volume_path)?;
        if !mount_paths
            .iter()
            .any(|path| path.eq_ignore_ascii_case(&target_path))
        {
            return Err(localized_format!(
                "msg_0654",
                target = mount_target,
                error = localized_text!("msg_0306")
            ));
        }
    }

    for mount_path in mount_paths {
        if !is_drive_letter_mount_path(&mount_path) || mount_path.eq_ignore_ascii_case(&target_path)
        {
            continue;
        }
        let mount_path_wide: Vec<u16> = mount_path.encode_utf16().chain(Some(0)).collect();
        if unsafe { DeleteVolumeMountPointW(mount_path_wide.as_ptr()) } == 0 {
            return Err(localized_format!(
                "msg_0655",
                mount_point = mount_path,
                error = std::io::Error::from_raw_os_error(unsafe { GetLastError() } as i32)
            ));
        }
    }

    Ok(())
}

#[cfg(windows)]
fn restore_volume_mount_paths(volume_path: &str, mount_paths: &[String]) -> Vec<(String, u32)> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::SetVolumeMountPointW;

    let volume_wide: Vec<u16> = volume_path.encode_utf16().chain(Some(0)).collect();
    let mut failures = Vec::new();
    for mount_path in mount_paths {
        let is_already_mounted = volume_mount_paths(volume_path)
            .map(|paths| {
                paths
                    .iter()
                    .any(|path| path.eq_ignore_ascii_case(mount_path))
            })
            .unwrap_or(false);
        if is_already_mounted {
            continue;
        }

        let mount_wide: Vec<u16> = mount_path.encode_utf16().chain(Some(0)).collect();
        if unsafe { SetVolumeMountPointW(mount_wide.as_ptr(), volume_wide.as_ptr()) } == 0 {
            failures.push((mount_path.clone(), unsafe { GetLastError() }));
        }
    }
    failures
}

#[cfg(windows)]
fn volume_mount_paths(volume_path: &str) -> Result<Vec<String>, String> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::GetVolumePathNamesForVolumeNameW;

    let volume_wide: Vec<u16> = volume_path.encode_utf16().chain(Some(0)).collect();
    let mut capacity = 256_usize;
    loop {
        let mut buffer = vec![0_u16; capacity];
        let mut returned = 0_u32;
        let ok = unsafe {
            GetVolumePathNamesForVolumeNameW(
                volume_wide.as_ptr(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                &mut returned,
            )
        };
        if ok != 0 {
            let length = (returned as usize).min(buffer.len());
            let mut paths = Vec::new();
            let mut start = 0;
            while start < length {
                let Some(relative_end) = buffer[start..length].iter().position(|unit| *unit == 0)
                else {
                    break;
                };
                if relative_end == 0 {
                    break;
                }
                let end = start + relative_end;
                paths.push(String::from_utf16_lossy(&buffer[start..end]));
                start = end + 1;
            }
            return Ok(paths);
        }

        let error = unsafe { GetLastError() };
        if error == 234 && capacity < 32_768 {
            capacity = (returned as usize)
                .saturating_add(1)
                .max(capacity.saturating_mul(2))
                .min(32_768);
            continue;
        }
        return Err(localized_format!(
            "msg_0657",
            volume = volume_path,
            error = std::io::Error::from_raw_os_error(error as i32)
        ));
    }
}

#[cfg(windows)]
fn normalize_volume_mount_path(mount_target: &str) -> String {
    let trimmed = mount_target.trim_end_matches(['\\', '/']);
    if trimmed.len() == 2
        && trimmed.as_bytes()[1] == b':'
        && trimmed.as_bytes()[0].is_ascii_alphabetic()
    {
        format!("{}\\", trimmed.to_ascii_uppercase())
    } else {
        format!("{trimmed}\\")
    }
}

#[cfg(windows)]
fn is_drive_letter_mount_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() == 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\'
}

pub fn validate_sync_directory_source(source: &str, profile: &DiskProfile) -> Result<(), String> {
    let source_path = Path::new(source);
    let canonical_source = validate_persistent_sync_directory(source_path)?;

    match profile.mount_directory.as_deref() {
        Some(mount_directory) => {
            if Path::new(mount_directory).is_dir() {
                ensure_disjoint_paths(&canonical_source, Path::new(mount_directory))?;
            }
        }
        None => {
            let normalized_source = normalized_path_string(&canonical_source);
            let target_root = format!("{}:\\", profile.normalized_drive_letter());
            if normalized_source.starts_with(&target_root.to_lowercase()) {
                return Err(localized_text!("msg_0607").to_owned());
            }
        }
    }
    Ok(())
}

fn validate_persistent_sync_directory(path: &Path) -> Result<PathBuf, String> {
    if !path.is_dir() {
        return Err(localized_format!("msg_0610", path = path.display()));
    }
    ensure_not_reparse_point(path)
        .map_err(|error| localized_format!("msg_0611", path = path.display(), error = error))?;
    let canonical_path = fs::canonicalize(path)
        .map_err(|error| localized_format!("msg_0612", path = path.display(), error = error))?;
    if canonical_path.parent().is_none() || canonical_path.file_name().is_none() {
        return Err(localized_text!("msg_0613").to_owned());
    }
    validate_persistent_ntfs_directory(&canonical_path)?;
    Ok(canonical_path)
}

#[cfg(windows)]
fn validate_persistent_ntfs_directory(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetFileAttributesW, GetVolumeInformationW, GetVolumePathNameW,
        FILE_ATTRIBUTE_DIRECTORY, INVALID_FILE_ATTRIBUTES,
    };

    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let attributes = unsafe { GetFileAttributesW(path_wide.as_ptr()) };
    if attributes == INVALID_FILE_ATTRIBUTES || attributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
        return Err(localized_format!("msg_0610", path = path.display()));
    }
    let path_with_separator: Vec<u16> = format!("{}\\", path.display())
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut volume_root = vec![0_u16; 32_768];
    if unsafe {
        GetVolumePathNameW(
            path_with_separator.as_ptr(),
            volume_root.as_mut_ptr(),
            volume_root.len() as u32,
        )
    } == 0
    {
        return Err(localized_format!(
            "msg_0614",
            error = unsafe { GetLastError() }
        ));
    }
    const DRIVE_FIXED: u32 = 3;
    if unsafe { GetDriveTypeW(volume_root.as_ptr()) } != DRIVE_FIXED {
        return Err(localized_text!("msg_0615").to_owned());
    }
    let mut file_system = [0_u16; 64];
    if unsafe {
        GetVolumeInformationW(
            volume_root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            file_system.as_mut_ptr(),
            file_system.len() as u32,
        )
    } == 0
    {
        return Err(localized_format!(
            "msg_0616",
            error = unsafe { GetLastError() }
        ));
    }
    let length = file_system
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(file_system.len());
    if !String::from_utf16_lossy(&file_system[..length]).eq_ignore_ascii_case("NTFS") {
        return Err(localized_text!("msg_0617").to_owned());
    }
    Ok(())
}

#[cfg(not(windows))]
fn validate_persistent_ntfs_directory(_path: &Path) -> Result<(), String> {
    Err(localized_text!("msg_0513").to_owned())
}

#[cfg(windows)]
fn directory_mount_target_is_available(directory: &str) -> Result<bool, String> {
    use windows_sys::Win32::Foundation::GetLastError;
    use windows_sys::Win32::Storage::FileSystem::{
        GetDriveTypeW, GetFileAttributesW, GetVolumeInformationW, GetVolumePathNameW,
        FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    };

    let directory = directory.trim_end_matches('\\');
    let path: Vec<u16> = directory.encode_utf16().chain(Some(0)).collect();
    let attributes = unsafe { GetFileAttributesW(path.as_ptr()) };
    if attributes == u32::MAX {
        let error = unsafe { GetLastError() };
        if error == windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND
            || error == windows_sys::Win32::Foundation::ERROR_PATH_NOT_FOUND
        {
            return Ok(false);
        }
        return Err(localized_format!(
            "msg_0338",
            directory = directory,
            error = error
        ));
    }
    if attributes & FILE_ATTRIBUTE_DIRECTORY == 0 || attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Ok(false);
    }

    let mut entries = match std::fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(localized_format!(
                "msg_0145",
                directory = directory,
                error = error
            ))
        }
    };
    match entries.next() {
        None => {}
        Some(Ok(_)) => return Ok(false),
        Some(Err(error)) => {
            return Err(localized_format!(
                "msg_0123",
                directory = directory,
                error = error
            ))
        }
    }

    let path_with_separator: Vec<u16> = format!("{directory}\\")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut volume_root = vec![0_u16; 32_768];
    if unsafe {
        GetVolumePathNameW(
            path_with_separator.as_ptr(),
            volume_root.as_mut_ptr(),
            volume_root.len() as u32,
        )
    } == 0
    {
        return Err(localized_format!(
            "msg_0392",
            unsafe { GetLastError() },
            directory = directory
        ));
    }
    let drive_type = unsafe { GetDriveTypeW(volume_root.as_ptr()) };
    const DRIVE_REMOVABLE: u32 = 2;
    const DRIVE_FIXED: u32 = 3;
    const DRIVE_RAMDISK: u32 = 6;
    if !matches!(drive_type, DRIVE_REMOVABLE | DRIVE_FIXED | DRIVE_RAMDISK) {
        return Ok(false);
    }
    let mut file_system = [0_u16; 64];
    if unsafe {
        GetVolumeInformationW(
            volume_root.as_ptr(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            file_system.as_mut_ptr(),
            file_system.len() as u32,
        )
    } == 0
    {
        return Err(localized_format!("msg_0545", unsafe { GetLastError() }));
    }
    let file_system_length = file_system
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(file_system.len());
    let file_system = String::from_utf16_lossy(&file_system[..file_system_length]);
    if !file_system.eq_ignore_ascii_case("NTFS") {
        return Ok(false);
    }

    Ok(true)
}

#[cfg(not(windows))]
fn directory_mount_target_is_available(_directory: &str) -> Result<bool, String> {
    Ok(false)
}

pub struct VolumeInformation {
    pub label: String,
    pub file_system: String,
}

#[cfg(windows)]
pub fn format_volume(
    volume_guid: &str,
    file_system: crate::profiles::FileSystem,
    temporary_label: &str,
) -> Result<(), String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::process::CommandExt;
    use std::process::Stdio;
    use std::thread;
    use windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW;

    let volume = volume_guid.trim_end_matches('\\');
    let label_argument = format!("/v:{temporary_label}");
    let mut system_directory = vec![0_u16; 32_768];
    let directory_length = unsafe {
        GetSystemDirectoryW(system_directory.as_mut_ptr(), system_directory.len() as u32)
    } as usize;
    if directory_length == 0 {
        return Err(localized_format!(
            "msg_0097",
            std::io::Error::last_os_error()
        ));
    }
    if directory_length >= system_directory.len() {
        return Err(localized_text!("msg_0174").to_owned());
    }
    let system_directory = OsString::from_wide(&system_directory[..directory_length]);
    let format_exe = PathBuf::from(system_directory).join("format.com");
    let mut child = std::process::Command::new(format_exe)
        .args([
            volume,
            file_system.format_argument(),
            "/q",
            "/y",
            &label_argument,
        ])
        .creation_flags(0x0800_0000)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| localized_format!("msg_0382", error = error))?;

    let stdout_reader = child.stdout.take().map(|mut stream| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stream.read_to_end(&mut bytes);
            (bytes, result)
        })
    });
    let stderr_reader = child.stderr.take().map(|mut stream| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            let result = stream.read_to_end(&mut bytes);
            (bytes, result)
        })
    });

    let deadline = Instant::now() + FORMAT_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(FORMAT_POLL_INTERVAL),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_format_output(stdout_reader);
                let _ = join_format_output(stderr_reader);
                return Err(localized_format!(
                    "msg_0376",
                    FORMAT_TIMEOUT.as_secs(),
                    volume = volume
                ));
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = join_format_output(stdout_reader);
                let _ = join_format_output(stderr_reader);
                return Err(localized_format!(
                    "msg_0370",
                    volume = volume,
                    error = error
                ));
            }
        }
    };
    let stdout = join_format_output(stdout_reader).unwrap_or_default();
    let stderr = join_format_output(stderr_reader).unwrap_or_default();
    if status.success() {
        Ok(())
    } else {
        let details = if stderr.is_empty() {
            String::from_utf8_lossy(&stdout)
        } else {
            String::from_utf8_lossy(&stderr)
        };
        let details = details.trim();
        if details.is_empty() {
            Err(localized_format!(
                "msg_0501",
                file_system.label(),
                status,
                volume = volume
            ))
        } else {
            Err(localized_format!(
                "msg_0561",
                file_system.label(),
                volume = volume,
                details = details
            ))
        }
    }
}

#[cfg(windows)]
fn join_format_output(
    reader: Option<std::thread::JoinHandle<(Vec<u8>, std::io::Result<usize>)>>,
) -> Option<Vec<u8>> {
    reader
        .and_then(|reader| reader.join().ok())
        .and_then(|(bytes, result)| result.ok().map(|_| bytes))
}

#[cfg(not(windows))]
pub fn format_volume(
    _volume_guid: &str,
    _file_system: crate::profiles::FileSystem,
    _temporary_label: &str,
) -> Result<(), String> {
    Err(localized_text!("msg_0342").to_owned())
}

#[cfg(windows)]
pub fn volume_information(root: &str) -> Result<VolumeInformation, String> {
    use windows_sys::Win32::Storage::FileSystem::GetVolumeInformationW;

    let root_wide: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
    let mut label = [0_u16; 256];
    let mut file_system = [0_u16; 64];
    let ok = unsafe {
        GetVolumeInformationW(
            root_wide.as_ptr(),
            label.as_mut_ptr(),
            label.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            file_system.as_mut_ptr(),
            file_system.len() as u32,
        )
    };
    if ok == 0 {
        return Err(localized_format!(
            "msg_0532",
            std::io::Error::last_os_error(),
            root = root
        ));
    }
    let label_length = label
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(label.len());
    let file_system_length = file_system
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(file_system.len());
    Ok(VolumeInformation {
        label: String::from_utf16_lossy(&label[..label_length]),
        file_system: String::from_utf16_lossy(&file_system[..file_system_length]),
    })
}

#[cfg(not(windows))]
pub fn volume_information(_root: &str) -> Result<VolumeInformation, String> {
    Err(localized_text!("msg_0513").to_owned())
}

#[cfg(windows)]
pub fn set_volume_label(root: &str, label: &str) -> Result<(), String> {
    use windows_sys::Win32::Storage::FileSystem::SetVolumeLabelW;

    let root_wide: Vec<u16> = root.encode_utf16().chain(Some(0)).collect();
    let label_wide: Vec<u16> = label.encode_utf16().chain(Some(0)).collect();
    let label_pointer = if label.is_empty() {
        std::ptr::null()
    } else {
        label_wide.as_ptr()
    };
    let ok = unsafe { SetVolumeLabelW(root_wide.as_ptr(), label_pointer) };
    if ok == 0 {
        return Err(localized_format!(
            "msg_0015",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn set_volume_label(_root: &str, _label: &str) -> Result<(), String> {
    Err(localized_text!("msg_0485").to_owned())
}

#[cfg(windows)]
pub fn large_page_minimum_bytes() -> Option<u64> {
    use windows_sys::Win32::System::Memory::GetLargePageMinimum;

    let minimum = unsafe { GetLargePageMinimum() };
    (minimum != 0).then_some(minimum as u64)
}

#[cfg(not(windows))]
pub fn large_page_minimum_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::{copy_directory_contents, ensure_disjoint_paths};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    struct TempTree(PathBuf);

    impl TempTree {
        fn new() -> Self {
            let stamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let path = std::env::temp_dir().join(format!(
                "aimtoolkit-system-test-{}-{stamp}",
                std::process::id()
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempTree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn directory_copy_includes_nested_files_and_skips_windows_system_folders() {
        let tree = TempTree::new();
        let source = tree.0.join("source");
        let destination = tree.0.join("destination");
        fs::create_dir_all(source.join("nested")).unwrap();
        fs::create_dir_all(source.join("$RECYCLE.BIN")).unwrap();
        fs::write(source.join("file.txt"), b"root").unwrap();
        fs::write(source.join("nested/child.txt"), b"nested").unwrap();
        fs::write(source.join("$RECYCLE.BIN/old.txt"), b"skip").unwrap();
        fs::create_dir_all(&destination).unwrap();

        copy_directory_contents(&source, &destination).unwrap_or_else(|error| {
            panic!(
                "copy failed: {error}; source entries: {:?}; destination entries: {:?}",
                fs::read_dir(&source)
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .collect::<Vec<_>>(),
                fs::read_dir(&destination)
                    .unwrap()
                    .map(|entry| entry.unwrap().file_name())
                    .collect::<Vec<_>>()
            )
        });

        assert_eq!(fs::read(destination.join("file.txt")).unwrap(), b"root");
        assert_eq!(
            fs::read(destination.join("nested/child.txt")).unwrap(),
            b"nested"
        );
        assert!(!destination.join("$RECYCLE.BIN").exists());
    }

    #[test]
    fn directory_copy_rejects_paths_that_overlap() {
        let tree = TempTree::new();
        let parent = tree.0.join("parent");
        let nested = parent.join("nested");
        let sibling = tree.0.join("parent-other");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir_all(&sibling).unwrap();

        assert!(ensure_disjoint_paths(&parent, &nested).is_err());
        assert!(ensure_disjoint_paths(&parent, &sibling).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn directory_copy_rejects_symbolic_links() {
        use std::os::unix::fs::symlink;

        let tree = TempTree::new();
        let source = tree.0.join("source");
        let destination = tree.0.join("destination");
        let outside = tree.0.join("outside.txt");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&destination).unwrap();
        fs::write(&outside, b"must not follow").unwrap();
        symlink(&outside, source.join("link.txt")).unwrap();

        assert!(copy_directory_contents(&source, &destination).is_err());
        assert!(!destination.join("link.txt").exists());
    }
}

#[cfg(windows)]
pub fn lock_memory_privilege_available() -> Result<bool, String> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, LUID};
    use windows_sys::Win32::Security::{GetTokenInformation, LookupPrivilegeValueW, TOKEN_QUERY};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

    const TOKEN_PRIVILEGES_CLASS: i32 = 3;
    const TOKEN_PRIVILEGES_HEADER_SIZE: usize = 4;
    const LUID_AND_ATTRIBUTES_SIZE: usize = 12;

    let mut token: HANDLE = std::ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(localized_format!("msg_0009", unsafe { GetLastError() }));
    }

    let result = (|| {
        let privilege_name: Vec<u16> = "SeLockMemoryPrivilege"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut expected_luid: LUID = unsafe { std::mem::zeroed() };
        if unsafe {
            LookupPrivilegeValueW(
                std::ptr::null(),
                privilege_name.as_ptr(),
                &mut expected_luid,
            )
        } == 0
        {
            return Err(localized_format!("msg_0294", unsafe { GetLastError() }));
        }

        let mut required = 0_u32;
        unsafe {
            GetTokenInformation(
                token,
                TOKEN_PRIVILEGES_CLASS,
                std::ptr::null_mut(),
                0,
                &mut required,
            );
        }
        if required < TOKEN_PRIVILEGES_HEADER_SIZE as u32 {
            return Err(localized_format!("msg_0242", unsafe { GetLastError() }));
        }
        let words =
            (required as usize + std::mem::size_of::<u64>() - 1) / std::mem::size_of::<u64>();
        let mut buffer = vec![0_u64; words];
        let ok = unsafe {
            GetTokenInformation(
                token,
                TOKEN_PRIVILEGES_CLASS,
                buffer.as_mut_ptr().cast(),
                required,
                &mut required,
            )
        };
        if ok == 0 {
            return Err(localized_format!("msg_0242", unsafe { GetLastError() }));
        }

        let bytes =
            unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), required as usize) };
        let count =
            u32::from_ne_bytes(bytes[0..4].try_into().expect("4-byte privilege count")) as usize;
        for index in 0..count {
            let offset = TOKEN_PRIVILEGES_HEADER_SIZE + index * LUID_AND_ATTRIBUTES_SIZE;
            if offset + LUID_AND_ATTRIBUTES_SIZE > bytes.len() {
                return Err(localized_text!("msg_0114").to_owned());
            }
            let low = u32::from_ne_bytes(
                bytes[offset..offset + 4]
                    .try_into()
                    .expect("4-byte LUID low"),
            );
            let high = i32::from_ne_bytes(
                bytes[offset + 4..offset + 8]
                    .try_into()
                    .expect("4-byte LUID high"),
            );
            if low == expected_luid.LowPart && high == expected_luid.HighPart {
                return Ok(true);
            }
        }
        Ok(false)
    })();

    unsafe { CloseHandle(token) };
    result
}

#[cfg(not(windows))]
pub fn lock_memory_privilege_available() -> Result<bool, String> {
    Err(localized_text!("msg_0504").to_owned())
}

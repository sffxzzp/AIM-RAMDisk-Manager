use crate::aim::{self, AimDevice, DeviceIdentity};
use crate::profiles::{DiskBackend, DiskProfile, MountTargetKey};
use crate::system;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const READY_TIMEOUT: Duration = Duration::from_secs(45);
const POLL_INTERVAL: Duration = Duration::from_millis(500);

#[derive(Clone, Copy)]
enum PersistenceKind {
    Directory,
    Image,
}

#[derive(Debug)]
pub struct CreateError {
    pub message: String,
    pub outcome_unknown: bool,
}

impl CreateError {
    fn known(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            outcome_unknown: false,
        }
    }

    fn unknown(message: impl Into<String>) -> Self {
        Self {
            message: localized_format!("msg_0431", message.into()),
            outcome_unknown: true,
        }
    }
}

impl From<String> for CreateError {
    fn from(message: String) -> Self {
        Self::known(message)
    }
}

#[derive(Clone, Debug)]
pub struct RuntimePaths {
    pub directory: PathBuf,
    pub ramdyn_exe: PathBuf,
    pub aimapi_backend_dll: PathBuf,
    pub aimapi_shim_dll: PathBuf,
}

impl RuntimePaths {
    pub fn discover() -> Self {
        let executable_directory = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(PathBuf::from))
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
        let adjacent = executable_directory.join("runtime");

        #[cfg(debug_assertions)]
        let directory = {
            let source_tree = option_env!("CARGO_MANIFEST_DIR")
                .map(|manifest_dir| PathBuf::from(manifest_dir).join("runtime"));
            let has_backend_api = |directory: &Path| {
                directory.join("aimapi-backend.dll").is_file()
            };
            if has_backend_api(&adjacent) {
                adjacent
            } else if let Some(source_tree) = source_tree {
                if has_backend_api(&source_tree) || !adjacent.exists() {
                    source_tree
                } else {
                    adjacent
                }
            } else {
                adjacent
            }
        };

        #[cfg(not(debug_assertions))]
        let directory = adjacent;

        Self {
            directory: directory.clone(),
            ramdyn_exe: directory.join("RamDyn.exe"),
            aimapi_backend_dll: directory.join("aimapi-backend.dll"),
            aimapi_shim_dll: directory.join("aimapi.dll"),
        }
    }

    pub fn validate_components(&self) -> Result<(), String> {
        self.validate_ramdyn()?;
        self.validate_aimapi()?;
        self.validate_aimapi_shim()
    }

    pub fn validate_ramdyn(&self) -> Result<(), String> {
        if !self.ramdyn_exe.is_file() {
            return Err(localized_format!("msg_0420", self.ramdyn_exe.display()));
        }
        system::require_x64_pe(&self.ramdyn_exe, "RamDyn.exe")
    }

    pub fn validate_aimapi(&self) -> Result<(), String> {
        if !self.aimapi_backend_dll.is_file() {
            return Err(localized_format!(
                "msg_0463",
                self.aimapi_backend_dll.display()
            ));
        }
        system::require_x64_pe(&self.aimapi_backend_dll, "aimapi-backend.dll")
    }

    pub fn validate_aimapi_shim(&self) -> Result<(), String> {
        if !self.aimapi_shim_dll.is_file() {
            return Err(localized_text!("msg_0618").to_owned());
        }
        system::require_x64_pe(&self.aimapi_shim_dll, "aimapi.dll")
    }
}

pub fn create_and_wait(
    profile: &DiskProfile,
    runtime: &RuntimePaths,
) -> Result<DeviceIdentity, CreateError> {
    profile.validate()?;
    let mount_target = profile.mount_target();
    match profile.backend {
        DiskBackend::RamDyn => runtime.validate_components()?,
        DiskBackend::AimVirtualMemory | DiskBackend::AimAweAlloc | DiskBackend::AimImageFile => {
            runtime.validate_aimapi()?
        }
    }
    if let Some(directory) = profile.sync_directory.as_deref() {
        system::validate_sync_directory_source(directory, profile)?;
    }
    if !system::mount_target_is_available(profile)? {
        return Err(CreateError::known(localized_format!(
            "msg_0177",
            profile.mount_target_display()
        )));
    }
    let before = aim::enumerate(&runtime.aimapi_backend_dll)?;
    if !before.inventory_complete() {
        return Err(CreateError::known(localized_text!("msg_0291")));
    }
    let selected_adapter_port = profile
        .adapter_port
        .or_else(|| before.preferred_adapter_port())
        .ok_or_else(|| CreateError::known(localized_text!("msg_0572")))?;
    if !before.adapter_ports.contains(&selected_adapter_port) {
        return Err(CreateError::known(localized_format!(
            "msg_0573",
            port = selected_adapter_port
        )));
    }
    let before_ids = before
        .devices
        .iter()
        .map(|device| (device.identity.port, device.identity.number))
        .collect::<HashSet<_>>();
    let correlation_label = creation_correlation_label();
    let expected_device_size = if profile.backend == DiskBackend::AimImageFile {
        let path = profile
            .image_file
            .as_deref()
            .ok_or_else(|| CreateError::known(localized_text!("msg_0589")))?;
        system::image_file_size_bytes(path)?
    } else {
        profile.expected_device_size_bytes()?
    };
    // Ask AIM to assign the requested target during creation. If Windows only
    // discovers the volume afterward and gives it a temporary drive letter,
    // the reconciliation below replaces that late automatic mount.
    let (mut child, expected_device_number) = match profile.backend {
        DiskBackend::RamDyn => {
            if profile.memory_mode.requires_lock_memory_privilege() {
                match system::lock_memory_privilege_available()? {
                    true => {}
                    false => {
                        return Err(CreateError::known(localized_text!("msg_0218").to_owned()))
                    }
                }
            }
            if profile.memory_mode == crate::profiles::MemoryMode::LargePage
                && system::large_page_minimum_bytes().is_none()
            {
                return Err(CreateError::known(localized_text!("msg_0269")));
            }
            let size_kib = profile.ramdyn_size_kib()?;
            (
                Some(launch(
                    profile,
                    runtime,
                    size_kib,
                    &correlation_label,
                    selected_adapter_port,
                )?),
                None,
            )
        }
        DiskBackend::AimVirtualMemory => {
            let memory = system::memory_snapshot()?;
            if expected_device_size > memory.available_commit_bytes {
                return Err(CreateError::known(localized_format!(
                    "msg_0232",
                    expected_device_size as f64 / (1024.0 * 1024.0 * 1024.0),
                    memory.available_commit_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
                )));
            }
            let device_number = aim::create_native_device(
                &runtime.aimapi_backend_dll,
                selected_adapter_port,
                expected_device_size,
                512,
                profile.removable_media,
                false,
                &mount_target,
            )
            .map_err(|error| CreateError {
                message: error.message,
                outcome_unknown: error.outcome_unknown,
            })?;
            (None, Some(device_number))
        }
        DiskBackend::AimAweAlloc => {
            let memory = system::memory_snapshot()?;
            if expected_device_size > memory.available_physical_bytes {
                return Err(CreateError::known(localized_format!(
                    "msg_0575",
                    needed = expected_device_size as f64 / (1024.0 * 1024.0 * 1024.0),
                    available = memory.available_physical_bytes as f64 / (1024.0 * 1024.0 * 1024.0)
                )));
            }
            let device_number = aim::create_native_device(
                &runtime.aimapi_backend_dll,
                selected_adapter_port,
                expected_device_size,
                512,
                profile.removable_media,
                true,
                &mount_target,
            )
            .map_err(|error| CreateError {
                message: error.message,
                outcome_unknown: error.outcome_unknown,
            })?;
            (None, Some(device_number))
        }
        DiskBackend::AimImageFile => {
            let image_file = profile
                .image_file
                .as_deref()
                .ok_or_else(|| CreateError::known(localized_text!("msg_0589")))?;
            let device_number = aim::create_image_device(
                &runtime.aimapi_backend_dll,
                selected_adapter_port,
                image_file,
                profile.removable_media,
                &mount_target,
            )
            .map_err(|error| CreateError {
                message: error.message,
                outcome_unknown: error.outcome_unknown,
            })?;
            (None, Some(device_number))
        }
    };
    let deadline = Instant::now() + READY_TIMEOUT;
    let mut readiness_detail = localized_format!("msg_0497", profile.mount_target_display());
    let mut native_volume_formatted = false;
    let mut mount_target_reconciled = false;
    let requested_label = profile.volume_label.trim();

    loop {
        if let Some(child) = child.as_mut() {
            let status = match child.try_wait() {
                Ok(status) => status,
                Err(error) => {
                    return Err(CreateError::unknown(localized_format!(
                        "msg_0210",
                        error = error
                    )));
                }
            };
            if let Some(status) = status {
                return Err(CreateError::unknown(localized_format!(
                    "msg_0253",
                    status = status
                )));
            }
        }

        match aim::enumerate(&runtime.aimapi_backend_dll) {
            Ok(snapshot) => {
                if !snapshot.inventory_complete() {
                    readiness_detail = snapshot
                        .adapter_scan_error
                        .clone()
                        .or_else(|| snapshot.mount_scan_error.clone())
                        .unwrap_or_else(|| localized_text!("msg_0306").to_owned());
                }
                if !mount_target_reconciled
                    && expected_device_number.is_some()
                    && matches!(
                        profile.backend,
                        DiskBackend::AimVirtualMemory
                            | DiskBackend::AimAweAlloc
                            | DiskBackend::AimImageFile
                    )
                {
                    let expected_number = expected_device_number.unwrap();
                    if let Some(device) = snapshot.devices.iter().find(|device| {
                        device.identity.port == selected_adapter_port
                            && device.identity.number == expected_number
                    }) {
                        let key = (device.identity.port, device.identity.number);
                        if before_ids.contains(&key) {
                            return Err(CreateError::unknown(localized_format!(
                                "msg_0484",
                                profile.mount_target_display()
                            )));
                        }
                        let flags = device.identity.flags;
                        let has_expected_type =
                            device_flags_match_backend(profile.backend, flags);
                        let size_matches = profile.backend == DiskBackend::AimImageFile
                            || device.identity.size_bytes == expected_device_size;
                        if !size_matches || device.identity.size_bytes == 0 || !has_expected_type {
                            return Err(CreateError::unknown(localized_format!(
                                "msg_0248",
                                profile.mount_target_display(),
                                device.identity.size_bytes,
                                flags
                            )));
                        }

                        let volume_guid = match profile.backend {
                            DiskBackend::AimVirtualMemory | DiskBackend::AimAweAlloc
                                if snapshot.adapter_scan_complete =>
                            {
                                single_volume_guid_for_device(device)
                            }
                            DiskBackend::AimImageFile
                                if snapshot.inventory_complete() && device.can_unmount() =>
                            {
                                device.identity.volume_guid.as_deref()
                            }
                            _ => None,
                        };
                        if let Some(volume_guid) = volume_guid {
                            let mount_points = if device.mount_points.is_empty() {
                                localized_text!("msg_0368").to_owned()
                            } else {
                                device
                                    .mount_points
                                    .iter()
                                    .map(|mount| mount.path.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            };
                            system::ensure_volume_mount_target(
                                volume_guid,
                                &mount_target,
                            )
                            .map_err(|error| {
                                CreateError::unknown(localized_format!(
                                    "msg_0656",
                                    mount_points = mount_points,
                                    target = profile.mount_target_display(),
                                    error = error
                                ))
                            })?;
                            mount_target_reconciled = true;
                        } else {
                            readiness_detail = snapshot
                                .adapter_scan_error
                                .clone()
                                .or_else(|| snapshot.mount_scan_error.clone())
                                .unwrap_or_else(|| localized_text!("msg_0096").to_owned());
                        }
                    }
                }

                if let Some((device, target_mount)) = snapshot.devices.iter().find_map(|device| {
                    device
                        .mount_points
                        .iter()
                        .find(|mount| {
                            MountTargetKey::from_mount_path(&mount.path)
                                == profile.mount_target_key()
                        })
                        .map(|mount| (device, mount))
                }) {
                    if device.identity.port != selected_adapter_port {
                        return Err(CreateError::unknown(localized_format!(
                            "msg_0576",
                            selected = selected_adapter_port,
                            actual = device.identity.port
                        )));
                    }
                    let key = (device.identity.port, device.identity.number);
                    if before_ids.contains(&key) {
                        return Err(CreateError::unknown(localized_format!(
                            "msg_0484",
                            profile.mount_target_display()
                        )));
                    }
                    if expected_device_number.is_some_and(|number| number != device.identity.number)
                    {
                        return Err(CreateError::unknown(localized_format!(
                            "msg_0153",
                            profile.mount_target_display()
                        )));
                    }
                    let flags = device.identity.flags;
                    let has_expected_type =
                        device_flags_match_backend(profile.backend, device.identity.flags);
                    let size_matches = profile.backend == DiskBackend::AimImageFile
                        || device.identity.size_bytes == expected_device_size;
                    if !size_matches || device.identity.size_bytes == 0 || !has_expected_type {
                        return Err(CreateError::unknown(localized_format!(
                            "msg_0248",
                            profile.mount_target_display(),
                            device.identity.size_bytes,
                            flags
                        )));
                    }
                    let created_volume_guid = match profile.backend {
                        DiskBackend::AimVirtualMemory
                        | DiskBackend::AimAweAlloc
                        | DiskBackend::RamDyn
                            if snapshot.adapter_scan_complete =>
                        {
                            single_volume_guid_for_device(device)
                        }
                        _ => None,
                    };
                    let unique_volume_matches = match profile.backend {
                        DiskBackend::AimVirtualMemory
                        | DiskBackend::AimAweAlloc
                        | DiskBackend::RamDyn => created_volume_guid.is_some_and(|guid| {
                            target_mount
                                .volume_guid
                                .as_deref()
                                .is_some_and(|mount_guid| mount_guid.eq_ignore_ascii_case(guid))
                        }),
                        DiskBackend::AimImageFile => device.volume_guids.len() == 1
                            && device.identity.volume_guid.as_deref().is_some_and(|guid| {
                                device.volume_guids[0].eq_ignore_ascii_case(guid)
                                    && target_mount
                                        .volume_guid
                                        .as_deref()
                                        .is_some_and(|mount_guid| {
                                            mount_guid.eq_ignore_ascii_case(guid)
                                        })
                            }),
                    };
                    let topology_ready = match profile.backend {
                        DiskBackend::AimVirtualMemory
                        | DiskBackend::AimAweAlloc
                        | DiskBackend::RamDyn => created_volume_guid.is_some(),
                        DiskBackend::AimImageFile => {
                            snapshot.inventory_complete() && device.mount_scan_complete
                        }
                    };
                    if !topology_ready {
                        readiness_detail = snapshot
                            .adapter_scan_error
                            .clone()
                            .or_else(|| snapshot.mount_scan_error.clone())
                            .unwrap_or_else(|| localized_text!("msg_0096").to_owned());
                    } else if profile.backend == DiskBackend::AimImageFile
                        && device.volume_guids.len() != 1
                    {
                        return Err(CreateError::unknown(localized_format!(
                            "msg_0647",
                            count = device.volume_guids.len()
                        )));
                    } else if !unique_volume_matches {
                        readiness_detail = localized_text!("msg_0082").to_owned();
                    } else if target_mount.aim_device_count != 1 || target_mount.extent_count != 1 {
                        readiness_detail = localized_format!(
                            "msg_0331",
                            target_mount.aim_device_count,
                            target_mount.extent_count
                        );
                    } else {
                        let volume_guid = created_volume_guid
                            .or(device.identity.volume_guid.as_deref())
                            .ok_or_else(|| CreateError::unknown(localized_text!("msg_0502")))?;
                        if matches!(
                            profile.backend,
                            DiskBackend::AimVirtualMemory | DiskBackend::AimAweAlloc
                        ) && !native_volume_formatted
                        {
                            system::format_volume(
                                volume_guid,
                                profile.file_system,
                                &correlation_label,
                            )
                            .map_err(|error| {
                                CreateError::unknown(localized_format!("msg_0401", error = error))
                            })?;
                            native_volume_formatted = true;
                        }
                        if profile.backend == DiskBackend::AimImageFile {
                            match system::volume_information(volume_guid) {
                                Ok(volume)
                                    if !volume
                                        .file_system
                                        .eq_ignore_ascii_case(profile.file_system.label()) =>
                                {
                                    readiness_detail = localized_format!(
                                        "msg_0536",
                                        volume.file_system,
                                        profile.file_system.label()
                                    );
                                }
                                Ok(_) => return Ok(device.identity.clone()),
                                Err(error) => {
                                    readiness_detail = localized_format!("msg_0336", error = error)
                                }
                            }
                        } else {
                            match system::volume_information(volume_guid) {
                                Ok(volume)
                                    if !volume
                                        .file_system
                                        .eq_ignore_ascii_case(profile.file_system.label()) =>
                                {
                                    readiness_detail = localized_format!(
                                        "msg_0536",
                                        volume.file_system,
                                        profile.file_system.label()
                                    );
                                }
                                Ok(volume)
                                    if !volume.label.eq_ignore_ascii_case(&correlation_label) =>
                                {
                                    readiness_detail = localized_format!(
                                        "msg_0303",
                                        volume.label,
                                        correlation_label
                                    );
                                }
                                Ok(_) => {
                                    if let Err(error) =
                                        system::set_volume_label(volume_guid, requested_label)
                                    {
                                        return Err(CreateError::unknown(localized_format!(
                                            "msg_0347",
                                            correlation_label = correlation_label,
                                            requested_label = requested_label,
                                            error = error
                                        )));
                                    }
                                    match system::volume_information(volume_guid) {
                                        Ok(volume)
                                            if volume.file_system.eq_ignore_ascii_case(
                                                profile.file_system.label(),
                                            ) && volume
                                                .label
                                                .eq_ignore_ascii_case(requested_label) =>
                                        {
                                            if let Some(sync_directory) =
                                                profile.sync_directory.as_deref()
                                            {
                                                system::sync_directory_to_mount(
                                                    Path::new(sync_directory),
                                                    Path::new(&target_mount.path),
                                                )
                                                .map_err(|error| {
                                                    CreateError::unknown(localized_format!(
                                                        "msg_0624",
                                                        error = error
                                                    ))
                                                })?;
                                            }
                                            return Ok(device.identity.clone());
                                        }
                                        Ok(volume) => {
                                            return Err(CreateError::unknown(localized_format!(
                                                "msg_0301",
                                                volume.file_system,
                                                volume.label
                                            )));
                                        }
                                        Err(error) => {
                                            return Err(CreateError::unknown(localized_format!(
                                                "msg_0221",
                                                error = error
                                            )));
                                        }
                                    }
                                }
                                Err(error) => {
                                    readiness_detail = localized_format!("msg_0336", error = error);
                                }
                            }
                        }
                    }
                }
            }
            Err(error) => readiness_detail = localized_format!("msg_0310", error = error),
        }

        if Instant::now() >= deadline {
            let backend_hint = match profile.backend {
                DiskBackend::RamDyn => localized_text!("msg_0024"),
                DiskBackend::AimVirtualMemory => localized_text!("msg_0059"),
                DiskBackend::AimAweAlloc => localized_text!("msg_0577"),
                DiskBackend::AimImageFile => localized_text!("msg_0619"),
            };
            return Err(CreateError::unknown(localized_format!(
                "msg_0512",
                profile.mount_target_display(),
                readiness_detail = readiness_detail,
                backend_hint = backend_hint
            )));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn creation_correlation_label() -> String {
    // Kept within FAT32's 11-character label limit so every supported filesystem
    // can use the same per-launch correlation marker.
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;
    let unique = timestamp ^ (std::process::id() as u64).rotate_left(23);
    format!("A{:010X}", unique & 0xFF_FFFF_FFFF)
}

fn launch(
    profile: &DiskProfile,
    runtime: &RuntimePaths,
    size_kib: u64,
    correlation_label: &str,
    adapter_port: u8,
) -> Result<Child, String> {
    #[cfg(not(windows))]
    let _ = adapter_port;
    let mount_target = profile.mount_target();
    let work_directory = &runtime.directory;
    let mut command = Command::new(&runtime.ramdyn_exe);
    // RamDyn syntax here is MountPoint, SizeKiB, -1 (TRIM), MemoryType,
    // BlockSize, FormatParam, Label, SectorSize, Removable. TRIM omits the
    // two cleanup-policy arguments used by the non-TRIM form.
    command
        .current_dir(work_directory)
        .arg(&mount_target)
        .arg(size_kib.to_string())
        .arg("-1")
        .arg(profile.memory_mode.ramdyn_argument())
        .arg(profile.block_size_exponent.to_string())
        .arg(profile.file_system.format_argument())
        .arg(correlation_label)
        .arg("512")
        .arg(if profile.removable_media { "1" } else { "0" });

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
        command.env("AIM_ADAPTER_PORT", adapter_port.to_string());
    }

    command
        .spawn()
        .map_err(|error| localized_format!("msg_0136", error = error))
}

pub fn unmount(
    identity: DeviceIdentity,
    force: bool,
    runtime: &RuntimePaths,
    sync_directory: Option<String>,
) -> Result<(String, Option<String>), aim::UnmountError> {
    let (persistence_kind, persistence_warning) = if let Some(directory) = sync_directory {
        match save_mounted_directory(&identity, runtime, &directory) {
            Ok(()) => (Some(PersistenceKind::Directory), None),
            Err(error) if force => (Some(PersistenceKind::Directory), Some(error)),
            Err(error) => {
                return Err(aim::UnmountError::known(localized_format!(
                    "msg_0633",
                    error = error
                )))
            }
        }
    } else if is_persistent_image_device(&identity) {
        match flush_image_volumes(&identity, runtime) {
            Ok(()) => (Some(PersistenceKind::Image), None),
            Err(error) if force => (Some(PersistenceKind::Image), Some(error)),
            Err(error) => {
                return Err(aim::UnmountError::known(localized_format!(
                    "msg_0646",
                    error = error
                )))
            }
        }
    } else {
        (None, None)
    };
    let mount_point = match aim::unmount(&runtime.aimapi_backend_dll, identity.clone(), force) {
        Ok(mount_point) => mount_point,
        Err(mut error) => {
            if persistence_kind.is_some() {
                error.message = if let Some(persistence_error) = &persistence_warning {
                    localized_format!(
                        "msg_0639",
                        persistence_error = persistence_error,
                        error = error.message
                    )
                } else {
                    localized_format!("msg_0634", error = error.message)
                };
            }
            return Err(error);
        }
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match aim::enumerate(&runtime.aimapi_backend_dll) {
            Ok(snapshot)
                if snapshot.inventory_complete()
                    && !snapshot
                        .devices
                        .iter()
                        .any(|device| device.identity.occupies_same_adapter_slot(&identity)) =>
            {
                return Ok((mount_point, persistence_warning));
            }
            _ => {}
        }
        if Instant::now() >= deadline {
            let operation = if force {
                localized_text!("msg_0378")
            } else {
                localized_text!("msg_0439")
            };
            return Err(aim::UnmountError::unknown(localized_format!(
                "msg_0138",
                operation = operation,
                mount_point = mount_point
            )));
        }
        thread::sleep(Duration::from_millis(250));
    }
}

fn is_persistent_image_device(identity: &DeviceIdentity) -> bool {
    const IMSCSI_TYPE_MASK: u32 = 0x0000_0F00;
    const IMSCSI_TYPE_FILE: u32 = 0x0000_0100;
    const IMSCSI_FILE_TYPE_MASK: u32 = 0x0000_F000;
    const IMSCSI_FILE_TYPE_AWEALLOC: u32 = 0x0000_1000;

    identity.flags & IMSCSI_TYPE_MASK == IMSCSI_TYPE_FILE
        && identity.flags & IMSCSI_FILE_TYPE_MASK != IMSCSI_FILE_TYPE_AWEALLOC
}

fn device_flags_match_backend(backend: DiskBackend, flags: u32) -> bool {
    if backend == DiskBackend::RamDyn {
        // QueryDevice may omit the proxy transport subtype (0x3000), so only
        // identify RamDyn by the stable hard-disk proxy bits.
        return flags & 0x0000_00F0 == 0x10 && flags & 0x0000_0F00 == 0x0000_0300;
    }

    let expected_backing_type = match backend {
        DiskBackend::RamDyn => return false,
        DiskBackend::AimVirtualMemory => 0x0000_0200,
        DiskBackend::AimAweAlloc | DiskBackend::AimImageFile => 0x0000_0100,
    };
    let expected_auxiliary_type = match backend {
        DiskBackend::AimAweAlloc => 0x0000_1000,
        DiskBackend::AimVirtualMemory | DiskBackend::AimImageFile => 0,
        DiskBackend::RamDyn => return false,
    };
    flags & 0x0000_00F0 == 0x10
        && flags & 0x0000_0F00 == expected_backing_type
        && flags & 0x0000_F000 == expected_auxiliary_type
}

fn single_volume_guid_for_device(device: &AimDevice) -> Option<&str> {
    // Native and RamDyn creation produce one partition. The adapter scan plus
    // its direct volume association remain useful when an unrelated Windows
    // volume makes the global mount scan incomplete.
    if device.identity.physical_drive_number.is_none() {
        return None;
    }
    let [volume_guid] = device.volume_guids.as_slice() else {
        return None;
    };
    let [association] = device.volume_associations.as_slice() else {
        return None;
    };
    if !association.volume_guid.eq_ignore_ascii_case(volume_guid)
        || association.aim_device_count != 1
        || association.extent_count != 1
    {
        return None;
    }
    Some(volume_guid.as_str())
}

fn flush_image_volumes(identity: &DeviceIdentity, runtime: &RuntimePaths) -> Result<(), String> {
    let snapshot = aim::enumerate(&runtime.aimapi_backend_dll)?;
    if !snapshot.inventory_complete() {
        return Err(localized_text!("msg_0291").to_owned());
    }
    let device = snapshot
        .devices
        .iter()
        .find(|device| device.identity == *identity)
        .ok_or_else(|| localized_text!("msg_0637").to_owned())?;
    if !device.can_unmount() {
        return Err(device
            .unmount_block_reason()
            .unwrap_or(localized_text!("msg_0637"))
            .to_owned());
    }
    if identity.volume_guids.is_empty() {
        return Err(localized_text!("msg_0638").to_owned());
    }

    let mut errors = Vec::new();
    for volume_guid in &identity.volume_guids {
        if let Err(error) = system::flush_mounted_volume(volume_guid) {
            errors.push(format!("{volume_guid}: {error}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn save_mounted_directory(
    identity: &DeviceIdentity,
    runtime: &RuntimePaths,
    destination: &str,
) -> Result<(), String> {
    let snapshot = aim::enumerate(&runtime.aimapi_backend_dll)?;
    if !snapshot.inventory_complete() {
        return Err(localized_text!("msg_0291").to_owned());
    }
    let device = snapshot
        .devices
        .iter()
        .find(|device| device.identity == *identity)
        .ok_or_else(|| localized_text!("msg_0637").to_owned())?;
    if !device.can_unmount() {
        return Err(device
            .unmount_block_reason()
            .unwrap_or(localized_text!("msg_0637"))
            .to_owned());
    }
    let volume_guid = identity
        .volume_guid
        .as_deref()
        .ok_or_else(|| localized_text!("msg_0637").to_owned())?;
    let mount = device
        .mount_points
        .iter()
        .find(|mount| {
            mount
                .volume_guid
                .as_deref()
                .is_some_and(|guid| guid.eq_ignore_ascii_case(volume_guid))
        })
        .ok_or_else(|| localized_text!("msg_0638").to_owned())?;
    system::flush_mounted_volume(volume_guid)?;
    system::sync_mount_to_directory(Path::new(&mount.path), Path::new(destination))
}

#[cfg(test)]
mod tests {
    use super::{device_flags_match_backend, is_persistent_image_device};
    use crate::aim::DeviceIdentity;
    use crate::profiles::DiskBackend;

    #[test]
    fn backend_flags_distinguish_native_awe_from_ramdyn_and_vm() {
        assert!(device_flags_match_backend(
            DiskBackend::AimAweAlloc,
            0x0000_1110
        ));
        assert!(device_flags_match_backend(
            DiskBackend::AimVirtualMemory,
            0x0000_0210
        ));
        assert!(device_flags_match_backend(
            DiskBackend::AimImageFile,
            0x0000_0110
        ));
        assert!(device_flags_match_backend(DiskBackend::RamDyn, 0x0000_0310));
        assert!(!device_flags_match_backend(
            DiskBackend::AimAweAlloc,
            0x0000_0310
        ));
    }

    #[test]
    fn image_backing_needs_flush_but_awealloc_is_temporary_memory() {
        let identity = |flags| DeviceIdentity {
            port: 3,
            number: 7,
            physical_drive_number: Some(8),
            size_bytes: 1024,
            flags,
            volume_guids: Vec::new(),
            volume_guid: None,
        };
        assert!(is_persistent_image_device(&identity(0x0000_0110)));
        assert!(!is_persistent_image_device(&identity(0x0000_1110)));
        assert!(!is_persistent_image_device(&identity(0x0000_0210)));
    }
}

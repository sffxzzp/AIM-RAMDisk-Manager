#[derive(Debug)]
pub struct UnmountError {
    pub message: String,
    pub outcome_unknown: bool,
}

impl UnmountError {
    pub fn known(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            outcome_unknown: false,
        }
    }

    pub fn unknown(message: impl Into<String>) -> Self {
        Self {
            message: localized_format!("msg_0422", message.into()),
            outcome_unknown: true,
        }
    }
}

impl From<String> for UnmountError {
    fn from(message: String) -> Self {
        Self::known(message)
    }
}

#[derive(Debug)]
pub struct DeviceCreateError {
    pub message: String,
    pub outcome_unknown: bool,
}

impl DeviceCreateError {
    fn known(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            outcome_unknown: false,
        }
    }

    #[cfg(windows)]
    fn unknown(message: impl Into<String>) -> Self {
        Self {
            message: localized_format!("msg_0431", message.into()),
            outcome_unknown: true,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct DeviceIdentity {
    pub port: u8,
    pub number: u32,
    pub physical_drive_number: Option<u32>,
    pub size_bytes: u64,
    pub flags: u32,
    /// All volume GUIDs associated with this device at the time of enumeration.
    /// Kept in the operation identity so unload can detect topology changes.
    pub volume_guids: Vec<String>,
    /// Set only when a complete volume scan identifies exactly one volume.
    pub volume_guid: Option<String>,
}

impl DeviceIdentity {
    pub fn label(&self) -> String {
        localized_format!("msg_0276", self.port, self.number)
    }

    pub fn occupies_same_adapter_slot(&self, other: &Self) -> bool {
        self.port == other.port && self.number == other.number
    }
}

#[derive(Clone, Debug)]
pub struct MountPoint {
    pub path: String,
    pub volume_label: Option<String>,
    pub volume_guid: Option<String>,
    pub aim_device_count: usize,
    pub extent_count: usize,
}

#[derive(Clone, Debug)]
pub struct VolumeAssociation {
    pub volume_guid: String,
    pub aim_device_count: usize,
    pub extent_count: usize,
}

#[derive(Clone, Debug)]
pub struct AimDevice {
    pub identity: DeviceIdentity,
    /// Raw AIM flags, including mutable status bits such as IMAGE_MODIFIED.
    pub raw_flags: u32,
    pub mount_scan_complete: bool,
    /// Includes associated volumes even when they currently have no mount path.
    pub volume_guids: Vec<String>,
    /// Topology for every associated volume, including unmounted volumes.
    pub volume_associations: Vec<VolumeAssociation>,
    pub mount_points: Vec<MountPoint>,
}

impl AimDevice {
    pub fn is_hard_disk_proxy(&self) -> bool {
        self.identity.flags & 0x0000_00F0 == 0x0000_0010
            && self.identity.flags & 0x0000_0F00 == 0x0000_0300
    }

    pub fn device_type_label(&self) -> &'static str {
        match self.identity.flags & 0x0000_00F0 {
            0x10 => localized_text!("msg_0410"),
            0x20 => localized_text!("msg_0531"),
            0x30 => localized_text!("msg_0034"),
            0x40 => localized_text!("msg_0388"),
            _ => localized_text!("msg_0412"),
        }
    }

    pub fn backing_type_label(&self) -> &'static str {
        if self.identity.flags & 0x0000_0F00 == 0x0000_0100
            && self.identity.flags & 0x0000_F000 == 0x0000_1000
        {
            return localized_text!("msg_0564");
        }
        match self.identity.flags & 0x0000_0F00 {
            0x0000_0100 => localized_text!("msg_0197"),
            0x0000_0200 => localized_text!("msg_0443"),
            0x0000_0300 => localized_text!("msg_0039"),
            _ => localized_text!("msg_0182"),
        }
    }

    pub fn preferred_mount_point(&self) -> Option<&MountPoint> {
        self.mount_points
            .iter()
            .find(|mount| is_drive_letter_root_mount(&mount.path))
            .or_else(|| self.mount_points.first())
    }

    pub fn display_mount_point(&self) -> String {
        if let Some(mount_point) = self.preferred_mount_point() {
            mount_point.display()
        } else if !self.mount_scan_complete {
            localized_text!("msg_0061").to_owned()
        } else {
            localized_text!("msg_0368").to_owned()
        }
    }

    pub fn can_unmount(&self) -> bool {
        self.unmount_block_reason().is_none()
    }

    pub fn unmount_block_reason(&self) -> Option<&'static str> {
        if !self.mount_scan_complete {
            return Some(localized_text!("msg_0091"));
        }
        if self.identity.physical_drive_number.is_none() {
            return Some(localized_text!("msg_0265"));
        }
        if self.volume_guids.is_empty() {
            return Some(localized_text!("msg_0313"));
        }
        if !same_volume_guid_set(&self.identity.volume_guids, &self.volume_guids)
            || self.volume_associations.len() != self.volume_guids.len()
        {
            return Some(localized_text!("msg_0045"));
        }
        if self.volume_guids.len() == 1 {
            let Some(volume_guid) = self.identity.volume_guid.as_deref() else {
                return Some(localized_text!("msg_0262"));
            };
            if !self.volume_guids[0].eq_ignore_ascii_case(volume_guid) {
                return Some(localized_text!("msg_0311"));
            }
        } else if self.identity.volume_guid.is_some() {
            return Some(localized_text!("msg_0254"));
        }
        let associated_volume_guids = self
            .volume_associations
            .iter()
            .map(|volume| volume.volume_guid.clone())
            .collect::<Vec<_>>();
        if !same_volume_guid_set(&self.volume_guids, &associated_volume_guids) {
            return Some(localized_text!("msg_0550"));
        }
        if self.volume_associations.iter().any(|volume| {
            !self
                .volume_guids
                .iter()
                .any(|guid| guid.eq_ignore_ascii_case(&volume.volume_guid))
                || volume.aim_device_count != 1
                || volume.extent_count != 1
        }) {
            return Some(localized_text!("msg_0102"));
        }
        if self.mount_points.iter().any(|mount| {
            !mount.volume_guid.as_deref().is_some_and(|guid| {
                self.volume_guids
                    .iter()
                    .any(|volume_guid| volume_guid.eq_ignore_ascii_case(guid))
            })
        }) {
            return Some(localized_text!("msg_0535"));
        }
        if self
            .mount_points
            .iter()
            .any(|mount| mount.aim_device_count != 1 || mount.extent_count != 1)
        {
            return Some(localized_text!("msg_0102"));
        }
        None
    }
}

fn same_volume_guid_set(left: &[String], right: &[String]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut matched = vec![false; right.len()];
    for guid in left {
        let Some(index) = right
            .iter()
            .enumerate()
            .position(|(index, candidate)| !matched[index] && candidate.eq_ignore_ascii_case(guid))
        else {
            return false;
        };
        matched[index] = true;
    }
    true
}

fn drive_letter_from_root_mount(path: &str) -> Option<char> {
    let path = path.trim_end_matches('\\');
    let mut chars = path.chars();
    let letter = chars.next()?;
    (letter.is_ascii_alphabetic() && chars.next() == Some(':') && chars.next().is_none())
        .then_some(letter)
}

fn is_drive_letter_root_mount(path: &str) -> bool {
    drive_letter_from_root_mount(path).is_some()
}

impl MountPoint {
    #[cfg(windows)]
    pub fn drive_letter_root(&self) -> Option<char> {
        drive_letter_from_root_mount(&self.path)
    }

    pub fn display(&self) -> String {
        let path = self.path.trim_end_matches('\\');
        match self
            .volume_label
            .as_deref()
            .filter(|label| !label.is_empty())
        {
            Some(label) => format!("{path} / {label}"),
            None => path.to_owned(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AimSnapshot {
    pub adapter_ports: Vec<u8>,
    pub adapter_instance_count: Option<usize>,
    pub adapter_scan_complete: bool,
    pub adapter_scan_error: Option<String>,
    pub mount_scan_complete: bool,
    pub mount_scan_error: Option<String>,
    pub devices: Vec<AimDevice>,
}

impl AimSnapshot {
    pub fn inventory_complete(&self) -> bool {
        self.adapter_scan_complete && self.mount_scan_complete
    }

    pub fn preferred_adapter_port(&self) -> Option<u8> {
        self.adapter_ports.first().copied()
    }
}

#[cfg(windows)]
mod native {
    use super::{
        same_volume_guid_set, AimDevice, AimSnapshot, DeviceCreateError, DeviceIdentity,
        MountPoint, UnmountError, VolumeAssociation,
    };
    use std::collections::{HashMap, HashSet};
    use std::ffi::CString;
    use std::io;
    use std::mem;
    use std::path::Path;
    use std::ptr;

    use windows_sys::Win32::Foundation::{
        CloseHandle, FreeLibrary, GetLastError, ERROR_ACCESS_DENIED, ERROR_FILE_NOT_FOUND,
        ERROR_PATH_NOT_FOUND, ERROR_REVISION_MISMATCH, HANDLE, HWND, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, FlushFileBuffers,
        GetVolumePathNamesForVolumeNameW, FILE_SHARE_READ, FILE_SHARE_WRITE, IOCTL_VOLUME_OFFLINE,
        IOCTL_VOLUME_ONLINE, OPEN_EXISTING,
    };
    use windows_sys::Win32::System::Ioctl::{
        FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, FSCTL_UNLOCK_VOLUME,
    };
    use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryExW};
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const IMSCSI_IMAGE_MODIFIED: u32 = 0x0001_0000;

    #[link(name = "Cfgmgr32")]
    unsafe extern "system" {
        fn CM_Get_Device_ID_List_SizeW(length: *mut u32, filter: *const u16, flags: u32) -> u32;
        fn CM_Get_Device_ID_ListW(
            filter: *const u16,
            buffer: *mut u16,
            buffer_length: u32,
            flags: u32,
        ) -> u32;
    }

    const ERROR_MORE_DATA: u32 = 234;
    const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
    const ERROR_INVALID_FUNCTION: u32 = 1;
    const ERROR_NO_MORE_FILES: u32 = 18;
    // AIM's driver accepts this marker for automatic device-number allocation.
    // The managed wrapper translates u32::MAX to this value before calling AIM.
    const IMSCSI_AUTO_DEVICE_NUMBER: u32 = 0x00FF_FFFF;
    // Windows IOCTL and AIM API flag values from their respective headers.
    const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
    const IMSCSI_API_NO_BROADCAST_NOTIFY: u64 = 0x1;
    const IMSCSI_API_FORCE_DISMOUNT: u64 = 0x2;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const IMSCSI_DEVICE_NUMBER_LIST_INITIAL: usize = 16;
    // Bound allocation if an incompatible API reports an unreasonable count.
    const IMSCSI_DEVICE_NUMBER_LIST_MAX: usize = 65_536;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct DeviceNumber {
        long_number: u32,
    }

    #[repr(C, packed(4))]
    struct DeviceConfigurationFixed {
        device_number: u32,
        disk_size: i64,
        bytes_per_sector: u32,
        reserved: u32,
        image_offset: i64,
        flags: u32,
        file_name_length: u16,
    }

    const _: [(); 4] = [(); mem::size_of::<DeviceNumber>()];
    const _: [(); 36] = [(); mem::size_of::<DeviceConfigurationFixed>()];
    const _: [(); 4] = [(); mem::offset_of!(DeviceConfigurationFixed, disk_size)];
    const _: [(); 12] = [(); mem::offset_of!(DeviceConfigurationFixed, bytes_per_sector)];
    const _: [(); 16] = [(); mem::offset_of!(DeviceConfigurationFixed, reserved)];
    const _: [(); 20] = [(); mem::offset_of!(DeviceConfigurationFixed, image_offset)];
    const _: [(); 28] = [(); mem::offset_of!(DeviceConfigurationFixed, flags)];
    const _: [(); 32] = [(); mem::offset_of!(DeviceConfigurationFixed, file_name_length)];
    // ImScsiQueryDevice requires space for the variable-length file name.
    const QUERY_BUFFER_SIZE: usize = mem::size_of::<DeviceConfigurationFixed>() + u16::MAX as usize;

    type OpenScsiAdapter = unsafe extern "system" fn(*mut u8) -> HANDLE;
    type OpenScsiAdapterByPort = unsafe extern "system" fn(u8) -> HANDLE;
    type CheckDriverVersion = unsafe extern "system" fn(HANDLE) -> i32;
    type GetDeviceList = unsafe extern "system" fn(u32, HANDLE, *mut DeviceNumber, *mut u32) -> i32;
    type QueryDevice = unsafe extern "system" fn(HANDLE, *mut DeviceConfigurationFixed, u32) -> i32;
    type OpenDiskByDeviceNumber = unsafe extern "system" fn(DeviceNumber, u32, *mut u32) -> HANDLE;
    type GetDeviceNumbersForVolumeEx =
        unsafe extern "system" fn(HANDLE, *mut DeviceNumber, *mut u8, u32, *mut u32) -> i32;
    type GetApiFlags = unsafe extern "system" fn() -> u64;
    type SetApiFlags = unsafe extern "system" fn(u64) -> u64;
    type RemoveDeviceByMountPoint = unsafe extern "system" fn(HWND, *const u16) -> i32;
    type RemoveDeviceByNumber = unsafe extern "system" fn(HWND, HANDLE, DeviceNumber) -> i32;
    type CreateDevice = unsafe extern "system" fn(
        HWND,
        HANDLE,
        *mut DeviceNumber,
        *mut i64,
        *mut u32,
        *mut i64,
        *mut u32,
        *const u16,
        i32,
        *mut u16,
        i32,
    ) -> i32;
    type NotifyRemovePending = unsafe extern "system" fn(u16) -> i32;

    struct ApiFunctions {
        open_scsi_adapter: OpenScsiAdapter,
        open_scsi_adapter_by_port: Option<OpenScsiAdapterByPort>,
        check_driver_version: CheckDriverVersion,
        get_device_list: GetDeviceList,
        query_device: QueryDevice,
        open_disk_by_device_number: OpenDiskByDeviceNumber,
        get_device_numbers_for_volume_ex: GetDeviceNumbersForVolumeEx,
        get_api_flags: GetApiFlags,
        set_api_flags: SetApiFlags,
        remove_device_by_mount_point: RemoveDeviceByMountPoint,
        remove_device_by_number: RemoveDeviceByNumber,
        create_device: Option<CreateDevice>,
        notify_remove_pending: Option<NotifyRemovePending>,
    }

    struct AimLibrary {
        module: windows_sys::Win32::Foundation::HMODULE,
        functions: ApiFunctions,
    }

    struct AdapterInventory {
        adapters: Vec<(OwnedHandle, u8)>,
        instance_count: Option<usize>,
        complete: bool,
        error: Option<String>,
    }

    impl Drop for AimLibrary {
        fn drop(&mut self) {
            unsafe { FreeLibrary(self.module) };
        }
    }

    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            if is_valid_handle(self.0) {
                unsafe { CloseHandle(self.0) };
            }
        }
    }

    struct PreparedVolume {
        guid: String,
        handle: OwnedHandle,
        locked: bool,
        offline: bool,
        rollback_online: bool,
    }

    impl PreparedVolume {
        fn rollback(&mut self) -> Vec<String> {
            let mut errors = Vec::new();
            if self.rollback_online && self.offline {
                match volume_control(self.handle.0, IOCTL_VOLUME_ONLINE) {
                    Ok(()) => self.offline = false,
                    Err(code) => errors.push(format_volume_error(
                        localized_text!("msg_0469"),
                        &self.guid,
                        code,
                    )),
                }
            }
            if self.locked {
                match volume_control(self.handle.0, FSCTL_UNLOCK_VOLUME) {
                    Ok(()) => self.locked = false,
                    Err(code) => errors.push(format_volume_error(
                        localized_text!("msg_0215"),
                        &self.guid,
                        code,
                    )),
                }
            }
            errors
        }
    }

    impl Drop for PreparedVolume {
        fn drop(&mut self) {
            let _ = self.rollback();
        }
    }

    struct VolumeSearchHandle(HANDLE);

    impl Drop for VolumeSearchHandle {
        fn drop(&mut self) {
            unsafe { FindVolumeClose(self.0) };
        }
    }

    struct MountPointScan {
        by_device: HashMap<(u8, u32), Vec<MountPoint>>,
        // Keep unmounted volumes too, so a multi-volume device cannot look single-volume.
        volume_guids_by_device: HashMap<(u8, u32), Vec<String>>,
        volume_associations_by_device: HashMap<(u8, u32), Vec<VolumeAssociation>>,
        complete: bool,
        first_error: Option<String>,
    }

    pub fn enumerate(dll_path: &Path) -> Result<AimSnapshot, String> {
        let library = AimLibrary::load(dll_path)?;
        let adapter_inventory = library.open_adapters()?;
        let adapter_ports = adapter_inventory
            .adapters
            .iter()
            .map(|(_, port)| *port)
            .collect::<Vec<_>>();
        let mut adapter_scan_complete = adapter_inventory.complete;
        let mut adapter_errors = adapter_inventory.error.into_iter().collect::<Vec<_>>();
        let mut device_configurations = Vec::new();
        let mut devices_by_physical_drive: HashMap<u32, Vec<(u8, u32)>> = HashMap::new();
        let mut devices_without_physical_drive = 0;
        for (adapter, adapter_port) in &adapter_inventory.adapters {
            if unsafe { (library.functions.check_driver_version)(adapter.0) } == 0 {
                adapter_scan_complete = false;
                adapter_errors.push(localized_format!("msg_0569", port = adapter_port));
                continue;
            }

            let numbers = match get_device_numbers(&library.functions, adapter.0) {
                Ok(numbers) => numbers,
                Err(error) => {
                    adapter_scan_complete = false;
                    adapter_errors.push(localized_format!(
                        "msg_0570",
                        port = adapter_port,
                        error = error
                    ));
                    continue;
                }
            };
            for number in numbers {
                let configuration = match query_device(&library.functions, adapter.0, number) {
                    Ok(configuration) => configuration,
                    Err(error) => {
                        adapter_scan_complete = false;
                        if number.long_number == u32::MAX
                            || number.long_number == IMSCSI_AUTO_DEVICE_NUMBER
                        {
                            adapter_errors.push(localized_format!(
                                "msg_0648",
                                number = number.long_number
                            ));
                        } else {
                            adapter_errors.push(error);
                        }
                        continue;
                    }
                };
                let physical_drive_number =
                    open_disk_number(&library.functions, number, *adapter_port);
                if let Some(disk_number) = physical_drive_number {
                    devices_by_physical_drive
                        .entry(disk_number)
                        .or_default()
                        .push((*adapter_port, number.long_number));
                } else {
                    devices_without_physical_drive += 1;
                }
                device_configurations.push((
                    *adapter_port,
                    number,
                    configuration,
                    physical_drive_number,
                ));
            }
        }
        // Volume extents are compared against the combined inventory from every
        // accessible AIM SCSI port, so shared or ambiguous mappings stay blocked.
        let mount_scan = scan_mount_points(
            &library.functions,
            &devices_by_physical_drive,
            adapter_scan_complete,
            devices_without_physical_drive,
        );
        let mut devices = Vec::with_capacity(device_configurations.len());

        for (adapter_port, number, configuration, physical_drive_number) in device_configurations {
            let mut points = mount_scan
                .by_device
                .get(&(adapter_port, number.long_number))
                .cloned()
                .unwrap_or_default();
            points.sort_by(|left, right| left.path.cmp(&right.path));
            points.dedup_by(|left, right| left.path == right.path);
            let mut volume_associations = mount_scan
                .volume_associations_by_device
                .get(&(adapter_port, number.long_number))
                .cloned()
                .unwrap_or_default();
            volume_associations.sort_by(|left, right| {
                left.volume_guid
                    .to_ascii_lowercase()
                    .cmp(&right.volume_guid.to_ascii_lowercase())
            });
            volume_associations
                .dedup_by(|left, right| left.volume_guid.eq_ignore_ascii_case(&right.volume_guid));
            let mut volume_guids = mount_scan
                .volume_guids_by_device
                .get(&(adapter_port, number.long_number))
                .cloned()
                .unwrap_or_default();
            volume_guids.sort_by_key(|guid| guid.to_ascii_lowercase());
            volume_guids.dedup_by(|left, right| left.eq_ignore_ascii_case(right));
            let volume_guid =
                (mount_scan.complete && volume_guids.len() == 1).then(|| volume_guids[0].clone());
            let size_bytes = configuration.0;
            let flags = configuration.1;
            devices.push(AimDevice {
                identity: DeviceIdentity {
                    port: adapter_port,
                    number: number.long_number,
                    physical_drive_number,
                    size_bytes,
                    // IMAGE_MODIFIED changes while an image-backed disk is in
                    // use, so it is diagnostic state rather than identity.
                    flags: flags & !IMSCSI_IMAGE_MODIFIED,
                    volume_guids: volume_guids.clone(),
                    volume_guid,
                },
                raw_flags: flags,
                mount_scan_complete: mount_scan.complete && adapter_scan_complete,
                volume_guids,
                volume_associations,
                mount_points: points,
            });
        }

        devices.sort_by_key(|device| (device.identity.port, device.identity.number));
        Ok(AimSnapshot {
            adapter_ports,
            adapter_instance_count: adapter_inventory.instance_count,
            adapter_scan_complete,
            adapter_scan_error: (!adapter_errors.is_empty()).then(|| adapter_errors.join("\n")),
            mount_scan_complete: mount_scan.complete,
            mount_scan_error: mount_scan.first_error.clone(),
            devices,
        })
    }

    pub fn create_native_device(
        dll_path: &Path,
        expected_adapter_port: u8,
        disk_size_bytes: u64,
        bytes_per_sector: u32,
        removable: bool,
        awe_alloc: bool,
        mount_point: &str,
    ) -> Result<u32, DeviceCreateError> {
        let library = AimLibrary::load(dll_path).map_err(DeviceCreateError::known)?;
        let adapter = library
            .open_adapter_by_port(expected_adapter_port)
            .map_err(DeviceCreateError::known)?;
        if unsafe { (library.functions.check_driver_version)(adapter.0) } == 0 {
            return Err(DeviceCreateError::known(win32_error(localized_text!(
                "msg_0028"
            ))));
        }
        let Some(create_device) = library.functions.create_device else {
            return Err(DeviceCreateError::known(localized_text!("msg_0476")));
        };
        let Ok(mut disk_size) = i64::try_from(disk_size_bytes) else {
            return Err(DeviceCreateError::known(localized_text!("msg_0212")));
        };
        let mut device_number = DeviceNumber {
            long_number: IMSCSI_AUTO_DEVICE_NUMBER,
        };
        let mut sector_size = bytes_per_sector;
        let mut flags = if awe_alloc {
            0x0000_0010 | 0x0000_0100 | 0x0000_1000
        } else {
            0x0000_0010 | 0x0000_0200
        };
        if removable {
            flags |= 0x0000_0002;
        }
        let mut mount_point = wide(mount_point);
        let created = unsafe {
            create_device(
                ptr::null_mut(),
                adapter.0,
                &mut device_number,
                &mut disk_size,
                &mut sector_size,
                ptr::null_mut(),
                &mut flags,
                ptr::null(),
                0,
                mount_point.as_mut_ptr(),
                1,
            )
        } != 0;
        if !created {
            return Err(DeviceCreateError::unknown(win32_error(localized_text!(
                "msg_0245"
            ))));
        }
        if device_number.long_number == u32::MAX
            || device_number.long_number == IMSCSI_AUTO_DEVICE_NUMBER
        {
            return Err(DeviceCreateError::unknown(localized_text!("msg_0219")));
        }
        Ok(device_number.long_number)
    }

    pub fn create_image_device(
        dll_path: &Path,
        expected_adapter_port: u8,
        image_file: &str,
        removable: bool,
        mount_point: &str,
    ) -> Result<u32, DeviceCreateError> {
        let library = AimLibrary::load(dll_path).map_err(DeviceCreateError::known)?;
        let adapter = library
            .open_adapter_by_port(expected_adapter_port)
            .map_err(DeviceCreateError::known)?;
        if unsafe { (library.functions.check_driver_version)(adapter.0) } == 0 {
            return Err(DeviceCreateError::known(win32_error(localized_text!(
                "msg_0028"
            ))));
        }
        let Some(create_device) = library.functions.create_device else {
            return Err(DeviceCreateError::known(localized_text!("msg_0476")));
        };
        let mut device_number = DeviceNumber {
            long_number: IMSCSI_AUTO_DEVICE_NUMBER,
        };
        let mut disk_size = 0_i64;
        let mut sector_size = 0_u32;
        let mut flags = 0x0000_0010 | 0x0000_0100;
        if removable {
            flags |= 0x0000_0002;
        }
        let image_file = wide(image_file);
        let mut mount_point = wide(mount_point);
        let created = unsafe {
            create_device(
                ptr::null_mut(),
                adapter.0,
                &mut device_number,
                &mut disk_size,
                &mut sector_size,
                ptr::null_mut(),
                &mut flags,
                image_file.as_ptr(),
                0,
                mount_point.as_mut_ptr(),
                0,
            )
        } != 0;
        if !created {
            return Err(DeviceCreateError::unknown(win32_error(localized_text!(
                "msg_0596"
            ))));
        }
        if device_number.long_number == u32::MAX
            || device_number.long_number == IMSCSI_AUTO_DEVICE_NUMBER
        {
            return Err(DeviceCreateError::unknown(localized_text!("msg_0219")));
        }
        Ok(device_number.long_number)
    }

    pub fn unmount(
        dll_path: &Path,
        identity: DeviceIdentity,
        force: bool,
    ) -> Result<String, UnmountError> {
        let library = AimLibrary::load(dll_path)?;
        let adapter_inventory = library.open_adapters()?;
        if !adapter_inventory.complete {
            return Err(UnmountError::known(
                adapter_inventory
                    .error
                    .unwrap_or_else(|| localized_text!("msg_0566").to_owned()),
            ));
        }
        let Some((adapter, _)) = adapter_inventory
            .adapters
            .iter()
            .find(|(_, port)| *port == identity.port)
        else {
            return Err(UnmountError::known(localized_text!("msg_0309")));
        };

        let mut device_configurations = Vec::new();
        for (current_adapter, port) in &adapter_inventory.adapters {
            if unsafe { (library.functions.check_driver_version)(current_adapter.0) } == 0 {
                return Err(UnmountError::known(localized_format!(
                    "msg_0569",
                    port = port
                )));
            }
            let numbers = get_device_numbers(&library.functions, current_adapter.0)
                .map_err(UnmountError::known)?;
            for number in numbers {
                let configuration = query_device(&library.functions, current_adapter.0, number)
                    .map_err(UnmountError::known)?;
                let physical_drive_number = open_disk_number(&library.functions, number, *port);
                device_configurations.push((*port, number, configuration, physical_drive_number));
            }
        }

        let target_configuration = device_configurations.iter().find(|(port, number, _, _)| {
            *port == identity.port && number.long_number == identity.number
        });
        let Some((_, _target, (current_size, current_flags), target_physical_drive)) =
            target_configuration.copied()
        else {
            return Err(UnmountError::known(localized_text!("msg_0352")));
        };
        if current_size != identity.size_bytes
            || current_flags & !IMSCSI_IMAGE_MODIFIED != identity.flags
        {
            return Err(UnmountError::known(localized_text!("msg_0510")));
        }
        let current_physical_drive = target_physical_drive;
        if identity.physical_drive_number.is_none()
            || current_physical_drive != identity.physical_drive_number
        {
            return Err(UnmountError::known(localized_text!("msg_0361")));
        }
        let mut devices_by_physical_drive: HashMap<u32, Vec<(u8, u32)>> = HashMap::new();
        let mut devices_without_physical_drive = 0;
        for (port, number, _, physical_drive_number) in &device_configurations {
            let physical_drive_number = *physical_drive_number;
            if let Some(disk_number) = physical_drive_number {
                devices_by_physical_drive
                    .entry(disk_number)
                    .or_default()
                    .push((*port, number.long_number));
            } else {
                devices_without_physical_drive += 1;
            }
        }
        let mut mount_scan = scan_mount_points(
            &library.functions,
            &devices_by_physical_drive,
            adapter_inventory.complete,
            devices_without_physical_drive,
        );
        if !mount_scan.complete {
            return Err(UnmountError::known(localized_text!("msg_0281")));
        }
        let current_volume_guids = mount_scan
            .volume_guids_by_device
            .remove(&(identity.port, identity.number))
            .unwrap_or_default();
        if identity.volume_guids.is_empty()
            || !same_volume_guid_set(&identity.volume_guids, &current_volume_guids)
        {
            return Err(UnmountError::known(localized_text!("msg_0148")));
        }
        let current_volume_associations = mount_scan
            .volume_associations_by_device
            .remove(&(identity.port, identity.number))
            .unwrap_or_default();
        let associated_volume_guids = current_volume_associations
            .iter()
            .map(|volume| volume.volume_guid.clone())
            .collect::<Vec<_>>();
        if current_volume_associations.len() != current_volume_guids.len()
            || !same_volume_guid_set(&associated_volume_guids, &current_volume_guids)
            || current_volume_associations
                .iter()
                .any(|volume| volume.aim_device_count != 1 || volume.extent_count != 1)
        {
            return Err(UnmountError::known(localized_text!("msg_0546")));
        }
        if current_volume_guids.len() == 1 {
            if !identity
                .volume_guid
                .as_deref()
                .is_some_and(|guid| guid.eq_ignore_ascii_case(&current_volume_guids[0]))
            {
                return Err(UnmountError::known(localized_text!("msg_0047")));
            }
        } else if identity.volume_guid.is_some() {
            return Err(UnmountError::known(localized_text!("msg_0312")));
        }
        let current_points = mount_scan
            .by_device
            .remove(&(identity.port, identity.number))
            .unwrap_or_default();
        if current_points
            .iter()
            .any(|point| point.aim_device_count != 1 || point.extent_count != 1)
            || current_points.iter().any(|point| {
                !point.volume_guid.as_deref().is_some_and(|guid| {
                    current_volume_guids
                        .iter()
                        .any(|volume_guid| volume_guid.eq_ignore_ascii_case(guid))
                })
            })
        {
            return Err(UnmountError::known(localized_text!("msg_0231")));
        }
        if current_volume_guids.len() > 1 {
            return unmount_multiple_volumes(
                &library.functions,
                adapter.0,
                &identity,
                &current_volume_associations,
                &current_points,
                force,
            );
        }
        let Some(expected_volume_guid) = identity.volume_guid.as_deref() else {
            return Err(UnmountError::known(localized_text!("msg_0017")));
        };
        let target_mount = current_points
            .iter()
            .find(|point| super::is_drive_letter_root_mount(&point.path))
            .or_else(|| current_points.first());

        let flags_before = unsafe { (library.functions.get_api_flags)() };
        let drive_letter =
            target_mount.and_then(|mount| super::drive_letter_from_root_mount(&mount.path));
        // Keep the shell notification while removing by stable volume identity.
        let use_volume_guid = if flags_before & IMSCSI_API_NO_BROADCAST_NOTIFY != 0 {
            true
        } else if let Some(letter) = drive_letter {
            if let Some(notify_remove_pending) = library.functions.notify_remove_pending {
                if unsafe { notify_remove_pending(letter.to_ascii_uppercase() as u16) } == 0 {
                    return Err(UnmountError::known(localized_text!("msg_0411")));
                }
                true
            } else {
                // Older API builds can notify only when given the drive path.
                false
            }
        } else {
            true
        };
        let removal_path = if use_volume_guid {
            expected_volume_guid.trim_end_matches('\\')
        } else {
            target_mount
                .map(|mount| mount.path.as_str())
                .unwrap_or(expected_volume_guid.trim_end_matches('\\'))
        };
        let mount = wide(removal_path);
        let flags_for_remove = if force {
            flags_before | IMSCSI_API_FORCE_DISMOUNT
        } else {
            flags_before & !IMSCSI_API_FORCE_DISMOUNT
        };
        unsafe {
            (library.functions.set_api_flags)(flags_for_remove);
        }
        let removed = unsafe {
            (library.functions.remove_device_by_mount_point)(ptr::null_mut(), mount.as_ptr())
        } != 0;
        let remove_error = if removed {
            None
        } else {
            Some(unsafe { GetLastError() })
        };
        unsafe {
            (library.functions.set_api_flags)(flags_before);
        }
        if let Some(code) = remove_error {
            let error = io::Error::from_raw_os_error(code as i32);
            let operation = if force {
                localized_text!("msg_0378")
            } else {
                localized_text!("msg_0439")
            };
            let message = if matches!(
                code,
                windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED
                    | windows_sys::Win32::Foundation::ERROR_BUSY
                    | windows_sys::Win32::Foundation::ERROR_DEVICE_IN_USE
                    | windows_sys::Win32::Foundation::ERROR_LOCK_VIOLATION
                    | windows_sys::Win32::Foundation::ERROR_SHARING_VIOLATION
            ) {
                if force {
                    localized_format!("msg_0330", code = code, error = error)
                } else {
                    localized_format!("msg_0168", code = code, error = error)
                }
            } else {
                localized_format!(
                    "msg_0133",
                    operation = operation,
                    code = code,
                    error = error
                )
            };
            return Err(UnmountError::unknown(message));
        }
        Ok(target_mount.map_or_else(
            || localized_format!("msg_0166", identity.label()),
            |mount| mount.path.trim_end_matches('\\').to_owned(),
        ))
    }

    fn unmount_multiple_volumes(
        functions: &ApiFunctions,
        adapter: HANDLE,
        identity: &DeviceIdentity,
        volumes: &[VolumeAssociation],
        mount_points: &[MountPoint],
        force: bool,
    ) -> Result<String, UnmountError> {
        if volumes.len() <= 1 {
            return Err(UnmountError::known(localized_text!("msg_0150")));
        }

        let flags_before = unsafe { (functions.get_api_flags)() };
        let mut force_issues = Vec::new();
        if flags_before & IMSCSI_API_NO_BROADCAST_NOTIFY == 0 {
            let mut drive_letters = mount_points
                .iter()
                .filter_map(MountPoint::drive_letter_root)
                .map(|letter| letter.to_ascii_uppercase())
                .collect::<Vec<_>>();
            drive_letters.sort_unstable();
            drive_letters.dedup();

            if !drive_letters.is_empty() {
                if let Some(notify_remove_pending) = functions.notify_remove_pending {
                    for letter in drive_letters {
                        if unsafe { notify_remove_pending(letter as u16) } == 0 {
                            let error = localized_format!("msg_0359", letter = letter);
                            if force {
                                force_issues.push(error);
                            } else {
                                return Err(UnmountError::known(localized_format!(
                                    "msg_0273",
                                    error = error
                                )));
                            }
                        }
                    }
                } else {
                    let error = localized_text!("msg_0487");
                    if force {
                        force_issues.push(error.to_owned());
                    } else {
                        return Err(UnmountError::known(localized_format!(
                            "msg_0273",
                            error = error
                        )));
                    }
                }
            }
        }

        let mut prepared = Vec::with_capacity(volumes.len());
        for volume in volumes {
            match open_volume_for_unmount(&volume.volume_guid) {
                Ok(handle) => prepared.push(PreparedVolume {
                    guid: volume.volume_guid.clone(),
                    handle,
                    locked: false,
                    offline: false,
                    rollback_online: true,
                }),
                Err(error) if force => force_issues.push(error),
                Err(error) => {
                    return Err(UnmountError::known(localized_format!(
                        "msg_0222",
                        error = error
                    )))
                }
            }
        }

        let mut flush_issues = Vec::new();
        for volume in &prepared {
            if unsafe { FlushFileBuffers(volume.handle.0) } == 0 {
                let code = unsafe { GetLastError() };
                flush_issues.push(format_volume_error(
                    localized_text!("msg_0394"),
                    &volume.guid,
                    code,
                ));
            }
        }
        if !flush_issues.is_empty() {
            if force {
                force_issues.extend(flush_issues);
            } else {
                let message = with_volume_rollback_details(
                    &mut prepared,
                    localized_format!("msg_0395", flush_issues[0]),
                );
                return Err(UnmountError::known(message));
            }
        }

        let mut lock_issues = Vec::new();
        for volume in &mut prepared {
            if let Err(code) = volume_control(volume.handle.0, FSCTL_LOCK_VOLUME) {
                lock_issues.push(format_volume_error(
                    localized_text!("msg_0560"),
                    &volume.guid,
                    code,
                ));
            } else {
                volume.locked = true;
            }
        }
        if !lock_issues.is_empty() {
            if !force {
                let message = with_volume_rollback_details(
                    &mut prepared,
                    localized_format!("msg_0408", lock_issues[0]),
                );
                return Err(UnmountError::known(message));
            }
        }

        let mut dismount_issues = Vec::new();
        for volume in &mut prepared {
            if let Err(code) = volume_control(volume.handle.0, FSCTL_DISMOUNT_VOLUME) {
                dismount_issues.push(format_volume_error(
                    localized_text!("msg_0305"),
                    &volume.guid,
                    code,
                ));
            }
        }
        if !dismount_issues.is_empty() {
            if force {
                force_issues.extend(dismount_issues);
            } else {
                let message = with_volume_rollback_details(
                    &mut prepared,
                    localized_format!("msg_0395", dismount_issues[0]),
                );
                return Err(UnmountError::known(message));
            }
        }

        if force && !lock_issues.is_empty() {
            for volume in prepared.iter_mut().filter(|volume| !volume.locked) {
                if let Err(code) = volume_control(volume.handle.0, FSCTL_LOCK_VOLUME) {
                    force_issues.push(format_volume_error(
                        localized_text!("msg_0299"),
                        &volume.guid,
                        code,
                    ));
                } else {
                    volume.locked = true;
                }
            }
        }

        let mut offline_issues = Vec::new();
        for volume in &mut prepared {
            if let Err(code) = volume_control(volume.handle.0, IOCTL_VOLUME_OFFLINE) {
                offline_issues.push(format_volume_error(
                    localized_text!("msg_0042"),
                    &volume.guid,
                    code,
                ));
            } else {
                volume.offline = true;
            }
        }
        if !offline_issues.is_empty() {
            if force {
                force_issues.extend(offline_issues);
            } else {
                let message = with_volume_rollback_details(
                    &mut prepared,
                    localized_format!("msg_0395", offline_issues[0]),
                );
                return Err(UnmountError::known(message));
            }
        }

        let removed = unsafe {
            (functions.remove_device_by_number)(
                ptr::null_mut(),
                adapter,
                DeviceNumber {
                    long_number: identity.number,
                },
            )
        } != 0;
        if !removed {
            let code = unsafe { GetLastError() };
            let error = io::Error::from_raw_os_error(code as i32);
            let operation = if force {
                localized_text!("msg_0378")
            } else {
                localized_text!("msg_0439")
            };
            let mut message = localized_format!(
                "msg_0133",
                operation = operation,
                code = code,
                error = error
            );
            if force && !force_issues.is_empty() {
                message.push_str(&localized_format!(
                    "msg_0528",
                    force_issues.len(),
                    describe_volume_issues(&force_issues)
                ));
            }
            let rollback_issues = restore_prepared_volumes(&mut prepared);
            if !rollback_issues.is_empty() {
                message.push_str(&localized_format!(
                    "msg_0467",
                    describe_volume_issues(&rollback_issues)
                ));
            }
            return Err(UnmountError::unknown(message));
        }

        for volume in &mut prepared {
            volume.rollback_online = false;
            volume.locked = false;
            volume.offline = false;
        }

        let volume_summaries = volumes
            .iter()
            .map(|volume| {
                let mut points = mount_points
                    .iter()
                    .filter(|point| {
                        point
                            .volume_guid
                            .as_deref()
                            .is_some_and(|guid| guid.eq_ignore_ascii_case(&volume.volume_guid))
                    })
                    .map(MountPoint::display)
                    .collect::<Vec<_>>();
                points.sort();
                points.dedup();
                if points.is_empty() {
                    localized_format!("msg_0461", volume.volume_guid)
                } else {
                    points.join(localized_text!("msg_0562"))
                }
            })
            .collect::<Vec<_>>();
        let mut summary = localized_format!("msg_0143", volumes.len(), volume_summaries.join("；"));
        if force && !force_issues.is_empty() {
            summary.push_str(&localized_format!(
                "msg_0503",
                force_issues.len(),
                describe_volume_issues(&force_issues)
            ));
        }
        Ok(summary)
    }

    fn open_volume_for_unmount(volume_guid: &str) -> Result<OwnedHandle, String> {
        // Volume GUID paths returned by FindFirstVolumeW have a trailing slash.
        let path = wide(volume_guid.trim_end_matches('\\'));
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            )
        };
        if is_valid_handle(handle) {
            Ok(OwnedHandle(handle))
        } else {
            Err(win32_error(&localized_format!(
                "msg_0005",
                volume_guid = volume_guid
            )))
        }
    }

    fn volume_control(volume: HANDLE, control_code: u32) -> Result<(), u32> {
        let mut bytes_returned = 0_u32;
        if unsafe {
            DeviceIoControl(
                volume,
                control_code,
                ptr::null(),
                0,
                ptr::null_mut(),
                0,
                &mut bytes_returned,
                ptr::null_mut(),
            )
        } != 0
        {
            Ok(())
        } else {
            Err(unsafe { GetLastError() })
        }
    }

    fn format_volume_error(operation: &str, volume_guid: &str, code: u32) -> String {
        let error = io::Error::from_raw_os_error(code as i32);
        localized_format!(
            "msg_0065",
            operation = operation,
            volume_guid = volume_guid,
            code = code,
            error = error
        )
    }

    fn restore_prepared_volumes(prepared: &mut [PreparedVolume]) -> Vec<String> {
        prepared
            .iter_mut()
            .rev()
            .flat_map(PreparedVolume::rollback)
            .collect()
    }

    fn describe_volume_issues(issues: &[String]) -> String {
        let mut visible = issues.iter().take(4).cloned().collect::<Vec<_>>();
        let omitted = issues.len().saturating_sub(visible.len());
        if omitted > 0 {
            visible.push(localized_format!("msg_0387", omitted = omitted));
        }
        visible.join("；")
    }

    fn with_volume_rollback_details(
        prepared: &mut [PreparedVolume],
        primary_error: String,
    ) -> String {
        let rollback_issues = restore_prepared_volumes(prepared);
        if rollback_issues.is_empty() {
            primary_error
        } else {
            localized_format!(
                "msg_0541",
                describe_volume_issues(&rollback_issues),
                primary_error = primary_error
            )
        }
    }

    impl AimLibrary {
        fn load(path: &Path) -> Result<Self, String> {
            if !path.is_file() {
                return Err(localized_format!("msg_0463", path.display()));
            }
            crate::system::require_x64_pe(path, "aimapi-backend.dll")?;
            let path = wide_path(path);
            let module = unsafe {
                LoadLibraryExW(path.as_ptr(), ptr::null_mut(), 0x0000_0100 | 0x0000_1000)
            };
            if module.is_null() {
                return Err(win32_error(localized_text!("msg_0184")));
            }
            let functions = (|| -> Result<ApiFunctions, String> {
                Ok(ApiFunctions {
                    open_scsi_adapter: load_symbol(module, "ImScsiOpenScsiAdapter")?,
                    open_scsi_adapter_by_port: load_symbol(
                        module,
                        "ImScsiOpenScsiAdapterByScsiPortNumber",
                    )
                    .ok(),
                    check_driver_version: load_symbol(module, "ImScsiCheckDriverVersion")?,
                    get_device_list: load_symbol(module, "ImScsiGetDeviceList")?,
                    query_device: load_symbol(module, "ImScsiQueryDevice")?,
                    open_disk_by_device_number: load_symbol(
                        module,
                        "ImScsiOpenDiskByDeviceNumber",
                    )?,
                    get_device_numbers_for_volume_ex: load_symbol(
                        module,
                        "ImScsiGetDeviceNumbersForVolumeEx",
                    )?,
                    get_api_flags: load_symbol(module, "ImScsiGetAPIFlags")?,
                    set_api_flags: load_symbol(module, "ImScsiSetAPIFlags")?,
                    remove_device_by_mount_point: load_symbol(
                        module,
                        "ImScsiRemoveDeviceByMountPoint",
                    )?,
                    remove_device_by_number: load_symbol(module, "ImScsiRemoveDeviceByNumber")?,
                    create_device: load_symbol(module, "ImScsiCreateDevice").ok(),
                    notify_remove_pending: load_symbol::<NotifyRemovePending>(
                        module,
                        "ImScsiNotifyRemovePending",
                    )
                    .ok(),
                })
            })();
            match functions {
                Ok(functions) => Ok(Self { module, functions }),
                Err(error) => {
                    unsafe { FreeLibrary(module) };
                    Err(error)
                }
            }
        }

        fn open_adapter(&self) -> Result<(OwnedHandle, u8), String> {
            let mut port = 0_u8;
            let handle = unsafe { (self.functions.open_scsi_adapter)(&mut port) };
            if !is_valid_handle(handle) {
                return Err(adapter_open_error());
            }
            Ok((OwnedHandle(handle), port))
        }

        fn open_adapter_by_port(&self, port: u8) -> Result<OwnedHandle, String> {
            if let Some(open_by_port) = self.functions.open_scsi_adapter_by_port {
                let handle = unsafe { open_by_port(port) };
                if is_valid_handle(handle) {
                    return Ok(OwnedHandle(handle));
                }
                return Err(adapter_open_error());
            }
            let (adapter, actual_port) = self.open_adapter()?;
            if actual_port == port {
                Ok(adapter)
            } else {
                Err(localized_format!("msg_0565", port = port))
            }
        }

        fn open_adapters(&self) -> Result<AdapterInventory, String> {
            let instance_count = adapter_instance_count();
            if let Some(open_by_port) = self.functions.open_scsi_adapter_by_port {
                let mut adapters = Vec::new();
                for port in u8::MIN..=u8::MAX {
                    let handle = unsafe { open_by_port(port) };
                    if is_valid_handle(handle) {
                        adapters.push((OwnedHandle(handle), port));
                    }
                }
                adapters.sort_by_key(|(_, port)| *port);

                if adapters.is_empty() {
                    // Preserve the usual AIM diagnostic if neither API path can
                    // open an adapter, rather than presenting an empty inventory.
                    let (adapter, port) = self.open_adapter()?;
                    adapters.push((adapter, port));
                    return Ok(AdapterInventory {
                        adapters,
                        instance_count,
                        complete: false,
                        error: Some(localized_text!("msg_0566").to_owned()),
                    });
                }

                let actual_count = adapters.len();
                let mismatch = instance_count.is_some_and(|expected| expected != actual_count);
                let error = mismatch.then(|| {
                    localized_format!(
                        "msg_0567",
                        expected = instance_count.unwrap_or_default(),
                        actual = actual_count
                    )
                });
                return Ok(AdapterInventory {
                    adapters,
                    instance_count,
                    complete: !mismatch,
                    error,
                });
            }

            let (adapter, port) = self.open_adapter()?;
            let complete = instance_count == Some(1);
            let error = (!complete).then(|| localized_text!("msg_0568").to_owned());
            Ok(AdapterInventory {
                adapters: vec![(adapter, port)],
                instance_count,
                complete,
                error,
            })
        }
    }

    fn adapter_instance_count() -> Option<usize> {
        const CM_GETIDLIST_FILTER_SERVICE: u32 = 0x0000_0002;
        const CM_GETIDLIST_FILTER_PRESENT: u32 = 0x0000_0100;
        const FILTER: u32 = CM_GETIDLIST_FILTER_SERVICE | CM_GETIDLIST_FILTER_PRESENT;
        let service = wide("phdskmnt");
        let mut length = 0_u32;
        if unsafe { CM_Get_Device_ID_List_SizeW(&mut length, service.as_ptr(), FILTER) } != 0
            || length < 2
        {
            return None;
        }

        let mut instances = vec![0_u16; length as usize];
        if unsafe {
            CM_Get_Device_ID_ListW(service.as_ptr(), instances.as_mut_ptr(), length, FILTER)
        } != 0
        {
            return None;
        }

        let mut offset = 0;
        let mut count = 0;
        while offset < instances.len() && instances[offset] != 0 {
            let end = instances[offset..].iter().position(|unit| *unit == 0)?;
            if end == 0 {
                break;
            }
            count += 1;
            offset += end + 1;
        }
        Some(count)
    }

    fn get_device_numbers(
        functions: &ApiFunctions,
        adapter: HANDLE,
    ) -> Result<Vec<DeviceNumber>, String> {
        let mut capacity = IMSCSI_DEVICE_NUMBER_LIST_INITIAL;
        loop {
            let mut numbers = vec![DeviceNumber::default(); capacity];
            let mut count = 0_u32;
            let ok = unsafe {
                (functions.get_device_list)(
                    capacity as u32,
                    adapter,
                    numbers.as_mut_ptr(),
                    &mut count,
                )
            };
            if ok != 0 {
                if count as usize > numbers.len() {
                    return Err(localized_text!("msg_0337").to_owned());
                }
                numbers.truncate(count as usize);
                return Ok(numbers);
            }
            let error = unsafe { GetLastError() };
            if error != ERROR_MORE_DATA || capacity >= IMSCSI_DEVICE_NUMBER_LIST_MAX {
                return Err(localized_format!("msg_0278", error = error));
            }
            capacity = usize::max(count as usize, capacity.saturating_mul(2));
            capacity = capacity.max(3).min(IMSCSI_DEVICE_NUMBER_LIST_MAX);
        }
    }

    fn query_device(
        functions: &ApiFunctions,
        adapter: HANDLE,
        number: DeviceNumber,
    ) -> Result<(u64, u32), String> {
        let word_count = QUERY_BUFFER_SIZE.div_ceil(mem::size_of::<u32>());
        let mut buffer = vec![0_u32; word_count];
        let fixed = buffer.as_mut_ptr().cast::<DeviceConfigurationFixed>();
        unsafe {
            ptr::addr_of_mut!((*fixed).device_number).write_unaligned(number.long_number);
        }
        let ok = unsafe { (functions.query_device)(adapter, fixed, QUERY_BUFFER_SIZE as u32) };
        if ok == 0 {
            return Err(localized_format!("msg_0356", number.long_number, unsafe {
                GetLastError()
            }));
        }
        let size = unsafe { ptr::addr_of!((*fixed).disk_size).read_unaligned() };
        if size < 0 {
            return Err(localized_format!("msg_0120", number.long_number));
        }
        let flags = unsafe { ptr::addr_of!((*fixed).flags).read_unaligned() };
        Ok((size as u64, flags))
    }

    fn open_disk_number(functions: &ApiFunctions, number: DeviceNumber, port: u8) -> Option<u32> {
        let mut disk_number = u32::MAX;
        let handle = unsafe {
            (functions.open_disk_by_device_number)(number, port as u32, &mut disk_number)
        };
        if !is_valid_handle(handle) {
            return None;
        }
        unsafe { CloseHandle(handle) };
        (disk_number != u32::MAX).then_some(disk_number)
    }

    fn record_mount_scan_error(first_error: &mut Option<String>, error: String) {
        if first_error.is_none() {
            *first_error = Some(error);
        }
    }

    fn record_volume_scan_error(
        first_error: &mut Option<String>,
        volume_guid: &str,
        error: String,
    ) {
        record_mount_scan_error(
            first_error,
            localized_format!("msg_0050", volume_guid = volume_guid, error = error),
        );
    }

    fn scan_mount_points(
        functions: &ApiFunctions,
        devices_by_physical_drive: &HashMap<u32, Vec<(u8, u32)>>,
        adapter_list_is_complete: bool,
        devices_without_physical_drive: usize,
    ) -> MountPointScan {
        let mut by_device: HashMap<(u8, u32), Vec<MountPoint>> = HashMap::new();
        let mut volume_guids_by_device: HashMap<(u8, u32), Vec<String>> = HashMap::new();
        let mut volume_associations_by_device: HashMap<(u8, u32), Vec<VolumeAssociation>> =
            HashMap::new();
        let mut volume_name = [0_u16; 128];
        let search =
            unsafe { FindFirstVolumeW(volume_name.as_mut_ptr(), volume_name.len() as u32) };
        if !is_valid_handle(search) {
            return MountPointScan {
                by_device,
                volume_guids_by_device,
                volume_associations_by_device,
                complete: false,
                first_error: Some(win32_error(localized_text!("msg_0478"))),
            };
        }
        let _search = VolumeSearchHandle(search);
        let mut complete = true;
        let mut first_error = None;
        if adapter_list_is_complete && devices_without_physical_drive > 0 {
            complete = false;
            record_mount_scan_error(
                &mut first_error,
                localized_format!(
                    "msg_0164",
                    devices_without_physical_drive = devices_without_physical_drive
                ),
            );
        }

        loop {
            let Some(name_length) = volume_name.iter().position(|unit| *unit == 0) else {
                complete = false;
                record_mount_scan_error(&mut first_error, localized_text!("msg_0515").to_owned());
                break;
            };
            let volume_guid = String::from_utf16_lossy(&volume_name[..name_length]);
            match volume_mount_paths(&volume_guid) {
                Ok(paths) => match open_volume(&volume_guid) {
                    Ok(volume) => {
                        let disk_numbers = match raw_volume_disk_numbers(volume.0) {
                            Ok(numbers) => numbers,
                            Err(error) => {
                                complete = false;
                                record_volume_scan_error(&mut first_error, &volume_guid, error);
                                Vec::new()
                            }
                        };
                        let extent_count = disk_numbers.len();
                        let mut direct_devices = Vec::new();
                        let mut seen_direct_devices = HashSet::new();
                        let mut ambiguous_physical_drive = false;
                        for disk_number in &disk_numbers {
                            if let Some(identities) = devices_by_physical_drive.get(disk_number) {
                                if identities.len() > 1 {
                                    ambiguous_physical_drive = true;
                                }
                                for identity in identities {
                                    if seen_direct_devices.insert(*identity) {
                                        direct_devices.push(*identity);
                                    }
                                }
                            }
                        }
                        if ambiguous_physical_drive {
                            complete = false;
                            record_volume_scan_error(
                                &mut first_error,
                                &volume_guid,
                                localized_text!("msg_0379").to_owned(),
                            );
                        }

                        let mut device_identities = direct_devices.clone();
                        if !adapter_list_is_complete {
                            // Keep the AIM helper for multi-adapter systems so it
                            // can find devices on other ports. If it disagrees
                            // with a direct match, do not call the scan complete:
                            // ERROR_INVALID_FUNCTION can mean either an ordinary
                            // disk or an AIM PhysicalDrive the helper could not open.
                            match volume_devices(functions, volume.0, extent_count) {
                                Ok(api_devices) => {
                                    let api_identities = api_devices
                                        .into_iter()
                                        .map(|(identity, _)| identity)
                                        .collect::<HashSet<_>>();
                                    if direct_devices
                                        .iter()
                                        .any(|identity| !api_identities.contains(identity))
                                    {
                                        complete = false;
                                        record_volume_scan_error(
                                            &mut first_error,
                                            &volume_guid,
                                            localized_text!("msg_0270").to_owned(),
                                        );
                                    }
                                    for identity in api_identities {
                                        if seen_direct_devices.insert(identity) {
                                            device_identities.push(identity);
                                        }
                                    }
                                }
                                Err(error) => {
                                    complete = false;
                                    record_volume_scan_error(&mut first_error, &volume_guid, error);
                                }
                            }
                        }

                        let device_count = device_identities.len();
                        let label = paths.iter().find_map(|path| read_volume_label(path));
                        for identity in device_identities {
                            let associations =
                                volume_associations_by_device.entry(identity).or_default();
                            if !associations.iter().any(|association| {
                                association.volume_guid.eq_ignore_ascii_case(&volume_guid)
                            }) {
                                associations.push(VolumeAssociation {
                                    volume_guid: volume_guid.clone(),
                                    aim_device_count: device_count,
                                    extent_count,
                                });
                            }
                            let volume_guids = volume_guids_by_device.entry(identity).or_default();
                            if !volume_guids
                                .iter()
                                .any(|guid| guid.eq_ignore_ascii_case(&volume_guid))
                            {
                                volume_guids.push(volume_guid.clone());
                            }
                            for path in &paths {
                                by_device.entry(identity).or_default().push(MountPoint {
                                    path: path.clone(),
                                    volume_label: label.clone(),
                                    volume_guid: Some(volume_guid.clone()),
                                    aim_device_count: device_count,
                                    extent_count,
                                });
                            }
                        }
                    }
                    Err(error) => {
                        complete = false;
                        record_volume_scan_error(&mut first_error, &volume_guid, error);
                    }
                },
                Err(error) => {
                    complete = false;
                    record_volume_scan_error(&mut first_error, &volume_guid, error);
                }
            }

            volume_name.fill(0);
            if unsafe {
                FindNextVolumeW(search, volume_name.as_mut_ptr(), volume_name.len() as u32)
            } == 0
            {
                let error = unsafe { GetLastError() };
                if error != ERROR_NO_MORE_FILES {
                    complete = false;
                    record_mount_scan_error(
                        &mut first_error,
                        win32_error_code(localized_text!("msg_0402"), error),
                    );
                    // Keep the entries found so far; failed enumeration must not
                    // make a partially readable AIM device look safely removable.
                }
                break;
            }
        }
        MountPointScan {
            by_device,
            volume_guids_by_device,
            volume_associations_by_device,
            complete,
            first_error,
        }
    }

    fn volume_mount_paths(volume_guid: &str) -> Result<Vec<String>, String> {
        const MAX_PATH_BUFFER: usize = 1_048_576;

        let volume_guid = wide(volume_guid);
        let mut capacity = 256_usize;
        loop {
            let mut buffer = vec![0_u16; capacity];
            let mut returned = 0_u32;
            let ok = unsafe {
                GetVolumePathNamesForVolumeNameW(
                    volume_guid.as_ptr(),
                    buffer.as_mut_ptr(),
                    capacity as u32,
                    &mut returned,
                )
            };
            if ok != 0 {
                let mut paths = Vec::new();
                let mut offset = 0;
                while offset < buffer.len() && buffer[offset] != 0 {
                    let Some(relative_end) = buffer[offset..].iter().position(|unit| *unit == 0)
                    else {
                        return Err(localized_text!("msg_0318").to_owned());
                    };
                    let end = relative_end + offset;
                    paths.push(String::from_utf16_lossy(&buffer[offset..end]));
                    offset = end + 1;
                }
                return Ok(paths);
            }

            let error = unsafe { GetLastError() };
            if error != ERROR_MORE_DATA {
                return Err(win32_error_code(localized_text!("msg_0080"), error));
            }
            if returned as usize <= capacity || returned as usize > MAX_PATH_BUFFER {
                return Err(localized_format!("msg_0345", returned));
            }
            capacity = returned as usize;
        }
    }

    fn open_volume(volume_guid: &str) -> Result<OwnedHandle, String> {
        // FindFirstVolumeW returns a GUID path with a trailing slash. CreateFileW
        // requires that slash to be removed when opening a volume handle.
        let path = wide(volume_guid.trim_end_matches('\\'));
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                ptr::null(),
                OPEN_EXISTING,
                0,
                ptr::null_mut(),
            )
        };
        if is_valid_handle(handle) {
            Ok(OwnedHandle(handle))
        } else {
            Err(win32_error(localized_text!("msg_0167")))
        }
    }

    fn raw_volume_disk_numbers(volume: HANDLE) -> Result<Vec<u32>, String> {
        let mut capacity = 4_usize;
        loop {
            let buffer_bytes = 8_usize
                .checked_add(
                    capacity
                        .checked_mul(24)
                        .ok_or_else(|| localized_text!("msg_0539").to_owned())?,
                )
                .ok_or_else(|| localized_text!("msg_0539").to_owned())?;
            let mut buffer = vec![0_u64; buffer_bytes.div_ceil(mem::size_of::<u64>())];
            let mut bytes_returned = 0_u32;
            let ok = unsafe {
                DeviceIoControl(
                    volume,
                    IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
                    ptr::null(),
                    0,
                    buffer.as_mut_ptr().cast(),
                    buffer_bytes as u32,
                    &mut bytes_returned,
                    ptr::null_mut(),
                )
            };
            let count = unsafe { ptr::read_unaligned(buffer.as_ptr().cast::<u32>()) as usize };
            if ok != 0 {
                if count > capacity || (bytes_returned as usize) < 8 + count * 24 {
                    return Err(localized_text!("msg_0214").to_owned());
                }
                let bytes = buffer.as_ptr().cast::<u8>();
                let mut disk_numbers = Vec::with_capacity(count);
                for index in 0..count {
                    let offset = 8 + index * 24;
                    disk_numbers
                        .push(unsafe { ptr::read_unaligned(bytes.add(offset).cast::<u32>()) });
                }
                return Ok(disk_numbers);
            }
            let error = unsafe { GetLastError() };
            if error != ERROR_MORE_DATA && error != ERROR_INSUFFICIENT_BUFFER {
                return Err(win32_error_code(localized_text!("msg_0490"), error));
            }
            if count <= capacity || count > 4096 {
                return Err(localized_format!("msg_0409", count = count));
            }
            capacity = count;
        }
    }

    fn read_volume_label(root: &str) -> Option<String> {
        let volume = crate::system::volume_information(root).ok()?;
        (!volume.label.is_empty()).then_some(volume.label)
    }

    fn volume_devices(
        functions: &ApiFunctions,
        volume: HANDLE,
        extent_count: usize,
    ) -> Result<Vec<((u8, u32), usize)>, String> {
        // The AIM helper does not report the required item count when its
        // internal IOCTL buffer is too small. The volume extent count gives
        // us a safe upper bound for its output buffer.
        let mut capacity = if extent_count == usize::MAX {
            4
        } else {
            extent_count.clamp(1, 4096)
        };
        loop {
            let mut numbers = vec![DeviceNumber::default(); capacity];
            let mut ports = vec![0_u8; capacity];
            let mut needed = 0_u32;
            let ok = unsafe {
                (functions.get_device_numbers_for_volume_ex)(
                    volume,
                    numbers.as_mut_ptr(),
                    ports.as_mut_ptr(),
                    capacity as u32,
                    &mut needed,
                )
            };
            if ok != 0 {
                if needed as usize > capacity {
                    return Err(localized_text!("msg_0135").to_owned());
                }
                numbers.truncate(needed as usize);
                ports.truncate(needed as usize);
                let mut seen = HashSet::new();
                let unique_devices = numbers
                    .into_iter()
                    .zip(ports)
                    .filter_map(|(number, port)| {
                        let identity = (port, number.long_number);
                        seen.insert(identity).then_some(identity)
                    })
                    .collect::<Vec<_>>();
                let aim_device_count = unique_devices.len();
                return Ok(unique_devices
                    .into_iter()
                    .map(|identity| (identity, aim_device_count))
                    .collect());
            }
            let error = unsafe { GetLastError() };
            if error == ERROR_INVALID_FUNCTION {
                // The AIM API returns ERROR_INVALID_FUNCTION when a volume has
                // no AIM-backed disk extents. Ordinary Windows volumes are
                // expected to take this path and do not make the scan incomplete.
                return Ok(Vec::new());
            }
            if error != ERROR_MORE_DATA {
                return Err(win32_error_code(localized_text!("msg_0181"), error));
            }
            if needed as usize <= capacity || needed > 4096 {
                return Err(localized_format!("msg_0471", needed = needed));
            }
            capacity = needed as usize;
        }
    }

    fn load_symbol<T: Copy>(
        module: windows_sys::Win32::Foundation::HMODULE,
        name: &str,
    ) -> Result<T, String> {
        let symbol = CString::new(name).expect("symbol names do not contain NUL");
        let address = unsafe { GetProcAddress(module, symbol.as_ptr().cast()) }
            .ok_or_else(|| localized_format!("msg_0351", name = name))?;
        Ok(unsafe { mem::transmute_copy(&address) })
    }

    fn is_valid_handle(handle: HANDLE) -> bool {
        !handle.is_null() && handle != INVALID_HANDLE_VALUE
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    fn wide_path(path: &Path) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;

        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    fn win32_error(action: &str) -> String {
        let code = unsafe { GetLastError() };
        win32_error_code(action, code)
    }

    fn win32_error_code(action: &str, code: u32) -> String {
        localized_format!(
            "msg_0068",
            io::Error::from_raw_os_error(code as i32),
            action = action,
            code = code
        )
    }

    fn adapter_open_error() -> String {
        let code = unsafe { GetLastError() };
        let message = match code {
            ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND => {
                localized_text!("msg_0013")
            }
            ERROR_ACCESS_DENIED => localized_text!("msg_0139"),
            ERROR_REVISION_MISMATCH => {
                localized_text!("msg_0201")
            }
            _ => localized_text!("msg_0508"),
        };
        localized_format!(
            "msg_0048",
            io::Error::from_raw_os_error(code as i32),
            message = message,
            code = code
        )
    }
}

#[cfg(windows)]
pub use native::{create_image_device, create_native_device, enumerate, unmount};

#[cfg(not(windows))]
pub fn enumerate(_dll_path: &std::path::Path) -> Result<AimSnapshot, String> {
    Err(localized_text!("msg_0189").to_owned())
}

#[cfg(not(windows))]
pub fn unmount(
    _dll_path: &std::path::Path,
    _identity: DeviceIdentity,
    _force: bool,
) -> Result<String, UnmountError> {
    Err(UnmountError::known(localized_text!("msg_0189")))
}

#[cfg(not(windows))]
pub fn create_native_device(
    _dll_path: &std::path::Path,
    _expected_adapter_port: u8,
    _disk_size_bytes: u64,
    _bytes_per_sector: u32,
    _removable: bool,
    _awe_alloc: bool,
    _mount_point: &str,
) -> Result<u32, DeviceCreateError> {
    Err(DeviceCreateError::known(localized_text!("msg_0165")))
}

#[cfg(not(windows))]
pub fn create_image_device(
    _dll_path: &std::path::Path,
    _expected_adapter_port: u8,
    _image_file: &str,
    _removable: bool,
    _mount_point: &str,
) -> Result<u32, DeviceCreateError> {
    Err(DeviceCreateError::known(localized_text!("msg_0165")))
}

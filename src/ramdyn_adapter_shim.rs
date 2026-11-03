#![allow(non_snake_case)]

use std::ffi::c_void;
use std::sync::OnceLock;

type Handle = *mut c_void;
type OpenAdapter = unsafe extern "system" fn(*mut u8) -> Handle;
type OpenAdapterByPort = unsafe extern "system" fn(u8) -> Handle;
type CreateDevice = unsafe extern "system" fn(
    Handle,
    Handle,
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
type OpenDiskByDeviceNumber = unsafe extern "system" fn(DeviceNumber, u32, *mut u32) -> Handle;
type RescanAdapter = unsafe extern "system" fn() -> i32;
type RemoveDeviceByNumber = unsafe extern "system" fn(Handle, Handle, DeviceNumber) -> i32;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct DeviceNumber {
    long_number: u32,
}

struct RealApi {
    open_adapter: OpenAdapter,
    open_adapter_by_port: Option<OpenAdapterByPort>,
    create_device: CreateDevice,
    open_disk_by_number: OpenDiskByDeviceNumber,
    rescan_adapter: RescanAdapter,
    remove_device_by_number: RemoveDeviceByNumber,
}

unsafe impl Send for RealApi {}
unsafe impl Sync for RealApi {}

static REAL_API: OnceLock<Result<RealApi, u32>> = OnceLock::new();

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CloseHandle(handle: Handle) -> i32;
    fn FreeLibrary(module: Handle) -> i32;
    fn GetLastError() -> u32;
    fn GetProcAddress(module: Handle, name: *const u8) -> *mut c_void;
    fn LoadLibraryW(name: *const u16) -> Handle;
    fn SetLastError(error: u32);
}

const INVALID_HANDLE: Handle = -1_isize as Handle;
const ERROR_MOD_NOT_FOUND: u32 = 126;
const ERROR_PROC_NOT_FOUND: u32 = 127;
const ERROR_INVALID_PARAMETER: u32 = 87;

unsafe fn symbol<T: Copy>(module: Handle, name: &'static [u8]) -> Option<T> {
    let address = GetProcAddress(module, name.as_ptr());
    (!address.is_null()).then(|| std::mem::transmute_copy(&address))
}

unsafe fn load_real_api() -> Result<RealApi, u32> {
    let name: Vec<u16> = "aimapi-backend.dll"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let module = LoadLibraryW(name.as_ptr());
    if module.is_null() || module == INVALID_HANDLE {
        return Err(GetLastError().max(ERROR_MOD_NOT_FOUND));
    }
    let api = RealApi {
        open_adapter: match symbol(module, b"ImScsiOpenScsiAdapter\0") {
            Some(function) => function,
            None => {
                FreeLibrary(module);
                return Err(ERROR_PROC_NOT_FOUND);
            }
        },
        open_adapter_by_port: symbol(module, b"ImScsiOpenScsiAdapterByScsiPortNumber\0"),
        create_device: match symbol(module, b"ImScsiCreateDevice\0") {
            Some(function) => function,
            None => {
                FreeLibrary(module);
                return Err(ERROR_PROC_NOT_FOUND);
            }
        },
        open_disk_by_number: match symbol(module, b"ImScsiOpenDiskByDeviceNumber\0") {
            Some(function) => function,
            None => {
                FreeLibrary(module);
                return Err(ERROR_PROC_NOT_FOUND);
            }
        },
        rescan_adapter: match symbol(module, b"ImScsiRescanScsiAdapter\0") {
            Some(function) => function,
            None => {
                FreeLibrary(module);
                return Err(ERROR_PROC_NOT_FOUND);
            }
        },
        remove_device_by_number: match symbol(module, b"ImScsiRemoveDeviceByNumber\0") {
            Some(function) => function,
            None => {
                FreeLibrary(module);
                return Err(ERROR_PROC_NOT_FOUND);
            }
        },
    };
    Ok(api)
}

fn api() -> Result<&'static RealApi, u32> {
    REAL_API
        .get_or_init(|| unsafe { load_real_api() })
        .as_ref()
        .map_err(|error| *error)
}

fn requested_port() -> Result<u8, u32> {
    std::env::var("AIM_ADAPTER_PORT")
        .ok()
        .and_then(|value| value.parse::<u8>().ok())
        .ok_or(ERROR_INVALID_PARAMETER)
}

unsafe fn open_requested_adapter(api: &RealApi, port: u8) -> Result<Handle, u32> {
    let handle = if let Some(open_by_port) = api.open_adapter_by_port {
        open_by_port(port)
    } else {
        let mut actual_port = u8::MAX;
        let handle = (api.open_adapter)(&mut actual_port);
        if actual_port != port {
            if !handle.is_null() && handle != INVALID_HANDLE {
                CloseHandle(handle);
            }
            SetLastError(ERROR_INVALID_PARAMETER);
            return Err(ERROR_INVALID_PARAMETER);
        }
        handle
    };
    if handle.is_null() || handle == INVALID_HANDLE {
        return Err(GetLastError());
    }
    Ok(handle)
}

#[no_mangle]
pub unsafe extern "system" fn ImScsiOpenScsiAdapter(port: *mut u8) -> Handle {
    let result = (|| {
        let api = api()?;
        let selected_port = requested_port()?;
        let handle = open_requested_adapter(api, selected_port)?;
        if !port.is_null() {
            *port = selected_port;
        }
        Ok::<Handle, u32>(handle)
    })();
    result.unwrap_or_else(|error| {
        SetLastError(error);
        INVALID_HANDLE
    })
}

#[no_mangle]
pub unsafe extern "system" fn ImScsiCreateDevice(
    window: Handle,
    _adapter: Handle,
    device_number: *mut DeviceNumber,
    disk_size: *mut i64,
    bytes_per_sector: *mut u32,
    image_offset: *mut i64,
    flags: *mut u32,
    file_name: *const u16,
    native_path: i32,
    mount_point: *mut u16,
    create_partition: i32,
) -> i32 {
    let result = (|| {
        let api = api()?;
        let port = requested_port()?;
        let adapter = open_requested_adapter(api, port)?;
        let created = (api.create_device)(
            window,
            adapter,
            device_number,
            disk_size,
            bytes_per_sector,
            image_offset,
            flags,
            file_name,
            native_path,
            mount_point,
            create_partition,
        );
        CloseHandle(adapter);
        Ok::<i32, u32>(created)
    })();
    result.unwrap_or_else(|error| {
        SetLastError(error);
        0
    })
}

#[no_mangle]
pub unsafe extern "system" fn ImScsiOpenDiskByDeviceNumber(
    device_number: DeviceNumber,
    _port: u32,
    disk_number: *mut u32,
) -> Handle {
    let result = (|| {
        let api = api()?;
        let port = requested_port()?;
        Ok::<Handle, u32>((api.open_disk_by_number)(
            device_number,
            port as u32,
            disk_number,
        ))
    })();
    result.unwrap_or_else(|error| {
        SetLastError(error);
        INVALID_HANDLE
    })
}

#[no_mangle]
pub unsafe extern "system" fn ImScsiRescanScsiAdapter() -> i32 {
    match api() {
        Ok(api) => (api.rescan_adapter)(),
        Err(error) => {
            SetLastError(error);
            0
        }
    }
}

#[no_mangle]
pub unsafe extern "system" fn ImScsiRemoveDeviceByNumber(
    window: Handle,
    adapter: Handle,
    device_number: DeviceNumber,
) -> i32 {
    match api() {
        Ok(api) => (api.remove_device_by_number)(window, adapter, device_number),
        Err(error) => {
            SetLastError(error);
            0
        }
    }
}

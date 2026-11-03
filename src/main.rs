#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[macro_use]
mod i18n;
mod aim;
mod app;
mod profiles;
mod ramdyn;
mod startup;
mod system;

fn load_application_icon() -> eframe::egui::IconData {
    let image = image::load_from_memory_with_format(
        include_bytes!("../assets/VD.ico"),
        image::ImageFormat::Ico,
    )
    .expect("failed to load embedded application icon")
    .into_rgba8();

    eframe::egui::IconData {
        width: image.width(),
        height: image.height(),
        rgba: image.into_raw(),
    }
}

fn main() -> eframe::Result<()> {
    let manager_settings = profiles::ManagerSettingsStore::load();
    i18n::set_language(
        manager_settings
            .language_override()
            .unwrap_or_else(i18n::Language::system_default),
    );
    let startup_mode = std::env::args_os()
        .skip(1)
        .any(|argument| argument == std::ffi::OsStr::new("--startup"));
    let _instance = match acquire_single_instance() {
        Ok(Some(instance)) => instance,
        Ok(None) if startup_mode => return Ok(()),
        Ok(None) => {
            show_startup_error(localized_text!("msg_0004"));
            return Ok(());
        }
        Err(error) => {
            show_startup_error(&error);
            return Ok(());
        }
    };

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([960.0, 640.0])
            .with_min_inner_size([720.0, 480.0])
            .with_icon(std::sync::Arc::new(load_application_icon())),
        ..Default::default()
    };
    let title = localized_text!("msg_0449");
    let result = eframe::run_native(
        title,
        options,
        Box::new(move |creation| {
            Ok(Box::new(app::ManagerApp::new(
                &creation.egui_ctx,
                startup_mode,
                manager_settings,
            )))
        }),
    );
    if let Err(error) = &result {
        show_startup_error(&error.to_string());
    }
    result
}

#[cfg(windows)]
fn show_startup_error(message: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};

    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = localized_text!("msg_0449")
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            message.as_ptr(),
            title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
fn show_startup_error(message: &str) {
    eprintln!("{message}");
}

#[cfg(windows)]
struct SingleInstance(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl Drop for SingleInstance {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

#[cfg(windows)]
fn acquire_single_instance() -> Result<Option<SingleInstance>, String> {
    use windows_sys::Win32::Foundation::{GetLastError, SetLastError, ERROR_ALREADY_EXISTS};
    use windows_sys::Win32::System::Threading::CreateMutexW;

    let name: Vec<u16> = "Local\\AIMToolkit.RamDiskManager"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    unsafe { SetLastError(0) };
    let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
    if handle.is_null() {
        return Err(localized_format!("msg_0519", unsafe { GetLastError() }));
    }
    if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
        return Ok(None);
    }
    Ok(Some(SingleInstance(handle)))
}

#[cfg(not(windows))]
struct SingleInstance;

#[cfg(not(windows))]
fn acquire_single_instance() -> Result<Option<SingleInstance>, String> {
    Ok(Some(SingleInstance))
}

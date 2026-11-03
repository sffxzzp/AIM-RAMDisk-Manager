use crate::aim::{AimSnapshot, DeviceIdentity};
use crate::i18n::{self, Language};
use crate::profiles::{
    new_profile_id, ActiveDiskModeStore, DiskBackend, DiskProfile, FileSystem,
    ManagerSettingsStore, MemoryMode, MountTargetKey, PendingCreateStore, ProfileStore,
};
use crate::ramdyn::{self, RuntimePaths};
use crate::system::{self, MemorySnapshot};
use eframe::egui;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

const MEMORY_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const STATUS_SPINNER_INTERVAL: Duration = Duration::from_millis(100);
const NATIVE_AWE_CREATE_WARNING_AFTER: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MainTab {
    Disks,
    Profiles,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SizeUnit {
    Mib,
    Gib,
}

impl SizeUnit {
    fn label(self) -> &'static str {
        match self {
            Self::Mib => "MiB",
            Self::Gib => "GiB",
        }
    }

    fn multiplier(self) -> u64 {
        match self {
            Self::Mib => 1,
            Self::Gib => 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EditorMode {
    CreateAndMount,
    SaveConfig,
    Edit,
}

#[derive(Clone)]
struct ProfileForm {
    profile: DiskProfile,
    size_value: String,
    size_unit: SizeUnit,
    image_file_path: String,
    sync_directory_enabled: bool,
    sync_directory_path: String,
    mount_as_directory: bool,
    directory_path: String,
    save_after_create: bool,
    mode: EditorMode,
    error: Option<String>,
}

impl ProfileForm {
    fn new(mode: EditorMode) -> Self {
        let letter = system::available_drive_letters()
            .into_iter()
            .find(|letter| ('R'..='Z').contains(letter))
            .or_else(|| system::available_drive_letters().into_iter().next())
            .unwrap_or('R');
        Self {
            profile: DiskProfile {
                id: new_profile_id(),
                name: localized_text!("msg_0037").to_owned(),
                size_mib: 4096,
                drive_letter: letter,
                volume_label: "RamDisk".to_owned(),
                memory_mode: MemoryMode::Awe,
                backend: DiskBackend::RamDyn,
                adapter_port: None,
                file_system: FileSystem::Ntfs,
                block_size_exponent: 20,
                removable_media: false,
                mount_directory: None,
                auto_mount: false,
                sync_directory: None,
                image_file: None,
            },
            size_value: "4".to_owned(),
            size_unit: SizeUnit::Gib,
            image_file_path: String::new(),
            sync_directory_enabled: false,
            sync_directory_path: String::new(),
            mount_as_directory: false,
            directory_path: String::new(),
            save_after_create: false,
            mode,
            error: None,
        }
    }

    fn from_profile(profile: DiskProfile) -> Self {
        let (size_value, size_unit) = if profile.size_mib % 1024 == 0 {
            ((profile.size_mib / 1024).to_string(), SizeUnit::Gib)
        } else {
            (profile.size_mib.to_string(), SizeUnit::Mib)
        };
        Self {
            mount_as_directory: profile.mount_directory.is_some(),
            directory_path: profile.mount_directory.clone().unwrap_or_default(),
            image_file_path: profile.image_file.clone().unwrap_or_default(),
            sync_directory_enabled: profile.sync_directory.is_some(),
            sync_directory_path: profile.sync_directory.clone().unwrap_or_default(),
            profile,
            size_value,
            size_unit,
            save_after_create: false,
            mode: EditorMode::Edit,
            error: None,
        }
    }

    fn for_create(profile: DiskProfile, save_after_create: bool) -> Self {
        let mut form = Self::from_profile(profile);
        form.mode = EditorMode::CreateAndMount;
        form.save_after_create = save_after_create;
        form
    }

    fn parsed_profile(&self) -> Result<DiskProfile, String> {
        let image_file = (self.profile.backend == DiskBackend::AimImageFile)
            .then(|| self.image_file_path.trim().to_owned());
        let size_mib = if let Some(path) = image_file.as_deref() {
            let size_bytes = system::image_file_size_bytes(path)?;
            size_bytes.div_ceil(1024 * 1024)
        } else {
            self.size_value
                .trim()
                .parse::<u64>()
                .map_err(|_| localized_text!("msg_0260").to_owned())?
                .checked_mul(self.size_unit.multiplier())
                .ok_or_else(|| localized_text!("msg_0349").to_owned())?
        };
        let profile = DiskProfile {
            size_mib,
            drive_letter: self.profile.drive_letter.to_ascii_uppercase(),
            name: self.profile.name.trim().to_owned(),
            volume_label: self.profile.volume_label.trim().to_owned(),
            mount_directory: self
                .mount_as_directory
                .then(|| self.directory_path.trim().to_owned()),
            image_file,
            sync_directory: (self.profile.backend != DiskBackend::AimImageFile
                && self.sync_directory_enabled)
                .then(|| self.sync_directory_path.trim().to_owned()),
            ..self.profile.clone()
        };
        profile.validate()?;
        if profile.backend == DiskBackend::RamDyn {
            profile.ramdyn_size_kib()?;
        } else if profile.backend != DiskBackend::AimImageFile {
            profile.expected_device_size_bytes()?;
        }
        Ok(profile)
    }

    fn requested_capacity_bytes(&self) -> Option<u64> {
        if self.profile.backend == DiskBackend::AimImageFile {
            return None;
        }
        let value = self.size_value.trim().parse::<u64>().ok()?;
        value
            .checked_mul(self.size_unit.multiplier())?
            .checked_mul(1024 * 1024)
    }

    fn title(&self) -> &'static str {
        match self.mode {
            EditorMode::CreateAndMount => localized_text!("msg_0554"),
            EditorMode::SaveConfig => localized_text!("msg_0272"),
            EditorMode::Edit => localized_text!("msg_0398"),
        }
    }

    fn submit_label(&self) -> &'static str {
        match self.mode {
            EditorMode::CreateAndMount => localized_text!("msg_0534"),
            EditorMode::SaveConfig | EditorMode::Edit => localized_text!("msg_0258"),
        }
    }
}

fn profile_matches_active_device(profile: &DiskProfile, device: &crate::aim::AimDevice) -> bool {
    if profile.backend != DiskBackend::RamDyn
        || !device.is_hard_disk_proxy()
        || device.volume_guids.len() != 1
        || profile
            .adapter_port
            .is_some_and(|port| port != device.identity.port)
    {
        return false;
    }
    let Some(volume_guid) = device.identity.volume_guid.as_deref() else {
        return false;
    };
    if !device.volume_guids[0].eq_ignore_ascii_case(volume_guid) {
        return false;
    }

    let expected_device_size = profile.expected_device_size_bytes().ok();
    if expected_device_size != Some(device.identity.size_bytes) {
        return false;
    }

    let volume_label = profile.volume_label.trim();
    if volume_label.is_empty() {
        return false;
    }
    device
        .mount_points
        .iter()
        .filter(|mount| {
            mount.aim_device_count == 1
                && mount.extent_count == 1
                && mount
                    .volume_guid
                    .as_deref()
                    .is_some_and(|guid| guid.eq_ignore_ascii_case(volume_guid))
                && mount
                    .volume_label
                    .as_deref()
                    .is_some_and(|label| label.trim().eq_ignore_ascii_case(volume_label))
                && MountTargetKey::from_mount_path(&mount.path) == profile.mount_target_key()
        })
        .count()
        == 1
}

fn single_volume_guid_for_identity(identity: &DeviceIdentity) -> Option<&str> {
    identity.volume_guid.as_deref().or_else(|| {
        (identity.volume_guids.len() == 1).then(|| identity.volume_guids[0].as_str())
    })
}

fn created_identity_matches(returned: &DeviceIdentity, current: &DeviceIdentity) -> bool {
    if !returned.occupies_same_adapter_slot(current)
        || returned.physical_drive_number != current.physical_drive_number
        || returned.size_bytes != current.size_bytes
        || returned.flags != current.flags
    {
        return false;
    }

    let volume_sets_match = returned.volume_guids.is_empty()
        || current.volume_guids.is_empty()
        || (returned.volume_guids.len() == current.volume_guids.len()
            && returned.volume_guids.iter().all(|guid| {
                current
                    .volume_guids
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(guid))
            }));
    let single_volume_matches = match (
        returned.volume_guid.as_deref(),
        current.volume_guid.as_deref(),
    ) {
        (Some(returned), Some(current)) => returned.eq_ignore_ascii_case(current),
        _ => true,
    };
    volume_sets_match && single_volume_matches
}

#[cfg(test)]
mod profile_identity_tests {
    use super::profile_matches_active_device;
    use crate::aim::{AimDevice, DeviceIdentity, MountPoint, VolumeAssociation};
    use crate::profiles::{DiskBackend, DiskProfile, FileSystem, MemoryMode};

    fn profile(adapter_port: Option<u8>) -> DiskProfile {
        DiskProfile {
            id: "test-profile".to_owned(),
            name: "Test".to_owned(),
            size_mib: 64,
            drive_letter: 'R',
            volume_label: "CACHE".to_owned(),
            memory_mode: MemoryMode::Awe,
            backend: DiskBackend::RamDyn,
            adapter_port,
            file_system: FileSystem::Ntfs,
            block_size_exponent: 20,
            removable_media: false,
            mount_directory: None,
            auto_mount: false,
            sync_directory: None,
            image_file: None,
        }
    }

    fn device(port: u8) -> AimDevice {
        let guid = r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\";
        AimDevice {
            identity: DeviceIdentity {
                port,
                number: 7,
                physical_drive_number: Some(8),
                size_bytes: 64 * 1024 * 1024 + 4096,
                flags: 0x0000_3310,
                volume_guids: vec![guid.to_owned()],
                volume_guid: Some(guid.to_owned()),
            },
            raw_flags: 0x0000_3310,
            mount_scan_complete: true,
            volume_guids: vec![guid.to_owned()],
            volume_associations: vec![VolumeAssociation {
                volume_guid: guid.to_owned(),
                aim_device_count: 1,
                extent_count: 1,
            }],
            mount_points: vec![MountPoint {
                path: r"R:\".to_owned(),
                volume_label: Some("CACHE".to_owned()),
                volume_guid: Some(guid.to_owned()),
                aim_device_count: 1,
                extent_count: 1,
            }],
        }
    }

    #[test]
    fn saved_adapter_port_is_part_of_legacy_mode_matching() {
        let live = device(3);
        assert!(profile_matches_active_device(&profile(Some(3)), &live));
        assert!(!profile_matches_active_device(&profile(Some(4)), &live));
        assert!(profile_matches_active_device(&profile(None), &live));
    }
}

enum Modal {
    Help,
    ProfileEditor(ProfileForm),
    NativeAweCreateSlow,
    ConfirmUnmount {
        identity: DeviceIdentity,
        focus_cancel: bool,
    },
    ConfirmDeleteProfile {
        id: String,
        focus_cancel: bool,
    },
    ConfirmClearPendingCreate {
        mount_target: MountTargetKey,
        focus_cancel: bool,
    },
}

#[derive(Clone)]
enum WorkerCommand {
    Refresh,
    Create(DiskProfile),
    Unmount(DeviceIdentity, bool, Option<String>),
    QueryLoginStartup,
    SetLoginStartup(bool),
}

#[derive(Clone)]
enum WorkerOperation {
    Create(DiskProfile),
    Unmount(DeviceIdentity, bool),
}

enum WorkerActionResult {
    Created(DeviceIdentity),
    Unmounted {
        mount_point: String,
        persistence_warning: Option<String>,
    },
}

enum WorkerActionError {
    Create(ramdyn::CreateError),
    Unmount(crate::aim::UnmountError),
}

#[derive(Clone, Copy)]
enum UnmountReconciliation {
    ConfirmedGone,
    StillPresent,
    SlotChanged,
    Unknown,
}

enum WorkerEvent {
    Snapshot(Result<AimSnapshot, String>),
    LoginStartupState(Result<bool, String>),
    LoginStartupChanged(bool, Result<(), String>),
    Completed {
        operation: WorkerOperation,
        result: Result<WorkerActionResult, WorkerActionError>,
        snapshot: Result<AimSnapshot, String>,
    },
}

struct PendingOperation {
    create_form: Option<ProfileForm>,
    unmount_identity: Option<DeviceIdentity>,
    label: &'static str,
    started_at: Instant,
    slow_awe_warning_shown: bool,
}

struct Notice {
    text: String,
    is_error: bool,
}

struct NoticeTimeout {
    text: String,
    is_error: bool,
    deadline: Instant,
}

pub struct ManagerApp {
    profiles: ProfileStore,
    pending_creates: PendingCreateStore,
    active_disk_modes: ActiveDiskModeStore,
    unrecorded_sync_directories: HashMap<String, (u64, String)>,
    manager_settings: ManagerSettingsStore,
    language_override: Option<Language>,
    runtime: RuntimePaths,
    runtime_error: Option<String>,
    worker_context: egui::Context,
    unsaved_profiles: Vec<DiskProfile>,
    worker: Option<Sender<WorkerCommand>>,
    events: Receiver<WorkerEvent>,
    tab: MainTab,
    snapshot: Option<AimSnapshot>,
    snapshot_error: Option<String>,
    refresh_in_flight: bool,
    discard_refresh_result: bool,
    selected_device: Option<DeviceIdentity>,
    created_modes: HashMap<DeviceIdentity, MemoryMode>,
    mode_store_warning_reported: bool,
    unresolved_create_targets: HashSet<MountTargetKey>,
    unresolved_unmounts: HashSet<DeviceIdentity>,
    pending: Option<PendingOperation>,
    modal: Option<Modal>,
    memory: Option<MemorySnapshot>,
    memory_error: Option<String>,
    lock_memory_privilege: Result<bool, String>,
    large_page_minimum_bytes: Option<u64>,
    last_memory_sample: Instant,
    window_was_focused: bool,
    notice: Option<Notice>,
    notice_timeout: Option<NoticeTimeout>,
    login_startup_enabled: Option<bool>,
    login_startup_change_pending: bool,
    login_startup_error: Option<String>,
    startup_waiting_for_snapshot: bool,
    startup_run_active: bool,
    startup_queue: VecDeque<DiskProfile>,
    startup_active_profile_id: Option<String>,
    startup_results: Vec<String>,
    startup_result_is_error: bool,
    font_warning: Option<String>,
    close_prompt: bool,
    allow_close: bool,
}

fn show_styled_dialog<T>(
    context: &egui::Context,
    id: &'static str,
    title: &str,
    width: f32,
    content: impl FnOnce(&mut egui::Ui) -> T,
) -> egui::ModalResponse<T> {
    let style = context.style();
    let screen = context.screen_rect();
    let width = width.min((screen.width() - 48.0).max(280.0));
    let max_content_height = (screen.height() - 144.0).max(180.0);
    let frame = egui::Frame::popup(&style)
        .inner_margin(egui::Margin::same(20))
        .corner_radius(egui::CornerRadius::same(8))
        .stroke(egui::Stroke::new(
            1.0_f32,
            style.visuals.window_stroke().color,
        ));
    let backdrop_alpha = if style.visuals.dark_mode { 150 } else { 96 };

    egui::Modal::new(egui::Id::new(id))
        .frame(frame)
        .backdrop_color(egui::Color32::from_black_alpha(backdrop_alpha))
        .show(context, |ui| {
            ui.set_min_width(width);
            ui.set_max_width(width);
            ui.heading(egui::RichText::new(title).size(19.0).strong());
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(12.0);
            egui::ScrollArea::vertical()
                .id_salt((id, "content"))
                .max_height(max_content_height)
                .show(ui, content)
                .inner
        })
}

fn dialog_footer(ui: &mut egui::Ui, actions: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(12.0);
    ui.separator();
    ui.add_space(10.0);
    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), actions);
}

fn secondary_dialog_button(label: &str) -> egui::Button<'_> {
    egui::Button::new(label).min_size(egui::vec2(96.0, 34.0))
}

fn primary_dialog_button(label: &str, dark_mode: bool) -> egui::Button<'_> {
    let fill = if dark_mode {
        egui::Color32::from_rgb(55, 119, 201)
    } else {
        egui::Color32::from_rgb(35, 97, 164)
    };
    egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE))
        .fill(fill)
        .min_size(egui::vec2(116.0, 34.0))
}

fn danger_dialog_button(label: &str, dark_mode: bool) -> egui::Button<'_> {
    let fill = if dark_mode {
        egui::Color32::from_rgb(174, 61, 61)
    } else {
        egui::Color32::from_rgb(176, 45, 45)
    };
    egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE))
        .fill(fill)
        .min_size(egui::vec2(116.0, 34.0))
}

fn warning_dialog_button(label: &str, dark_mode: bool) -> egui::Button<'_> {
    let fill = if dark_mode {
        egui::Color32::from_rgb(155, 100, 30)
    } else {
        egui::Color32::from_rgb(145, 86, 14)
    };
    egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE))
        .fill(fill)
        .min_size(egui::vec2(116.0, 34.0))
}

fn retranslate_known_text(text: &mut String, from: Language, to: Language) {
    if let Some(translated) = i18n::translate_rendered(text, from, to) {
        *text = translated;
    }
}

fn retranslate_optional_text(text: &mut Option<String>, from: Language, to: Language) {
    if let Some(text) = text {
        retranslate_known_text(text, from, to);
    }
}

impl ManagerApp {
    pub fn new(
        context: &egui::Context,
        startup_mode: bool,
        manager_settings: ManagerSettingsStore,
    ) -> Self {
        let language_override = manager_settings.language_override();
        i18n::set_language(language_override.unwrap_or_else(Language::system_default));
        let font_warning = install_chinese_font(context);
        let profiles = ProfileStore::load();
        let pending_creates = PendingCreateStore::load();
        let active_disk_modes = ActiveDiskModeStore::load();
        let unresolved_create_targets: HashSet<MountTargetKey> = pending_creates
            .pending_creates()
            .iter()
            .map(|entry| entry.profile.mount_target_key())
            .collect();
        let mut startup_queue = VecDeque::new();
        let mut startup_results = Vec::new();
        let mut startup_result_is_error = false;
        if startup_mode {
            if let Some(error) = profiles.load_error() {
                startup_results.push(localized_format!("msg_0240", error = error));
                startup_result_is_error = true;
            } else {
                let mut startup_targets = HashSet::new();
                for profile in profiles
                    .profiles()
                    .iter()
                    .filter(|profile| profile.auto_mount)
                {
                    if startup_targets.insert(profile.mount_target_key()) {
                        startup_queue.push_back(profile.clone());
                    } else {
                        startup_results.push(localized_format!(
                            "msg_0374",
                            profile.name,
                            profile.mount_target_display()
                        ));
                        startup_result_is_error = true;
                    }
                }
            }
        }
        let runtime = RuntimePaths::discover();
        let runtime_error = runtime.validate_aimapi().err();
        let (event_sender, events) = mpsc::channel();
        drop(event_sender);
        let mut app = Self {
            profiles,
            pending_creates,
            active_disk_modes,
            unrecorded_sync_directories: HashMap::new(),
            manager_settings,
            language_override,
            runtime,
            runtime_error,
            worker_context: context.clone(),
            unsaved_profiles: Vec::new(),
            worker: None,
            events,
            tab: MainTab::Disks,
            snapshot: None,
            snapshot_error: Some(localized_text!("msg_0183").to_owned()),
            refresh_in_flight: false,
            discard_refresh_result: false,
            selected_device: None,
            created_modes: HashMap::new(),
            mode_store_warning_reported: false,
            unresolved_create_targets,
            unresolved_unmounts: HashSet::new(),
            pending: None,
            modal: None,
            memory: None,
            memory_error: None,
            lock_memory_privilege: system::lock_memory_privilege_available(),
            large_page_minimum_bytes: system::large_page_minimum_bytes(),
            last_memory_sample: Instant::now() - MEMORY_REFRESH_INTERVAL,
            window_was_focused: false,
            notice: None,
            notice_timeout: None,
            login_startup_enabled: None,
            login_startup_change_pending: false,
            login_startup_error: None,
            startup_waiting_for_snapshot: startup_mode,
            startup_run_active: false,
            startup_queue,
            startup_active_profile_id: None,
            startup_results,
            startup_result_is_error,
            font_warning,
            close_prompt: false,
            allow_close: false,
        };
        app.request_login_startup_state();
        app.request_refresh();
        app
    }

    fn request_refresh(&mut self) {
        let timed_out_awe_can_be_refreshed = self
            .pending
            .as_ref()
            .is_some_and(|operation| operation.slow_awe_warning_shown);
        if (self.pending.is_none() || timed_out_awe_can_be_refreshed)
            && !self.refresh_in_flight
        {
            self.refresh_memory();
            self.refresh_runtime_status();
            if let Err(error) = self.ensure_worker() {
                self.snapshot_error = Some(error.clone());
                self.fail_startup_scan(&error);
                return;
            }
            self.refresh_in_flight = true;
            let sent = self
                .worker
                .as_ref()
                .is_some_and(|worker| worker.send(WorkerCommand::Refresh).is_ok());
            if !sent {
                self.refresh_in_flight = false;
                let error = localized_text!("msg_0092").to_owned();
                self.snapshot_error = Some(error.clone());
                self.fail_startup_scan(&error);
            } else {
                self.snapshot_error = None;
            }
        }
    }

    fn fail_startup_scan(&mut self, error: &str) {
        if !self.startup_waiting_for_snapshot {
            return;
        }
        self.startup_queue.clear();
        let message = localized_format!("msg_0257", error = error);
        self.startup_results.push(message);
        self.startup_result_is_error = true;
        let results = std::mem::take(&mut self.startup_results);
        self.complete_startup_run(results, true, false);
    }

    fn ensure_worker(&mut self) -> Result<(), String> {
        if self.worker.is_some() {
            return Ok(());
        }
        let (worker, events) = start_worker(self.worker_context.clone(), self.runtime.clone())?;
        self.worker = Some(worker);
        self.events = events;
        Ok(())
    }

    fn open_new_disk(&mut self) {
        if self.refresh_in_flight {
            self.notice = Some(Notice {
                text: localized_text!("msg_0391").to_owned(),
                is_error: true,
            });
            return;
        }
        if let Some(error) = self.aim_state_unavailable_reason().map(str::to_owned) {
            self.notice = Some(Notice {
                text: localized_format!("msg_0187", error = error),
                is_error: true,
            });
            return;
        }
        self.refresh_runtime_status();
        if let Some(error) = &self.runtime_error {
            self.notice = Some(Notice {
                text: localized_format!("msg_0206", error = error),
                is_error: true,
            });
            return;
        }
        if let Some(error) = self.pending_creates.load_error() {
            self.notice = Some(Notice {
                text: localized_format!("msg_0558", error = error),
                is_error: true,
            });
            return;
        }
        if self.pending.is_none() && self.modal.is_none() && !self.close_prompt {
            self.refresh_memory();
            let mut form = ProfileForm::new(EditorMode::CreateAndMount);
            form.profile.adapter_port = self
                .snapshot
                .as_ref()
                .and_then(AimSnapshot::preferred_adapter_port);
            self.modal = Some(Modal::ProfileEditor(form));
        }
    }

    fn handle_keyboard_shortcuts(&mut self, context: &egui::Context) {
        let (new_disk, refresh, escape) = context.input_mut(|input| {
            (
                input.consume_key(egui::Modifiers::CTRL, egui::Key::N),
                input.consume_key(egui::Modifiers::NONE, egui::Key::F5),
                input.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            )
        });

        if escape {
            if self.close_prompt {
                self.close_prompt = false;
            } else if self.modal.is_some() {
                self.modal = None;
            } else {
                self.selected_device = None;
            }
        }
        if refresh {
            self.request_refresh();
        }
        if new_disk {
            self.open_new_disk();
        }
    }

    fn refresh_memory(&mut self) {
        self.last_memory_sample = Instant::now();
        match system::memory_snapshot() {
            Ok(snapshot) => {
                self.memory = Some(snapshot);
                self.memory_error = None;
            }
            Err(error) => {
                self.memory = None;
                self.memory_error = Some(error);
            }
        }
    }

    fn refresh_runtime_status(&mut self) {
        self.runtime_error = self.runtime.validate_aimapi().err();
    }

    fn runtime_error_for_profile(&self, profile: &DiskProfile) -> Option<String> {
        match profile.backend {
            DiskBackend::RamDyn => self.runtime.validate_components().err(),
            DiskBackend::AimVirtualMemory
            | DiskBackend::AimAweAlloc
            | DiskBackend::AimImageFile => self.runtime.validate_aimapi().err(),
        }
    }

    fn request_login_startup_state(&mut self) {
        if let Err(error) = self.ensure_worker() {
            self.login_startup_error = Some(error);
            return;
        }
        let sent = self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.send(WorkerCommand::QueryLoginStartup).is_ok());
        if !sent {
            self.login_startup_error = Some(localized_text!("msg_0285").to_owned());
        }
    }

    fn set_login_startup(&mut self, enabled: bool) {
        if self.login_startup_change_pending {
            return;
        }
        if let Err(error) = self.ensure_worker() {
            self.login_startup_error = Some(error);
            return;
        }
        let sent = self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.send(WorkerCommand::SetLoginStartup(enabled)).is_ok());
        if sent {
            self.login_startup_change_pending = true;
            self.login_startup_error = None;
        } else {
            self.login_startup_error = Some(localized_text!("msg_0124").to_owned());
        }
    }

    fn set_language_override(
        &mut self,
        language_override: Option<Language>,
        context: &egui::Context,
    ) {
        if self.language_override == language_override {
            return;
        }
        self.language_override = language_override;
        let previous_language = i18n::current_language();
        let language = language_override.unwrap_or_else(Language::system_default);
        i18n::set_language(language);
        if previous_language != language {
            for result in &mut self.startup_results {
                retranslate_known_text(result, previous_language, language);
            }
            if let Some(notice) = &mut self.notice {
                retranslate_known_text(&mut notice.text, previous_language, language);
            }
            if let Some(notice) = &mut self.notice_timeout {
                retranslate_known_text(&mut notice.text, previous_language, language);
            }
            retranslate_optional_text(&mut self.runtime_error, previous_language, language);
            retranslate_optional_text(&mut self.snapshot_error, previous_language, language);
            retranslate_optional_text(&mut self.memory_error, previous_language, language);
            retranslate_optional_text(&mut self.login_startup_error, previous_language, language);
            retranslate_optional_text(&mut self.font_warning, previous_language, language);
            if let Err(error) = &mut self.lock_memory_privilege {
                retranslate_known_text(error, previous_language, language);
            }
            if let Some(Modal::ProfileEditor(form)) = &mut self.modal {
                if let Some(error) = &mut form.error {
                    retranslate_known_text(error, previous_language, language);
                }
            }
        }
        context.send_viewport_cmd(egui::ViewportCommand::Title(
            localized_text!("msg_0449").to_owned(),
        ));
        let save_result = self
            .manager_settings
            .set_language_override(language_override);
        let (text, is_error) = match save_result {
            Ok(()) => (localized_text!("msg_0116").to_owned(), false),
            Err(error) => (format!("{} {error}", localized_text!("msg_0499")), true),
        };
        self.notice = Some(Notice { text, is_error });
    }

    fn set_close_after_startup(&mut self, enabled: bool) {
        match self.manager_settings.set_close_after_startup(enabled) {
            Ok(()) => {
                self.notice = Some(Notice {
                    text: if enabled {
                        localized_text!("msg_0390").to_owned()
                    } else {
                        localized_text!("msg_0044").to_owned()
                    },
                    is_error: false,
                });
            }
            Err(error) => {
                self.notice = Some(Notice {
                    text: error,
                    is_error: true,
                });
            }
        }
    }

    fn aim_state_unavailable_reason(&self) -> Option<&str> {
        if let Some(error) = self.snapshot_error.as_deref() {
            Some(error)
        } else if self.snapshot.is_none() {
            Some(localized_text!("msg_0108"))
        } else if let Some(snapshot) = self
            .snapshot
            .as_ref()
            .filter(|snapshot| !snapshot.inventory_complete())
        {
            Some(
                snapshot
                    .adapter_scan_error
                    .as_deref()
                    .or(snapshot.mount_scan_error.as_deref())
                    .unwrap_or(localized_text!("msg_0393")),
            )
        } else {
            None
        }
    }

    fn memory_mode_unavailable_reason(&self, mode: MemoryMode) -> Option<String> {
        if mode.requires_lock_memory_privilege() {
            match &self.lock_memory_privilege {
                Ok(true) => {}
                Ok(false) => return Some(localized_text!("msg_0022").to_owned()),
                Err(error) => return Some(localized_format!("msg_0328", error = error)),
            }
        }
        if mode == MemoryMode::LargePage && self.large_page_minimum_bytes.is_none() {
            return Some(localized_text!("msg_0199").to_owned());
        }
        None
    }

    fn start_create(&mut self, profile: DiskProfile, mut form: ProfileForm) {
        form.profile = profile.clone();
        match self.begin_create(&profile, form.clone()) {
            Ok(()) => self.notice = None,
            Err(error) => {
                form.error = Some(error.clone());
                self.modal = Some(Modal::ProfileEditor(form));
                self.notice = Some(Notice {
                    text: error,
                    is_error: true,
                });
            }
        }
    }

    fn begin_create(&mut self, profile: &DiskProfile, form: ProfileForm) -> Result<(), String> {
        if self.refresh_in_flight {
            return Err(localized_text!("msg_0405").to_owned());
        }
        if let Some(error) = self.aim_state_unavailable_reason() {
            return Err(localized_format!("msg_0031", error = error));
        }
        profile.validate()?;
        profile.expected_device_size_bytes()?;
        let target_key = profile.mount_target_key();
        if self.unresolved_create_targets.contains(&target_key) {
            return Err(localized_format!(
                "msg_0426",
                profile.mount_target_display()
            ));
        }
        if let Some(error) = self.runtime_error_for_profile(profile) {
            return Err(localized_format!("msg_0284", error = error));
        }
        if profile.backend == DiskBackend::RamDyn {
            if let Some(error) = self.memory_mode_unavailable_reason(profile.memory_mode) {
                return Err(error);
            }
        }
        if !system::mount_target_is_available(profile)? {
            return Err(localized_format!(
                "msg_0543",
                profile.mount_target_display()
            ));
        }
        if let Some(error) = self.pending_creates.load_error() {
            return Err(localized_format!("msg_0558", error = error));
        }
        self.ensure_worker()
            .map_err(|error| localized_format!("msg_0105", error = error))?;
        self.refresh_memory();
        self.pending_creates.begin(profile.clone())?;
        self.unresolved_create_targets.insert(target_key.clone());
        let sent = self
            .worker
            .as_ref()
            .is_some_and(|worker| worker.send(WorkerCommand::Create(profile.clone())).is_ok());
        if !sent {
            let error = match self.clear_pending_create_guard(&target_key) {
                Ok(()) => localized_text!("msg_0332").to_owned(),
                Err(error) => {
                    localized_format!("msg_0358", error = error)
                }
            };
            return Err(error);
        }
        self.pending = Some(PendingOperation {
            create_form: Some(form),
            unmount_identity: None,
            label: match profile.backend {
                DiskBackend::RamDyn => localized_text!("msg_0101"),
                DiskBackend::AimVirtualMemory => localized_text!("msg_0455"),
                DiskBackend::AimAweAlloc => localized_text!("msg_0563"),
                DiskBackend::AimImageFile => localized_text!("msg_0588"),
            },
            started_at: Instant::now(),
            slow_awe_warning_shown: false,
        });
        Ok(())
    }

    fn clear_pending_create_guard(&mut self, target: &MountTargetKey) -> Result<(), String> {
        self.pending_creates.resolve(target)?;
        self.unresolved_create_targets.remove(target);
        Ok(())
    }

    fn advance_startup_mount_queue(&mut self) {
        if !self.startup_run_active || self.pending.is_some() {
            return;
        }
        loop {
            let Some(profile) = self.startup_queue.pop_front() else {
                let results = std::mem::take(&mut self.startup_results);
                let has_errors = std::mem::take(&mut self.startup_result_is_error);
                self.complete_startup_run(results, has_errors, true);
                return;
            };

            let form = ProfileForm::for_create(profile.clone(), false);
            match self.begin_create(&profile, form) {
                Ok(()) => {
                    self.startup_active_profile_id = Some(profile.id);
                    self.notice = None;
                    return;
                }
                Err(error) => {
                    self.startup_result_is_error = true;
                    self.startup_results.push(localized_format!(
                        "msg_0226",
                        profile.name,
                        error = error
                    ));
                }
            }
        }
    }

    fn complete_startup_run(
        &mut self,
        mut results: Vec<String>,
        has_errors: bool,
        allow_auto_close: bool,
    ) {
        self.startup_waiting_for_snapshot = false;
        self.startup_run_active = false;
        self.startup_active_profile_id = None;
        if results.is_empty() {
            results.push(localized_text!("msg_0384").to_owned());
        }
        let text = results.join(" ");
        let saved =
            self.manager_settings
                .record_startup_run(results, has_errors, i18n::current_language());
        let history_saved = saved.is_ok();
        let mut notice_text = text;
        let mut notice_is_error = has_errors;
        if let Err(error) = saved {
            notice_text.push_str(&localized_format!("msg_0158", error = error));
            notice_is_error = true;
        }
        self.notice = Some(Notice {
            text: notice_text,
            is_error: notice_is_error,
        });

        let can_close = allow_auto_close
            && history_saved
            && self.manager_settings.close_after_startup()
            && self.pending.is_none()
            && self.snapshot_error.is_none()
            && self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.mount_scan_complete)
            && self.unresolved_create_targets.is_empty()
            && self.unresolved_unmounts.is_empty();
        if can_close {
            self.allow_close = true;
            self.worker_context
                .send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn start_unmount(&mut self, identity: DeviceIdentity, force: bool) {
        if self.refresh_in_flight {
            self.notice = Some(Notice {
                text: localized_text!("msg_0316").to_owned(),
                is_error: true,
            });
            return;
        }
        if self.unresolved_unmounts.contains(&identity) {
            self.notice = Some(Notice {
                text: localized_text!("msg_0118").to_owned(),
                is_error: true,
            });
            return;
        }
        if let Err(error) = self.ensure_worker() {
            self.notice = Some(Notice {
                text: localized_format!("msg_0071", error = error),
                is_error: true,
            });
            return;
        }
        let volume_key = identity
            .volume_guid
            .as_deref()
            .map(|guid| guid.to_ascii_lowercase());
        let sync_directory = identity
            .volume_guid
            .as_deref()
            .and_then(|guid| {
                self.active_disk_modes
                    .sync_directory_for(guid, identity.size_bytes)
                    .map(str::to_owned)
            })
            .or_else(|| {
                volume_key
                    .as_deref()
                    .and_then(|key| self.unrecorded_sync_directories.get(key))
                    .filter(|(size_bytes, _)| *size_bytes == identity.size_bytes)
                    .map(|(_, directory)| directory.clone())
            });
        let sent = self.worker.as_ref().is_some_and(|worker| {
            worker
                .send(WorkerCommand::Unmount(
                    identity.clone(),
                    force,
                    sync_directory,
                ))
                .is_ok()
        });
        if !sent {
            self.notice = Some(Notice {
                text: localized_text!("msg_0441").to_owned(),
                is_error: true,
            });
            return;
        }
        self.pending = Some(PendingOperation {
            create_form: None,
            unmount_identity: Some(identity),
            label: if force {
                localized_text!("msg_0019")
            } else {
                localized_text!("msg_0424")
            },
            started_at: Instant::now(),
            slow_awe_warning_shown: false,
        });
        self.notice = None;
    }

    fn close_unmount_confirmation(&mut self, identity: &DeviceIdentity) {
        let is_matching_confirmation = self.modal.as_ref().is_some_and(|modal| {
            matches!(
                modal,
                Modal::ConfirmUnmount {
                    identity: modal_identity,
                    ..
                } if modal_identity == identity
            )
        });
        if is_matching_confirmation {
            self.modal = None;
        }
    }

    fn consume_events(&mut self) {
        if self.worker.is_none() {
            return;
        }
        loop {
            match self.events.try_recv() {
                Ok(WorkerEvent::Snapshot(result)) => {
                    self.refresh_in_flight = false;
                    if self.discard_refresh_result {
                        self.discard_refresh_result = false;
                    } else {
                        self.apply_snapshot_result(result);
                    }
                }
                Ok(WorkerEvent::LoginStartupState(result)) => match result {
                    Ok(enabled) => {
                        self.login_startup_enabled = Some(enabled);
                        self.login_startup_error = None;
                    }
                    Err(error) => {
                        self.login_startup_enabled = None;
                        self.login_startup_error = Some(error);
                    }
                },
                Ok(WorkerEvent::LoginStartupChanged(enabled, result)) => {
                    self.login_startup_change_pending = false;
                    match result {
                        Ok(()) => {
                            self.login_startup_enabled = Some(enabled);
                            self.login_startup_error = None;
                            self.notice = Some(Notice {
                                text: if enabled {
                                    localized_text!("msg_0465").to_owned()
                                } else {
                                    localized_text!("msg_0555").to_owned()
                                },
                                is_error: false,
                            });
                        }
                        Err(error) => {
                            self.login_startup_error = Some(error.clone());
                            self.login_startup_enabled = None;
                            self.notice = Some(Notice {
                                text: localized_format!("msg_0111", error = error),
                                is_error: true,
                            });
                            self.request_login_startup_state();
                        }
                    }
                }
                Ok(WorkerEvent::Completed {
                    operation,
                    result,
                    snapshot,
                }) => self.finish_operation(operation, result, snapshot),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.recover_worker();
                    break;
                }
            }
        }
    }

    fn recover_worker(&mut self) {
        self.worker = None;
        self.refresh_in_flight = false;
        self.discard_refresh_result = false;
        self.login_startup_change_pending = false;
        self.login_startup_enabled = None;
        self.login_startup_error = Some(localized_text!("msg_0140").to_owned());
        let startup_was_active = self.startup_run_active || self.startup_waiting_for_snapshot;
        let interrupted_startup_profile = self.startup_active_profile_id.take();
        if self.startup_run_active {
            self.startup_run_active = false;
            self.startup_queue.clear();
        }
        self.startup_waiting_for_snapshot = false;

        if let Some(pending) = self.pending.take() {
            if let Some(mut form) = pending.create_form {
                let target = form.profile.mount_target_key();
                self.unresolved_create_targets.insert(target.clone());
                let message = localized_format!("msg_0109", target.label());
                if interrupted_startup_profile.as_deref() != Some(&form.profile.id) {
                    form.error = Some(message.clone());
                    self.modal = Some(Modal::ProfileEditor(form));
                }
                self.notice = Some(Notice {
                    text: message,
                    is_error: true,
                });
            } else if let Some(identity) = pending.unmount_identity {
                self.unresolved_unmounts.insert(identity);
                self.notice = Some(Notice {
                    text: localized_text!("msg_0003").to_owned(),
                    is_error: true,
                });
            } else {
                self.notice = Some(Notice {
                    text: localized_text!("msg_0544").to_owned(),
                    is_error: true,
                });
            }
        } else {
            if self.notice.is_none() {
                self.notice = Some(Notice {
                    text: localized_text!("msg_0288").to_owned(),
                    is_error: true,
                });
            }
        }

        if startup_was_active {
            if let Some(notice) = self.notice.take() {
                self.startup_result_is_error |= notice.is_error;
                self.startup_results.push(notice.text);
            }
            self.startup_results
                .push(localized_text!("msg_0032").to_owned());
            self.startup_result_is_error = true;
            let results = std::mem::take(&mut self.startup_results);
            self.complete_startup_run(results, true, false);
        }

        self.request_login_startup_state();
        self.request_refresh();
    }

    fn apply_snapshot_result(&mut self, result: Result<AimSnapshot, String>) {
        match result {
            Ok(snapshot) => {
                self.snapshot_error = None;
                let unresolved_unmounts = std::mem::take(&mut self.unresolved_unmounts);
                let mut missing_unmounts = 0;
                let mut present_unmounts = 0;
                let mut changed_unmounts = 0;
                let mut unverified_unmounts = 0;
                let mut incomplete_unmounts = 0;
                for identity in unresolved_unmounts {
                    if let Some(device) = snapshot
                        .devices
                        .iter()
                        .find(|device| device.identity.occupies_same_adapter_slot(&identity))
                    {
                        if !snapshot.inventory_complete() {
                            incomplete_unmounts += 1;
                            self.unresolved_unmounts.insert(identity);
                        } else if device.identity == identity {
                            present_unmounts += 1;
                        } else {
                            // Device numbers can be reused and volume mappings can
                            // change. A matching adapter slot with changed identity
                            // is not proof that the original target disappeared.
                            changed_unmounts += 1;
                        }
                    } else if !snapshot.inventory_complete() {
                        unverified_unmounts += 1;
                        self.unresolved_unmounts.insert(identity);
                    } else {
                        missing_unmounts += 1;
                    }
                }
                let observed_targets = snapshot
                    .devices
                    .iter()
                    .flat_map(|device| device.mount_points.iter())
                    .map(|mount| MountTargetKey::from_mount_path(&mount.path))
                    .collect::<HashSet<_>>();
                let observed_pending_targets = self
                    .unresolved_create_targets
                    .intersection(&observed_targets)
                    .cloned()
                    .collect::<Vec<_>>();
                let mut reconciliation = Vec::new();
                if !observed_pending_targets.is_empty() {
                    let targets = observed_pending_targets
                        .iter()
                        .map(MountTargetKey::label)
                        .collect::<Vec<_>>()
                        .join(localized_text!("msg_0562"));
                    reconciliation.push(localized_format!("msg_0283", targets = targets));
                    if let Some(Modal::ProfileEditor(form)) = self.modal.as_mut() {
                        if observed_pending_targets.contains(&form.profile.mount_target_key()) {
                            form.error = Some(localized_text!("msg_0526").to_owned());
                        }
                    }
                }
                if missing_unmounts > 0 {
                    reconciliation.push(localized_format!(
                        "msg_0014",
                        missing_unmounts = missing_unmounts
                    ));
                }
                if present_unmounts > 0 {
                    reconciliation.push(localized_format!(
                        "msg_0132",
                        present_unmounts = present_unmounts
                    ));
                }
                if changed_unmounts > 0 {
                    reconciliation.push(localized_format!(
                        "msg_0223",
                        changed_unmounts = changed_unmounts
                    ));
                }
                if unverified_unmounts > 0 {
                    reconciliation.push(localized_format!(
                        "msg_0460",
                        unverified_unmounts = unverified_unmounts
                    ));
                }
                if incomplete_unmounts > 0 {
                    reconciliation.push(localized_format!(
                        "msg_0079",
                        incomplete_unmounts = incomplete_unmounts
                    ));
                }
                if !reconciliation.is_empty() {
                    self.notice = Some(Notice {
                        text: reconciliation.join(" "),
                        is_error: !observed_pending_targets.is_empty()
                            || present_unmounts > 0
                            || changed_unmounts > 0
                            || unverified_unmounts > 0
                            || incomplete_unmounts > 0,
                    });
                }
                self.retain_live_selection(&snapshot);
                let startup_scan_complete = snapshot.mount_scan_complete;
                if snapshot.inventory_complete() {
                    let live_sync_bindings =
                        snapshot
                            .devices
                            .iter()
                            .filter_map(|device| {
                                device.identity.volume_guid.as_ref().map(|guid| {
                                    (guid.to_ascii_lowercase(), device.identity.size_bytes)
                                })
                            })
                            .collect::<HashSet<_>>();
                    self.unrecorded_sync_directories.retain(|guid, (size, _)| {
                        live_sync_bindings.contains(&(guid.clone(), *size))
                    });
                }
                self.snapshot = Some(snapshot);
                if self.startup_waiting_for_snapshot {
                    self.startup_waiting_for_snapshot = false;
                    if startup_scan_complete {
                        self.startup_run_active = true;
                        self.advance_startup_mount_queue();
                    } else {
                        self.startup_queue.clear();
                        self.startup_results
                            .push(localized_text!("msg_0149").to_owned());
                        self.startup_result_is_error = true;
                        let results = std::mem::take(&mut self.startup_results);
                        self.complete_startup_run(results, true, false);
                    }
                }
            }
            Err(error) => {
                self.snapshot_error = Some(error.clone());
                if self.startup_waiting_for_snapshot {
                    self.startup_queue.clear();
                    self.startup_results
                        .push(localized_format!("msg_0070", error = error));
                    self.startup_result_is_error = true;
                    let results = std::mem::take(&mut self.startup_results);
                    self.complete_startup_run(results, true, false);
                }
            }
        }
    }

    fn retain_live_selection(&mut self, snapshot: &AimSnapshot) {
        let live = snapshot
            .devices
            .iter()
            .map(|device| device.identity.clone())
            .collect::<HashSet<_>>();

        // Partial adapter or volume scans may omit identities, so only a
        // complete combined inventory can prove that a device disappeared.
        let inventory_is_complete = snapshot.inventory_complete();
        if inventory_is_complete {
            self.created_modes
                .retain(|identity, _| live.contains(identity));

            let live_volumes = snapshot
                .devices
                .iter()
                .filter(|device| device.volume_guids.len() == 1)
                .filter_map(|device| {
                    device
                        .identity
                        .volume_guid
                        .as_ref()
                        .map(|volume_guid| (volume_guid.clone(), device.identity.size_bytes))
                })
                .collect::<Vec<_>>();
            if let Err(error) = self.active_disk_modes.retain_live_volumes(&live_volumes) {
                self.warn_mode_store_once(localized_format!("msg_0553", error = error));
            }

            // Older versions kept the mode only in memory. Recover it once
            // when one saved profile uniquely matches one live RamDyn volume.
            let mut migrations = Vec::new();
            for device in &snapshot.devices {
                let Some(volume_guid) = device.identity.volume_guid.as_deref() else {
                    continue;
                };
                if self.active_disk_modes.contains_volume_guid(volume_guid) {
                    continue;
                }
                let matching_profiles = self
                    .profiles
                    .profiles()
                    .iter()
                    .filter(|profile| profile_matches_active_device(profile, device))
                    .collect::<Vec<_>>();
                if matching_profiles.len() == 1 {
                    let profile = matching_profiles[0];
                    migrations.push((
                        device.identity.clone(),
                        volume_guid.to_owned(),
                        profile.id.clone(),
                        profile.memory_mode,
                    ));
                }
            }
            let mut profile_matches = HashMap::<String, usize>::new();
            for (_, _, profile_id, _) in &migrations {
                *profile_matches.entry(profile_id.clone()).or_default() += 1;
            }
            let mut migrated_count = 0;
            for (identity, volume_guid, profile_id, mode) in migrations {
                if profile_matches.get(&profile_id) != Some(&1) {
                    continue;
                }
                if let Err(error) =
                    self.active_disk_modes
                        .remember(&volume_guid, identity.size_bytes, mode)
                {
                    self.created_modes.insert(identity, mode);
                    self.warn_mode_store_once(localized_format!("msg_0385", error = error));
                } else {
                    self.created_modes.insert(identity, mode);
                    migrated_count += 1;
                }
            }
            if migrated_count > 0 {
                self.append_notice(
                    localized_format!("msg_0414", migrated_count = migrated_count),
                    false,
                );
            }
        }

        for device in &snapshot.devices {
            if !device.is_hard_disk_proxy() || device.volume_guids.len() != 1 {
                continue;
            }
            let Some(volume_guid) = device.identity.volume_guid.as_deref() else {
                continue;
            };
            let associated_device_count = snapshot
                .devices
                .iter()
                .filter(|candidate| {
                    candidate
                        .volume_guids
                        .iter()
                        .any(|guid| guid.eq_ignore_ascii_case(volume_guid))
                })
                .count();
            if associated_device_count != 1
                || device.mount_points.iter().any(|mount| {
                    !mount
                        .volume_guid
                        .as_deref()
                        .is_some_and(|guid| guid.eq_ignore_ascii_case(volume_guid))
                        || mount.aim_device_count != 1
                        || mount.extent_count != 1
                })
            {
                continue;
            }
            if let Some(mode) = self
                .active_disk_modes
                .mode_for(volume_guid, device.identity.size_bytes)
            {
                self.created_modes.insert(device.identity.clone(), mode);
            }
        }
        if inventory_is_complete {
            self.created_modes
                .retain(|identity, _| live.contains(identity));
        }
        if self
            .selected_device
            .as_ref()
            .is_some_and(|identity| !live.contains(identity))
        {
            self.selected_device = None;
        }
    }

    fn append_notice(&mut self, text: String, is_error: bool) {
        if let Some(notice) = &mut self.notice {
            notice.text.push(' ');
            notice.text.push_str(&text);
            notice.is_error |= is_error;
        } else {
            self.notice = Some(Notice { text, is_error });
        }
    }

    fn warn_mode_store_once(&mut self, text: String) {
        if self.mode_store_warning_reported {
            return;
        }
        self.mode_store_warning_reported = true;
        self.append_notice(text, true);
    }

    fn remove_active_mode_record(&mut self, identity: &DeviceIdentity) -> Option<String> {
        let volume_guid = single_volume_guid_for_identity(identity)?;
        self.active_disk_modes.remove(volume_guid).err()
    }

    fn finish_operation(
        &mut self,
        operation: WorkerOperation,
        result: Result<WorkerActionResult, WorkerActionError>,
        snapshot: Result<AimSnapshot, String>,
    ) {
        if matches!(
            &operation,
            WorkerOperation::Create(profile) if profile.backend == DiskBackend::AimAweAlloc
        ) && self
            .modal
            .as_ref()
            .is_some_and(|modal| matches!(modal, Modal::NativeAweCreateSlow))
        {
            self.modal = None;
        }
        if self.refresh_in_flight {
            self.discard_refresh_result = true;
        }
        let created_identity = match result.as_ref() {
            Ok(WorkerActionResult::Created(identity)) => Some(identity.clone()),
            _ => None,
        };
        let confirmed_created = match (created_identity.as_ref(), snapshot.as_ref()) {
            (Some(identity), Ok(snapshot)) => Some(
                snapshot
                    .devices
                    .iter()
                    .any(|device| created_identity_matches(identity, &device.identity)),
            ),
            _ => None,
        };
        let unmount_reconciliation = match (&operation, &result, snapshot.as_ref()) {
            (
                WorkerOperation::Unmount(identity, _),
                Err(WorkerActionError::Unmount(error)),
                Ok(snapshot),
            ) if error.outcome_unknown => Some(if !snapshot.inventory_complete() {
                UnmountReconciliation::Unknown
            } else {
                match snapshot
                    .devices
                    .iter()
                    .find(|device| device.identity.occupies_same_adapter_slot(identity))
                {
                    None => UnmountReconciliation::ConfirmedGone,
                    Some(device) if &device.identity == identity => {
                        UnmountReconciliation::StillPresent
                    }
                    Some(_) if !snapshot.mount_scan_complete => UnmountReconciliation::Unknown,
                    Some(_) => UnmountReconciliation::SlotChanged,
                }
            }),
            _ => None,
        };
        let automatic_startup_create = matches!(
            &operation,
            WorkerOperation::Create(profile)
                if self.startup_active_profile_id.as_deref() == Some(profile.id.as_str())
        );
        self.apply_snapshot_result(snapshot);
        let create_form = self.pending.take().and_then(|state| state.create_form);
        self.refresh_memory();
        match (operation, result) {
            (WorkerOperation::Create(profile), Ok(WorkerActionResult::Created(identity))) => {
                let confirmed = confirmed_created == Some(true);
                if confirmed {
                    let memory_mode =
                        (profile.backend == DiskBackend::RamDyn).then_some(profile.memory_mode);
                    let volume_guid = single_volume_guid_for_identity(&identity);
                    let metadata_store_error = match volume_guid {
                        Some(volume_guid) => self
                            .active_disk_modes
                            .remember_managed_disk(
                                volume_guid,
                                identity.size_bytes,
                                memory_mode,
                                profile.sync_directory.as_deref(),
                            )
                            .err(),
                        None if memory_mode.is_some() => {
                            Some(localized_text!("msg_0480").to_owned())
                        }
                        None if profile.sync_directory.is_some() => {
                            Some(localized_text!("msg_0641").to_owned())
                        }
                        None => None,
                    };
                    if let (Some(volume_guid), Some(sync_directory)) =
                        (volume_guid, profile.sync_directory.as_deref())
                    {
                        self.unrecorded_sync_directories.insert(
                            volume_guid.to_ascii_lowercase(),
                            (identity.size_bytes, sync_directory.to_owned()),
                        );
                    }
                    if profile.backend == DiskBackend::RamDyn {
                        let mode_identity = self
                            .snapshot
                            .as_ref()
                            .and_then(|snapshot| {
                                snapshot.devices.iter().find(|device| {
                                    created_identity_matches(&identity, &device.identity)
                                })
                            })
                            .map(|device| device.identity.clone())
                            .unwrap_or_else(|| identity.clone());
                        self.created_modes
                            .insert(mode_identity, profile.memory_mode);
                    }
                    let guard_error =
                        if metadata_store_error.is_some() && profile.sync_directory.is_some() {
                            Some(localized_text!("msg_0642").to_owned())
                        } else {
                            self.clear_pending_create_guard(&profile.mount_target_key())
                                .err()
                        };
                    let profile_to_save = create_form
                        .as_ref()
                        .filter(|form| form.save_after_create)
                        .map(|form| form.profile.clone());
                    let saved = profile_to_save
                        .as_ref()
                        .map(|profile| self.profiles.upsert(profile.clone()));
                    match (&profile_to_save, &saved) {
                        (Some(profile), Some(Ok(()))) => {
                            self.unsaved_profiles.retain(|entry| entry.id != profile.id);
                        }
                        (Some(profile), Some(Err(_))) => {
                            self.unsaved_profiles.retain(|entry| entry.id != profile.id);
                            self.unsaved_profiles.push(profile.clone());
                        }
                        _ => {}
                    }
                    let (mut text, mut is_error) = match saved {
                        Some(Err(error)) => (
                            localized_format!(
                                "msg_0220",
                                profile.mount_target_display(),
                                error = error
                            ),
                            true,
                        ),
                        Some(Ok(())) => (
                            localized_format!("msg_0224", profile.mount_target_display()),
                            false,
                        ),
                        None => (
                            localized_format!("msg_0496", profile.mount_target_display()),
                            false,
                        ),
                    };
                    if let Some(error) = metadata_store_error {
                        text.push_str(&localized_format!("msg_0364", error = error));
                        is_error = true;
                    }
                    if let Some(error) = guard_error {
                        text.push_str(&localized_format!("msg_0159", error = error));
                        is_error = true;
                    }
                    self.notice = Some(Notice { text, is_error });
                } else {
                    self.unresolved_create_targets
                        .insert(profile.mount_target_key());
                    let text = if confirmed_created.is_none() {
                        localized_format!("msg_0155", profile.mount_target_display())
                    } else {
                        localized_format!("msg_0230", profile.mount_target_display())
                    };
                    let mut form = create_form
                        .clone()
                        .unwrap_or_else(|| ProfileForm::for_create(profile.clone(), false));
                    self.notice = Some(Notice {
                        text: text.clone(),
                        is_error: true,
                    });
                    if !automatic_startup_create {
                        form.error = Some(text);
                        self.modal = Some(Modal::ProfileEditor(form));
                    }
                }
            }
            (
                WorkerOperation::Unmount(identity, force),
                Ok(WorkerActionResult::Unmounted {
                    mount_point,
                    persistence_warning,
                }),
            ) => {
                self.close_unmount_confirmation(&identity);
                let mode_store_error = self.remove_active_mode_record(&identity);
                if let Some(volume_guid) = single_volume_guid_for_identity(&identity) {
                    self.unrecorded_sync_directories
                        .remove(&volume_guid.to_ascii_lowercase());
                }
                self.created_modes.remove(&identity);
                let mut text = if identity.volume_guids.len() > 1 {
                    if force {
                        localized_format!("msg_0383", identity.label(), mount_point = mount_point)
                    } else {
                        localized_format!("msg_0151", identity.label(), mount_point = mount_point)
                    }
                } else if force {
                    localized_format!("msg_0282", identity.label(), mount_point = mount_point)
                } else {
                    localized_format!("msg_0256", identity.label(), mount_point = mount_point)
                };
                let mut is_error = force && mount_point.contains(localized_text!("msg_0264"));
                if let Some(error) = mode_store_error {
                    text.push_str(&localized_format!("msg_0195", error = error));
                    is_error = true;
                }
                if let Some(error) = persistence_warning {
                    text.push_str(&localized_format!("msg_0640", persistence_error = error));
                    is_error = true;
                }
                self.notice = Some(Notice { text, is_error });
            }
            (WorkerOperation::Create(profile), Err(WorkerActionError::Create(error))) => {
                let mut message = error.message.clone();
                if error.outcome_unknown {
                    self.unresolved_create_targets
                        .insert(profile.mount_target_key());
                } else if let Err(guard_error) =
                    self.clear_pending_create_guard(&profile.mount_target_key())
                {
                    message.push_str(&localized_format!("msg_0355", guard_error = guard_error));
                }
                self.notice = Some(Notice {
                    text: message.clone(),
                    is_error: true,
                });
                if !automatic_startup_create {
                    let mut form = create_form
                        .clone()
                        .unwrap_or_else(|| ProfileForm::for_create(profile, false));
                    form.error = Some(message);
                    self.modal = Some(Modal::ProfileEditor(form));
                }
            }
            (WorkerOperation::Unmount(identity, force), Err(WorkerActionError::Unmount(error))) => {
                if error.outcome_unknown {
                    match unmount_reconciliation.unwrap_or(UnmountReconciliation::Unknown) {
                        UnmountReconciliation::ConfirmedGone => {
                            self.close_unmount_confirmation(&identity);
                            let mode_store_error = self.remove_active_mode_record(&identity);
                            if let Some(volume_guid) = single_volume_guid_for_identity(&identity) {
                                self.unrecorded_sync_directories
                                    .remove(&volume_guid.to_ascii_lowercase());
                            }
                            self.created_modes.remove(&identity);
                            let mut text = localized_format!("msg_0367", identity.label());
                            let mut is_error = false;
                            if let Some(error) = mode_store_error {
                                text.push_str(&localized_format!("msg_0195", error = error));
                                is_error = true;
                            }
                            self.notice = Some(Notice { text, is_error });
                        }
                        UnmountReconciliation::StillPresent => {
                            let retry_guidance = if self
                                .snapshot
                                .as_ref()
                                .is_some_and(|snapshot| snapshot.mount_scan_complete)
                            {
                                if force {
                                    localized_text!("msg_0236")
                                } else {
                                    localized_text!("msg_0107")
                                }
                            } else {
                                localized_text!("msg_0194")
                            };
                            self.notice = Some(Notice {
                                text: localized_format!(
                                    "msg_0363",
                                    identity.label(),
                                    error.message,
                                    retry_guidance = retry_guidance
                                ),
                                is_error: true,
                            });
                        }
                        UnmountReconciliation::SlotChanged => {
                            self.close_unmount_confirmation(&identity);
                            self.created_modes.remove(&identity);
                            self.notice = Some(Notice {
                                text: localized_format!("msg_0088", identity.label()),
                                is_error: true,
                            });
                        }
                        UnmountReconciliation::Unknown => {
                            self.close_unmount_confirmation(&identity);
                            self.unresolved_unmounts.insert(identity);
                            self.notice = Some(Notice {
                                text: error.message,
                                is_error: true,
                            });
                        }
                    }
                } else {
                    self.notice = Some(Notice {
                        text: error.message,
                        is_error: true,
                    });
                }
            }
            _ => {
                self.notice = Some(Notice {
                    text: localized_text!("msg_0406").to_owned(),
                    is_error: true,
                });
            }
        }
        if automatic_startup_create {
            if let Some(notice) = self.notice.take() {
                self.startup_result_is_error |= notice.is_error;
                self.startup_results.push(notice.text);
            }
            self.startup_active_profile_id = None;
            self.advance_startup_mount_queue();
        }
    }

    fn show_top_bar(&mut self, context: &egui::Context) {
        egui::TopBottomPanel::top("manager_top").show(context, |ui| {
            ui.horizontal(|ui| {
                if self.refresh_in_flight {
                    if self.snapshot.is_some() {
                        ui.label(localized_text!("msg_0251"));
                    } else {
                        ui.label(localized_text!("msg_0183"));
                    }
                } else if let Some(error) = &self.snapshot_error {
                    if self.snapshot.is_some() {
                        ui.colored_label(
                            egui::Color32::from_rgb(190, 75, 65),
                            localized_text!("msg_0529"),
                        )
                        .on_hover_text(error.as_str());
                    } else {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(error.as_str())
                                    .color(egui::Color32::from_rgb(190, 75, 65)),
                            )
                            .truncate(),
                        )
                        .on_hover_text(error.as_str());
                    }
                } else if let Some(snapshot) = &self.snapshot {
                    let ports = snapshot
                        .adapter_ports
                        .iter()
                        .map(u8::to_string)
                        .collect::<Vec<_>>()
                        .join(", ");
                    let adapter_count = snapshot
                        .adapter_instance_count
                        .unwrap_or(snapshot.adapter_ports.len());
                    if snapshot.adapter_scan_complete {
                        ui.label(localized_format!(
                            "msg_0586",
                            count = adapter_count,
                            ports = ports
                        ));
                    } else {
                        let status = ui.colored_label(
                            egui::Color32::from_rgb(190, 100, 35),
                            localized_text!("msg_0587"),
                        );
                        if let Some(error) = snapshot.adapter_scan_error.as_deref() {
                            status.on_hover_text(error);
                        }
                    }
                    if !snapshot.mount_scan_complete {
                        let status = ui.colored_label(
                            egui::Color32::from_rgb(190, 100, 35),
                            localized_text!("msg_0306"),
                        );
                        if let Some(error) = snapshot.mount_scan_error.as_deref() {
                            status.on_hover_text(error);
                        }
                    }
                } else {
                    ui.label(localized_text!("msg_0183"));
                }

                if let Some(error) = self.pending_creates.load_error() {
                    ui.colored_label(
                        egui::Color32::from_rgb(190, 75, 65),
                        localized_text!("msg_0429"),
                    )
                    .on_hover_text(error);
                } else if !self.unresolved_create_targets.is_empty() {
                    let mut targets = self
                        .unresolved_create_targets
                        .iter()
                        .map(MountTargetKey::label)
                        .collect::<Vec<_>>();
                    targets.sort();
                    ui.colored_label(
                        egui::Color32::from_rgb(190, 100, 35),
                        localized_format!("msg_0431", targets.join(localized_text!("msg_0562"))),
                    );
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let can_create = self.pending.is_none()
                        && self.modal.is_none()
                        && !self.close_prompt
                        && !self.refresh_in_flight
                        && self.aim_state_unavailable_reason().is_none()
                        && self.pending_creates.load_error().is_none()
                        && self.runtime_error.is_none();
                    let new_disk_hint = if self.refresh_in_flight {
                        localized_text!("msg_0247")
                    } else if let Some(error) = self.aim_state_unavailable_reason() {
                        error
                    } else {
                        self.pending_creates
                            .load_error()
                            .or(self.runtime_error.as_deref())
                            .unwrap_or("Ctrl+N")
                    };
                    let new_disk = ui
                        .add_enabled(can_create, egui::Button::new(localized_text!("msg_0057")))
                        .on_hover_text(new_disk_hint);
                    if new_disk.clicked() {
                        self.open_new_disk();
                    }
                    let refresh = ui
                        .add_enabled(
                            (self.pending.is_none()
                                || self.pending.as_ref().is_some_and(|operation| {
                                    operation.slow_awe_warning_shown
                                }))
                                && !self.refresh_in_flight,
                            egui::Button::new(if self.refresh_in_flight {
                                localized_text!("msg_0289")
                            } else {
                                localized_text!("msg_0394")
                            }),
                        )
                        .on_hover_text("F5");
                    if refresh.clicked() {
                        self.request_refresh();
                    }
                    if ui
                        .add_enabled(
                            self.modal.is_none() && !self.close_prompt,
                            egui::Button::new(localized_text!("msg_0456")),
                        )
                        .clicked()
                    {
                        self.modal = Some(Modal::Help);
                    }
                });
            });

            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(localized_text!("msg_0275"));
                if let Some(memory) = self.memory {
                    let total = bytes_gib(memory.total_physical_bytes);
                    let available = bytes_gib(memory.available_physical_bytes);
                    let used = bytes_gib(memory.used_physical_bytes());
                    ui.label(localized_format!(
                        "msg_0237",
                        available = available,
                        total = total
                    ));
                    ui.weak(localized_format!("msg_0524", used = used));
                    ui.add(
                        egui::ProgressBar::new(memory.used_fraction().clamp(0.0, 1.0))
                            .desired_width(104.0)
                            .show_percentage(),
                    )
                    .on_hover_text(localized_text!("msg_0051"));
                    ui.weak(localized_format!(
                        "msg_0180",
                        bytes_gib(memory.available_commit_bytes),
                        bytes_gib(memory.total_commit_limit_bytes)
                    ))
                    .on_hover_text(localized_text!("msg_0027"));
                } else {
                    let label = self
                        .memory_error
                        .as_deref()
                        .unwrap_or(localized_text!("msg_0302"));
                    ui.label("—").on_hover_text(label);
                }

                if let Some(operation) = self.pending.as_ref() {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        low_frequency_spinner(ui);
                        ui.label(operation_label(operation));
                    });
                }
            });

            if let Some(error) = &self.runtime_error {
                ui.horizontal_wrapped(|ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(localized_format!("msg_0284", error = error))
                                .color(egui::Color32::from_rgb(190, 75, 65)),
                        )
                        .wrap(),
                    );
                    ui.weak(localized_text!("msg_0366"));
                });
            }
        });
    }

    fn display_backing_type(&self, device: &crate::aim::AimDevice) -> String {
        match self.created_modes.get(&device.identity) {
            Some(mode) => localized_format!("msg_0407", mode.label()),
            None if device.backing_type_label() == localized_text!("msg_0039") => {
                localized_text!("msg_0190").to_owned()
            }
            None => device.backing_type_label().to_owned(),
        }
    }

    fn show_disks(&mut self, ui: &mut egui::Ui) {
        if let Some(error) = &self.snapshot_error {
            ui.horizontal_wrapped(|ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(error.as_str())
                            .color(egui::Color32::from_rgb(190, 75, 65)),
                    )
                    .truncate(),
                )
                .on_hover_text(error.as_str());
                if ui.small_button(localized_text!("msg_0129")).clicked() {
                    ui.ctx().copy_text(error.clone());
                }
            });
            if self.snapshot.is_some() {
                ui.weak(localized_text!("msg_0507"));
            }
        }

        let Some(snapshot) = self.snapshot.clone() else {
            ui.add_space(24.0);
            let message = if self.refresh_in_flight {
                localized_text!("msg_0454")
            } else {
                localized_text!("msg_0271")
            };
            ui.centered_and_justified(|ui| {
                ui.label(message);
            });
            return;
        };

        if !snapshot.inventory_complete() {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(
                    egui::Color32::from_rgb(190, 100, 35),
                    localized_text!("msg_0052"),
                );
                if let Some(error) = snapshot
                    .adapter_scan_error
                    .as_deref()
                    .or(snapshot.mount_scan_error.as_deref())
                {
                    ui.add(egui::Label::new(error).truncate())
                        .on_hover_text(error);
                    if ui.small_button(localized_text!("msg_0129")).clicked() {
                        ui.ctx().copy_text(error.to_owned());
                    }
                }
            });
        }

        if snapshot.devices.is_empty() {
            ui.add_space(18.0);
            if !snapshot.inventory_complete() {
                ui.colored_label(
                    egui::Color32::from_rgb(190, 100, 35),
                    localized_text!("msg_0002"),
                );
            } else {
                ui.label(localized_text!("msg_0413"));
            }
            return;
        }

        let mut choose_device = None;
        let mut unmount_device = None;
        let mount_column_width = (ui.available_width() - 450.0).clamp(160.0, 360.0);
        let type_column_width =
            (ui.available_width() - mount_column_width - 350.0).clamp(100.0, 240.0);
        let capacity_column_width = 78.0;
        let state_column_width = 82.0;
        let operation_column_width = 132.0;
        let row_height = 36.0;
        let grid_spacing = [8.0, 4.0];

        egui::Grid::new("current_disks_header")
            .spacing(grid_spacing)
            .min_col_width(0.0)
            .max_col_width(360.0)
            .show(ui, |ui| {
                ui.add_sized(
                    [mount_column_width, row_height],
                    egui::Label::new(egui::RichText::new(localized_text!("msg_0483")).strong())
                        .truncate(),
                );
                ui.add_sized(
                    [capacity_column_width, row_height],
                    egui::Label::new(egui::RichText::new(localized_text!("msg_0482")).strong()),
                );
                ui.add_sized(
                    [type_column_width, row_height],
                    egui::Label::new(egui::RichText::new(localized_text!("msg_0007")).strong())
                        .truncate(),
                );
                ui.add_sized(
                    [state_column_width, row_height],
                    egui::Label::new(egui::RichText::new(localized_text!("msg_0228")).strong()),
                );
                ui.add_sized(
                    [operation_column_width, row_height],
                    egui::Label::new(egui::RichText::new(localized_text!("msg_0523")).strong()),
                );
                ui.end_row();
            });

        egui::ScrollArea::vertical().show(ui, |ui| {
            egui::Grid::new("current_disks_grid")
                .striped(true)
                .spacing(grid_spacing)
                .min_col_width(0.0)
                .max_col_width(360.0)
                .show(ui, |ui| {
                    for device in &snapshot.devices {
                        let mount_id =
                            ui.make_persistent_id(("current_disk_mount", &device.identity));
                        let open_id =
                            ui.make_persistent_id(("current_disk_open", &device.identity));
                        let unmount_id =
                            ui.make_persistent_id(("current_disk_unmount", &device.identity));
                        let mount = device.display_mount_point();
                        let selected = self.selected_device.as_ref() == Some(&device.identity);
                        let mount_response = stable_table_button(
                            ui,
                            mount_id,
                            &mount,
                            egui::vec2(mount_column_width, row_height),
                            selected,
                            false,
                        )
                        .on_hover_text(&mount);
                        if mount_response.clicked() {
                            choose_device = Some(device.identity.clone());
                        }
                        ui.add_sized(
                            [capacity_column_width, row_height],
                            egui::Label::new(format_size(device.identity.size_bytes)).truncate(),
                        );
                        let device_type = format!(
                            "{} · {}",
                            device.device_type_label(),
                            self.display_backing_type(device)
                        );
                        ui.add_sized(
                            [type_column_width, row_height],
                            egui::Label::new(device_type.clone()).truncate(),
                        )
                        .on_hover_text(device_type);
                        let unmount_unresolved =
                            self.unresolved_unmounts.contains(&device.identity);
                        let state = if unmount_unresolved {
                            localized_text!("msg_0320")
                        } else if !device.mount_scan_complete {
                            localized_text!("msg_0357")
                        } else if device.mount_points.is_empty() {
                            localized_text!("msg_0368")
                        } else {
                            localized_text!("msg_0191")
                        };
                        let state_label = ui.add_sized(
                            [state_column_width, row_height],
                            egui::Label::new(state).truncate(),
                        );
                        if !device.mount_scan_complete {
                            state_label.on_hover_text(localized_text!("msg_0006"));
                        }
                        ui.allocate_ui_with_layout(
                            egui::vec2(operation_column_width, row_height),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                let button_height = ui.spacing().interact_size.y;
                                let button_frame = ui.visuals().button_frame;
                                if let Some(mount_point) = device.preferred_mount_point() {
                                    let open_enabled = self.pending.is_none()
                                        && !self.refresh_in_flight
                                        && self.snapshot_error.is_none();
                                    let open_response = ui
                                        .add_enabled_ui(open_enabled, |ui| {
                                            stable_table_button(
                                                ui,
                                                open_id,
                                                localized_text!("msg_0445"),
                                                egui::vec2(48.0, button_height),
                                                false,
                                                button_frame,
                                            )
                                        })
                                        .inner;
                                    if open_response.clicked() {
                                        if let Err(error) = open_mount_point(&mount_point.path) {
                                            self.notice = Some(Notice {
                                                text: error,
                                                is_error: true,
                                            });
                                        }
                                    }
                                }
                                let can_unmount =
                                    device.can_unmount() && self.snapshot_error.is_none();
                                let unmount_enabled = self.pending.is_none()
                                    && !self.refresh_in_flight
                                    && can_unmount
                                    && !unmount_unresolved;
                                let button = ui
                                    .add_enabled_ui(unmount_enabled, |ui| {
                                        stable_table_button(
                                            ui,
                                            unmount_id,
                                            localized_text!("msg_0016"),
                                            egui::vec2(48.0, button_height),
                                            false,
                                            button_frame,
                                        )
                                    })
                                    .inner;
                                if button.clicked() {
                                    unmount_device = Some(device.identity.clone());
                                }
                                if self.refresh_in_flight {
                                    button.on_hover_text(localized_text!("msg_0447"));
                                } else if let Some(error) = &self.snapshot_error {
                                    button.on_hover_text(localized_format!(
                                        "msg_0075",
                                        error = error
                                    ));
                                } else if unmount_unresolved {
                                    button.on_hover_text(localized_text!("msg_0333"));
                                } else if let Some(reason) = device.unmount_block_reason() {
                                    button.on_hover_text(reason);
                                }
                            },
                        );
                        ui.end_row();
                    }
                });
        });

        if let Some(identity) = choose_device {
            self.selected_device = Some(identity);
        }
        if let Some(identity) = unmount_device {
            self.modal = Some(Modal::ConfirmUnmount {
                identity,
                focus_cancel: true,
            });
        }

        if let Some(selected) = self.selected_device.as_ref() {
            if let Some(device) = snapshot
                .devices
                .iter()
                .find(|device| &device.identity == selected)
            {
                ui.separator();
                let backing_type = self.display_backing_type(device);
                ui.collapsing(localized_text!("msg_0293"), |ui| {
                    ui.label(device.identity.label());
                    ui.label(localized_format!(
                        "msg_0263",
                        format_size(device.identity.size_bytes)
                    ));
                    ui.label(format!("{} · {}", device.device_type_label(), backing_type));
                    if let Some(number) = device.identity.physical_drive_number {
                        ui.label(format!("PhysicalDrive{number}"));
                    }
                    ui.label(localized_format!("msg_0036", device.raw_flags));
                    if let Some(volume_guid) = &device.identity.volume_guid {
                        ui.label(localized_format!("msg_0321", volume_guid = volume_guid));
                    }
                    if device.volume_guids.len() > 1 {
                        ui.label(localized_format!("msg_0154", device.volume_guids.len()));
                        for volume in &device.volume_associations {
                            let mount_points = device
                                .mount_points
                                .iter()
                                .filter(|mount| {
                                    mount.volume_guid.as_deref().is_some_and(|guid| {
                                        guid.eq_ignore_ascii_case(&volume.volume_guid)
                                    })
                                })
                                .map(crate::aim::MountPoint::display)
                                .collect::<Vec<_>>();
                            ui.label(format!(
                                "{} · {}",
                                volume.volume_guid,
                                if mount_points.is_empty() {
                                    localized_text!("msg_0368").to_owned()
                                } else {
                                    mount_points.join(localized_text!("msg_0562"))
                                }
                            ));
                        }
                    }
                    if device.mount_points.is_empty() {
                        if device.mount_scan_complete {
                            ui.label(localized_text!("msg_0127"));
                        } else {
                            ui.label(localized_text!("msg_0419"));
                        }
                    } else {
                        ui.label(localized_format!(
                            "msg_0434",
                            device
                                .mount_points
                                .iter()
                                .map(crate::aim::MountPoint::display)
                                .collect::<Vec<_>>()
                                .join(localized_text!("msg_0562"))
                        ));
                    }
                    ui.label(localized_text!("msg_0339"));
                    let mut details = vec![
                        localized_format!("msg_0021", device.identity.label()),
                        localized_format!("msg_0263", format_size(device.identity.size_bytes)),
                        localized_format!("msg_0112", device.device_type_label()),
                        localized_format!("msg_0481", backing_type),
                        localized_format!("msg_0036", device.raw_flags),
                    ];
                    if let Some(number) = device.identity.physical_drive_number {
                        details.push(format!("PhysicalDrive：{number}"));
                    }
                    if device.volume_guids.len() > 1 {
                        details.push(localized_format!("msg_0472", device.volume_guids.len()));
                        details.extend(
                            device
                                .volume_guids
                                .iter()
                                .map(|guid| localized_format!("msg_0063", guid = guid)),
                        );
                    } else if let Some(volume_guid) = &device.identity.volume_guid {
                        details.push(localized_format!("msg_0321", volume_guid = volume_guid));
                    }
                    for mount in &device.mount_points {
                        details.push(localized_format!("msg_0434", mount.path));
                        if let Some(label) = &mount.volume_label {
                            details.push(localized_format!("msg_0134", label = label));
                        }
                        if let Some(volume_guid) = &mount.volume_guid {
                            details.push(localized_format!("msg_0321", volume_guid = volume_guid));
                        }
                    }
                    if device.mount_points.is_empty() {
                        details.push(if device.mount_scan_complete {
                            localized_text!("msg_0127").to_owned()
                        } else {
                            localized_text!("msg_0419").to_owned()
                        });
                    }
                    if ui.small_button(localized_text!("msg_0129")).clicked() {
                        ui.ctx().copy_text(details.join("\n"));
                    }
                });
            }
        }
    }

    fn show_profiles(&mut self, ui: &mut egui::Ui) {
        let auto_mount_count = self
            .profiles
            .profiles()
            .iter()
            .filter(|profile| profile.auto_mount)
            .count();
        ui.horizontal_wrapped(|ui| {
            let mut enabled = self.login_startup_enabled.unwrap_or(false);
            let startup_setting = ui.add_enabled(
                self.login_startup_enabled.is_some() && !self.login_startup_change_pending,
                egui::Checkbox::new(&mut enabled, localized_text!("msg_0514")),
            );
            if startup_setting.changed() {
                self.set_login_startup(enabled);
            }
            if self.login_startup_change_pending {
                low_frequency_spinner(ui);
                ui.weak(localized_text!("msg_0268"));
            } else if ui.small_button(localized_text!("msg_0432")).clicked() {
                self.request_login_startup_state();
            }
            ui.weak(match self.login_startup_enabled {
                Some(true) if auto_mount_count > 0 => {
                    localized_format!("msg_0162", auto_mount_count)
                }
                Some(true) => localized_text!("msg_0492").to_owned(),
                Some(false) if auto_mount_count > 0 => {
                    localized_format!("msg_0298", auto_mount_count = auto_mount_count)
                }
                Some(false) => localized_text!("msg_0451").to_owned(),
                None => localized_text!("msg_0494").to_owned(),
            });
        });
        ui.weak(localized_text!("msg_0446"));
        if let Some(error) = &self.login_startup_error {
            ui.colored_label(egui::Color32::from_rgb(190, 75, 65), error);
        }
        let settings_error = self.manager_settings.load_error().map(str::to_owned);
        ui.horizontal_wrapped(|ui| {
            let mut close_after_startup = self.manager_settings.close_after_startup();
            let response = ui.add_enabled(
                settings_error.is_none(),
                egui::Checkbox::new(&mut close_after_startup, localized_text!("msg_0452")),
            );
            if response.changed() {
                self.set_close_after_startup(close_after_startup);
            }
        });
        ui.weak(localized_text!("msg_0208"));
        if let Some(error) = settings_error {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(egui::Color32::from_rgb(190, 75, 65), &error);
                if ui.small_button(localized_text!("msg_0129")).clicked() {
                    ui.ctx().copy_text(error);
                }
            });
        }
        let last_startup_run = self
            .manager_settings
            .last_startup_run()
            .map(|run| (run.results.clone(), run.has_errors, run.language));
        if let Some((results, has_errors, results_language)) = last_startup_run {
            let results = results
                .iter()
                .map(|result| {
                    i18n::translate_rendered(result, results_language, i18n::current_language())
                        .unwrap_or_else(|| result.to_owned())
                })
                .collect::<Vec<_>>();
            egui::CollapsingHeader::new(localized_text!("msg_0217"))
                .default_open(false)
                .show(ui, |ui| {
                    if has_errors {
                        ui.colored_label(
                            egui::Color32::from_rgb(190, 75, 65),
                            localized_text!("msg_0425"),
                        );
                    }
                    for result in &results {
                        ui.label(result.as_str());
                    }
                    ui.horizontal(|ui| {
                        if ui.small_button(localized_text!("msg_0175")).clicked() {
                            ui.ctx().copy_text(results.join("\n"));
                        }
                    });
                });
        } else {
            ui.weak(localized_text!("msg_0117"));
        }
        ui.add_space(8.0);

        if let Some(error) = self.profiles.load_error() {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(egui::Color32::from_rgb(190, 75, 65), error);
                if ui.small_button(localized_text!("msg_0129")).clicked() {
                    ui.ctx().copy_text(error.to_owned());
                }
            });
            ui.weak(localized_text!("msg_0474"));
            if !self.profiles.path().as_os_str().is_empty() {
                ui.label(self.profiles.path().display().to_string());
            }
            return;
        }

        ui.horizontal(|ui| {
            ui.label(localized_format!(
                "msg_0509",
                self.profiles.profiles().len()
            ));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(
                        self.pending.is_none(),
                        egui::Button::new(localized_text!("msg_0272")),
                    )
                    .clicked()
                {
                    self.refresh_memory();
                    self.modal = Some(Modal::ProfileEditor(ProfileForm::new(
                        EditorMode::SaveConfig,
                    )));
                }
            });
        });
        ui.weak(localized_text!("msg_0518"));
        ui.add_space(8.0);

        let profiles = self.profiles.profiles().to_vec();
        if profiles.is_empty() {
            ui.label(localized_text!("msg_0380"));
            return;
        }

        let mut mount = None;
        let mut edit = None;
        let mut delete = None;
        let aim_error = self.aim_state_unavailable_reason().map(str::to_owned);
        let aim_available = aim_error.is_none();
        egui::ScrollArea::vertical().show(ui, |ui| {
            let available_width = ui.available_width();
            let column_spacing = if available_width < 760.0 { 4.0 } else { 8.0 };
            let grid_spacing = [column_spacing, 4.0];
            let row_height = 40.0;
            let drive_column_width = (available_width * 0.13).clamp(82.0, 138.0);
            let capacity_column_width = 78.0;
            let mode_column_width = 132.0;
            let status_column_width = (available_width * 0.15).clamp(92.0, 144.0);
            let operation_column_width = 48.0 + 48.0 + 80.0 + 2.0 * column_spacing;
            let profile_column_width = (available_width
                - 5.0 * column_spacing
                - drive_column_width
                - capacity_column_width
                - mode_column_width
                - status_column_width
                - operation_column_width)
                .max(100.0);
            egui::Grid::new("saved_profiles_grid")
                .striped(true)
                .spacing(grid_spacing)
                .min_col_width(0.0)
                .show(ui, |ui| {
                    ui.add_sized(
                        [profile_column_width, row_height],
                        egui::Label::new(egui::RichText::new(localized_text!("msg_0122")).strong())
                            .truncate(),
                    );
                    ui.add_sized(
                        [drive_column_width, row_height],
                        egui::Label::new(egui::RichText::new(localized_text!("msg_0095")).strong()),
                    );
                    ui.add_sized(
                        [capacity_column_width, row_height],
                        egui::Label::new(egui::RichText::new(localized_text!("msg_0482")).strong()),
                    );
                    ui.add_sized(
                        [mode_column_width, row_height],
                        egui::Label::new(egui::RichText::new(localized_text!("msg_0506")).strong())
                            .truncate(),
                    );
                    ui.add_sized(
                        [status_column_width, row_height],
                        egui::Label::new(egui::RichText::new(localized_text!("msg_0228")).strong()),
                    );
                    ui.add_sized(
                        [operation_column_width, row_height],
                        egui::Label::new(egui::RichText::new(localized_text!("msg_0523")).strong()),
                    );
                    ui.end_row();
                    for profile in &profiles {
                        let mount_id =
                            ui.make_persistent_id(("saved_profile_mount", profile.id.as_str()));
                        let edit_id =
                            ui.make_persistent_id(("saved_profile_edit", profile.id.as_str()));
                        let delete_id =
                            ui.make_persistent_id(("saved_profile_delete", profile.id.as_str()));
                        ui.allocate_ui_with_layout(
                            egui::vec2(profile_column_width, row_height),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.vertical(|ui| {
                                    ui.add(egui::Label::new(&profile.name).truncate())
                                        .on_hover_text(&profile.name);
                                    let volume_label = localized_format!(
                                        "msg_0517",
                                        profile.volume_label,
                                        profile.file_system.label(),
                                        if profile.auto_mount {
                                            localized_text!("msg_0458")
                                        } else {
                                            localized_text!("msg_0074")
                                        }
                                    );
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(volume_label.clone()).weak(),
                                        )
                                        .truncate(),
                                    )
                                    .on_hover_text(volume_label);
                                });
                            },
                        );
                        ui.add_sized(
                            [drive_column_width, row_height],
                            egui::Label::new(profile.mount_target_display()).truncate(),
                        );
                        ui.add_sized(
                            [capacity_column_width, row_height],
                            egui::Label::new(format_size(
                                profile.size_mib.saturating_mul(1024 * 1024),
                            ))
                            .truncate(),
                        );
                        let mode_description = match profile.backend {
                            DiskBackend::RamDyn => localized_format!(
                                "msg_0076",
                                profile.backend.label(),
                                profile.memory_mode.label(),
                                1_u64 << profile.block_size_exponent.saturating_sub(20)
                            ),
                            DiskBackend::AimVirtualMemory => {
                                localized_format!("msg_0475", profile.backend.label())
                            }
                            DiskBackend::AimAweAlloc => {
                                localized_format!("msg_0475", profile.backend.label())
                            }
                            DiskBackend::AimImageFile => {
                                localized_format!("msg_0475", profile.backend.label())
                            }
                        };
                        ui.add_sized(
                            [mode_column_width, row_height],
                            egui::Label::new(mode_description.clone()).truncate(),
                        )
                        .on_hover_text(localized_format!(
                            "msg_0404",
                            mode_description,
                            profile.file_system.label(),
                            if profile.auto_mount {
                                localized_text!("msg_0350")
                            } else {
                                localized_text!("msg_0323")
                            }
                        ));
                        let target_key = profile.mount_target_key();
                        let unresolved = self.unresolved_create_targets.contains(&target_key);
                        let target_availability = system::mount_target_is_available(profile);
                        let target_available = target_availability
                            .as_ref()
                            .is_ok_and(|available| *available);
                        let create_guard_available = self.pending_creates.load_error().is_none();
                        let runtime_error = self.runtime_error_for_profile(profile);
                        let runtime_available = runtime_error.is_none();
                        let mode_unavailable_reason = if profile.backend == DiskBackend::RamDyn {
                            self.memory_mode_unavailable_reason(profile.memory_mode)
                        } else {
                            None
                        };
                        let mode_available = mode_unavailable_reason.is_none();
                        let creating = self
                            .pending
                            .as_ref()
                            .and_then(|operation| operation.create_form.as_ref())
                            .is_some_and(|form| form.profile.mount_target_key() == target_key);
                        let status = if !create_guard_available {
                            localized_text!("msg_0334")
                        } else if creating {
                            localized_text!("msg_0244")
                        } else if unresolved {
                            localized_text!("msg_0521")
                        } else if !runtime_available {
                            localized_text!("msg_0085")
                        } else if self.refresh_in_flight {
                            localized_text!("msg_0202")
                        } else if !aim_available {
                            localized_text!("msg_0335")
                        } else if !mode_available {
                            localized_text!("msg_0505")
                        } else {
                            match &target_availability {
                                Ok(true) => localized_text!("msg_0192"),
                                Ok(false) => localized_text!("msg_0171"),
                                Err(_) => localized_text!("msg_0327"),
                            }
                        };
                        let status_response = ui.add_sized(
                            [status_column_width, row_height],
                            egui::Label::new(status).truncate(),
                        );
                        if self.refresh_in_flight {
                            status_response.on_hover_text(localized_text!("msg_0179"));
                        } else if let Some(error) = self.pending_creates.load_error() {
                            status_response.on_hover_text(error);
                        } else if let Some(error) = runtime_error.as_deref() {
                            status_response.on_hover_text(error);
                        } else if let Some(error) = aim_error.as_deref() {
                            status_response.on_hover_text(error);
                        } else if let Some(error) = mode_unavailable_reason.as_deref() {
                            status_response.on_hover_text(error);
                        } else if let Err(error) = &target_availability {
                            status_response.on_hover_text(error);
                        }
                        ui.allocate_ui_with_layout(
                            egui::vec2(operation_column_width, row_height),
                            egui::Layout::left_to_right(egui::Align::Center),
                            |ui| {
                                ui.spacing_mut().item_spacing.x = column_spacing;
                                let button_height = ui.spacing().interact_size.y;
                                let button_frame = ui.visuals().button_frame;
                                let mount_enabled = self.pending.is_none()
                                    && !self.refresh_in_flight
                                    && create_guard_available
                                    && runtime_available
                                    && mode_available
                                    && aim_available
                                    && !unresolved
                                    && target_available;
                                let mount_response = ui
                                    .add_enabled_ui(mount_enabled, |ui| {
                                        stable_table_button(
                                            ui,
                                            mount_id,
                                            localized_text!("msg_0261"),
                                            egui::vec2(48.0, button_height),
                                            false,
                                            button_frame,
                                        )
                                    })
                                    .inner;
                                if mount_response.clicked() {
                                    mount = Some(profile.clone());
                                }
                                let edit_response = ui
                                    .add_enabled_ui(self.pending.is_none(), |ui| {
                                        stable_table_button(
                                            ui,
                                            edit_id,
                                            localized_text!("msg_0011"),
                                            egui::vec2(48.0, button_height),
                                            false,
                                            button_frame,
                                        )
                                    })
                                    .inner;
                                if edit_response.clicked() {
                                    edit = Some(profile.clone());
                                }
                                let delete_response = ui
                                    .add_enabled_ui(self.pending.is_none(), |ui| {
                                        stable_table_button(
                                            ui,
                                            delete_id,
                                            localized_text!("msg_0178"),
                                            egui::vec2(80.0, button_height),
                                            false,
                                            button_frame,
                                        )
                                    })
                                    .inner;
                                if delete_response.clicked() {
                                    delete = Some(profile.id.clone());
                                }
                            },
                        );
                        ui.end_row();
                    }
                });
        });

        if let Some(profile) = mount {
            let form = ProfileForm::for_create(profile.clone(), false);
            self.start_create(profile, form);
        } else if let Some(profile) = edit {
            self.refresh_memory();
            self.modal = Some(Modal::ProfileEditor(ProfileForm::from_profile(profile)));
        } else if let Some(id) = delete {
            self.modal = Some(Modal::ConfirmDeleteProfile {
                id,
                focus_cancel: true,
            });
        }
    }

    fn show_notice(&mut self, ui: &mut egui::Ui) {
        let now = Instant::now();
        let mut notice = self
            .notice
            .as_ref()
            .map(|notice| (notice.text.clone(), notice.is_error));
        let notice_deadline = if let Some((text, is_error)) = notice.as_ref() {
            let reset_timeout = self.notice_timeout.as_ref().map_or(true, |timeout| {
                timeout.text.as_str() != text.as_str() || timeout.is_error != *is_error
            });
            if reset_timeout {
                let duration = if *is_error {
                    Duration::from_secs(12)
                } else {
                    Duration::from_secs(6)
                };
                self.notice_timeout = Some(NoticeTimeout {
                    text: text.clone(),
                    is_error: *is_error,
                    deadline: now + duration,
                });
            }
            self.notice_timeout.as_ref().map(|timeout| timeout.deadline)
        } else {
            self.notice_timeout = None;
            None
        };
        if let Some(deadline) = notice_deadline {
            if now >= deadline {
                self.notice = None;
                self.notice_timeout = None;
                notice = None;
            } else {
                ui.ctx()
                    .request_repaint_after(deadline.saturating_duration_since(now));
            }
        }
        let unsaved_profiles = self
            .unsaved_profiles
            .iter()
            .map(|profile| (profile.id.clone(), profile.name.clone()))
            .collect::<Vec<_>>();
        let pending_creates = self
            .pending_creates
            .pending_creates()
            .iter()
            .map(|entry| {
                (
                    entry.profile.mount_target_key(),
                    entry.profile.mount_target_display(),
                    entry.profile.name.clone(),
                )
            })
            .collect::<Vec<_>>();
        let journal_error = self.pending_creates.load_error().map(str::to_owned);
        let journal_path = self.pending_creates.path().to_path_buf();
        if notice.is_none()
            && unsaved_profiles.is_empty()
            && pending_creates.is_empty()
            && journal_error.is_none()
        {
            return;
        }
        let (close, retry_id) = ui
            .horizontal_wrapped(|ui| {
                let mut close = false;
                if let Some((text, is_error)) = &notice {
                    let color = if *is_error {
                        egui::Color32::from_rgb(190, 75, 65)
                    } else {
                        egui::Color32::from_rgb(35, 125, 75)
                    };
                    ui.colored_label(color, text);
                    close = ui.small_button(localized_text!("msg_0160")).clicked();
                    if *is_error && ui.small_button(localized_text!("msg_0129")).clicked() {
                        ui.ctx().copy_text(text.clone());
                    }
                } else if !unsaved_profiles.is_empty() {
                    ui.colored_label(
                        egui::Color32::from_rgb(190, 75, 65),
                        localized_text!("msg_0233"),
                    );
                }
                let mut retry_id = None;
                for (id, name) in &unsaved_profiles {
                    if ui
                        .small_button(localized_format!("msg_0110", name = name))
                        .clicked()
                    {
                        retry_id = Some(id.clone());
                    }
                }
                (close, retry_id)
            })
            .inner;

        if close {
            self.notice = None;
            self.notice_timeout = None;
        }
        if let Some(id) = retry_id {
            if let Some(profile) = self
                .unsaved_profiles
                .iter()
                .find(|profile| profile.id == id)
                .cloned()
            {
                match self.profiles.upsert(profile.clone()) {
                    Ok(()) => {
                        self.unsaved_profiles.retain(|entry| entry.id != profile.id);
                        self.notice = Some(Notice {
                            text: localized_format!("msg_0549", profile.name),
                            is_error: false,
                        });
                    }
                    Err(error) => {
                        self.notice = Some(Notice {
                            text: localized_format!("msg_0067", profile.name, error = error),
                            is_error: true,
                        });
                    }
                }
            }
        }

        let active_create_target = self
            .pending
            .as_ref()
            .and_then(|operation| operation.create_form.as_ref())
            .map(|form| form.profile.mount_target_key());
        let can_review_pending_create = self.pending.is_none()
            && !self.refresh_in_flight
            && self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.mount_scan_complete)
            && self.snapshot_error.is_none();
        let mut request_refresh = false;
        let mut clear_pending_target = None;
        for (target_key, target_display, name) in pending_creates {
            ui.horizontal_wrapped(|ui| {
                let message = if active_create_target.as_ref() == Some(&target_key) {
                    localized_format!("msg_0491", target_display = target_display, name = name)
                } else {
                    localized_format!("msg_0371", target_display = target_display, name = name)
                };
                ui.colored_label(egui::Color32::from_rgb(190, 100, 35), message);
                if ui
                    .add_enabled(
                        self.pending.is_none() && !self.refresh_in_flight,
                        egui::Button::new(localized_text!("msg_0131")),
                    )
                    .clicked()
                {
                    request_refresh = true;
                }
                let clear = ui
                    .add_enabled(
                        can_review_pending_create,
                        egui::Button::new(localized_text!("msg_0119")),
                    )
                    .on_hover_text(localized_text!("msg_0126"));
                if clear.clicked() {
                    clear_pending_target = Some(target_key.clone());
                }
            });
        }
        if let Some(error) = journal_error {
            ui.horizontal_wrapped(|ui| {
                let detail = if journal_path.as_os_str().is_empty() {
                    localized_format!("msg_0146", error = error)
                } else {
                    localized_format!("msg_0250", journal_path.display(), error = error)
                };
                ui.colored_label(egui::Color32::from_rgb(190, 75, 65), &detail);
                ui.weak(localized_text!("msg_0161"));
                if ui.small_button(localized_text!("msg_0129")).clicked() {
                    ui.ctx().copy_text(detail);
                }
            });
        }
        if request_refresh {
            self.request_refresh();
        }
        if let Some(mount_target) = clear_pending_target {
            self.request_refresh();
            self.modal = Some(Modal::ConfirmClearPendingCreate {
                mount_target,
                focus_cancel: true,
            });
        }
    }

    fn show_modal(&mut self, context: &egui::Context) {
        let Some(modal) = self.modal.take() else {
            return;
        };
        match modal {
            Modal::Help => self.show_help(context),
            Modal::ProfileEditor(mut form) => {
                self.show_profile_editor(context, &mut form);
            }
            Modal::NativeAweCreateSlow => self.show_native_awe_create_slow(context),
            Modal::ConfirmUnmount {
                identity,
                focus_cancel,
            } => {
                self.show_unmount_confirmation(context, identity, focus_cancel);
            }
            Modal::ConfirmDeleteProfile { id, focus_cancel } => {
                self.show_delete_confirmation(context, &id, focus_cancel);
            }
            Modal::ConfirmClearPendingCreate {
                mount_target,
                focus_cancel,
            } => {
                self.show_clear_pending_create_confirmation(context, mount_target, focus_cancel);
            }
        }
    }

    fn show_native_awe_create_slow(&mut self, context: &egui::Context) {
        let mut continue_waiting = false;
        show_styled_dialog(
            context,
            "native_awe_create_slow",
            localized_text!("msg_0649"),
            520.0,
            |ui| {
                ui.label(localized_format!(
                    "msg_0650",
                    minutes = NATIVE_AWE_CREATE_WARNING_AFTER.as_secs() / 60
                ));
                dialog_footer(ui, |ui| {
                    let dark_mode = ui.visuals().dark_mode;
                    if ui
                        .add(primary_dialog_button(
                            localized_text!("msg_0652"),
                            dark_mode,
                        ))
                        .clicked()
                    {
                        continue_waiting = true;
                    }
                });
            },
        );
        if !continue_waiting {
            self.modal = Some(Modal::NativeAweCreateSlow);
        }
    }

    fn show_help(&mut self, context: &egui::Context) {
        let mut close = false;
        show_styled_dialog(
            context,
            "help_dialog",
            localized_text!("msg_0456"),
            500.0,
            |ui| {
                ui.label(localized_text!("msg_0464"));
                ui.label(localized_text!("msg_0538"));
                ui.label(localized_text!("msg_0130"));
                ui.label(localized_text!("msg_0020"));
                ui.label(localized_text!("msg_0207"));
                dialog_footer(ui, |ui| {
                    if ui
                        .add(secondary_dialog_button(localized_text!("msg_0160")))
                        .clicked()
                    {
                        close = true;
                    }
                });
            },
        );
        if !close {
            self.modal = Some(Modal::Help);
        }
    }

    fn show_profile_editor(&mut self, context: &egui::Context, form: &mut ProfileForm) {
        let mut submit = false;
        let mut cancel = false;
        show_styled_dialog(
            context,
            "profile_editor_dialog",
            form.title(),
            510.0,
            |ui| {
                egui::Grid::new("profile_editor_grid")
                    .num_columns(2)
                    .spacing([12.0, 10.0])
                    .show(ui, |ui| {
                        if form.mode != EditorMode::CreateAndMount {
                            ui.label(localized_text!("msg_0341"));
                            ui.add(
                                egui::TextEdit::singleline(&mut form.profile.name)
                                    .desired_width(280.0),
                            );
                            ui.end_row();
                        }

                        ui.label(localized_text!("msg_0287"));
                        egui::ComboBox::from_id_salt("disk_backend")
                            .selected_text(form.profile.backend.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut form.profile.backend,
                                    DiskBackend::RamDyn,
                                    localized_text!("msg_0170"),
                                );
                                ui.selectable_value(
                                    &mut form.profile.backend,
                                    DiskBackend::AimVirtualMemory,
                                    localized_text!("msg_0435"),
                                );
                                ui.selectable_value(
                                    &mut form.profile.backend,
                                    DiskBackend::AimAweAlloc,
                                    localized_text!("msg_0563"),
                                );
                                ui.selectable_value(
                                    &mut form.profile.backend,
                                    DiskBackend::AimImageFile,
                                    localized_text!("msg_0588"),
                                );
                            });
                        ui.end_row();

                        ui.label(localized_text!("msg_0580"));
                        let adapter_ports = self
                            .snapshot
                            .as_ref()
                            .map(|snapshot| snapshot.adapter_ports.clone())
                            .unwrap_or_default();
                        let adapter_label = form
                            .profile
                            .adapter_port
                            .map(|port| localized_format!("msg_0582", port = port))
                            .unwrap_or_else(|| localized_text!("msg_0581").to_owned());
                        egui::ComboBox::from_id_salt("aim_adapter_port")
                            .selected_text(adapter_label)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut form.profile.adapter_port,
                                    None,
                                    localized_text!("msg_0581"),
                                );
                                for port in &adapter_ports {
                                    ui.selectable_value(
                                        &mut form.profile.adapter_port,
                                        Some(*port),
                                        localized_format!("msg_0582", port = port),
                                    );
                                }
                            });
                        ui.end_row();

                        if form.profile.backend == DiskBackend::AimImageFile {
                            ui.label(localized_text!("msg_0594"));
                            ui.add(
                                egui::TextEdit::singleline(&mut form.image_file_path)
                                    .desired_width(330.0)
                                    .hint_text("C:\\Images\\disk.img"),
                            );
                            ui.end_row();
                        }

                        ui.label(localized_text!("msg_0482"));
                        if form.profile.backend == DiskBackend::AimImageFile {
                            ui.horizontal(|ui| {
                                match system::image_file_size_bytes(&form.image_file_path) {
                                    Ok(bytes) => {
                                        ui.weak(localized_format!("msg_0625", bytes_gib(bytes)))
                                    }
                                    Err(_) => ui.weak(localized_text!("msg_0626")),
                                };
                            });
                        } else {
                            ui.horizontal(|ui| {
                                ui.add(
                                    egui::TextEdit::singleline(&mut form.size_value)
                                        .desired_width(110.0)
                                        .hint_text(localized_text!("msg_0438")),
                                );
                                egui::ComboBox::from_id_salt("size_unit")
                                    .selected_text(form.size_unit.label())
                                    .show_ui(ui, |ui| {
                                        ui.selectable_value(
                                            &mut form.size_unit,
                                            SizeUnit::Mib,
                                            "MiB",
                                        );
                                        ui.selectable_value(
                                            &mut form.size_unit,
                                            SizeUnit::Gib,
                                            "GiB",
                                        );
                                    });
                                if let Some(memory) = self.memory {
                                    match form.profile.backend {
                                        DiskBackend::RamDyn | DiskBackend::AimAweAlloc => {
                                            ui.weak(localized_format!(
                                                "msg_0069",
                                                bytes_gib(memory.available_physical_bytes)
                                            ))
                                        }
                                        DiskBackend::AimVirtualMemory => {
                                            ui.weak(localized_format!(
                                                "msg_0375",
                                                bytes_gib(memory.available_commit_bytes)
                                            ))
                                        }
                                        DiskBackend::AimImageFile => unreachable!(),
                                    };
                                }
                            });
                        }
                        ui.end_row();

                        ui.label(localized_text!("msg_0121"));
                        ui.horizontal(|ui| {
                            ui.radio_value(
                                &mut form.mount_as_directory,
                                false,
                                localized_text!("msg_0249"),
                            );
                            ui.radio_value(
                                &mut form.mount_as_directory,
                                true,
                                localized_text!("msg_0200"),
                            );
                        });
                        ui.end_row();

                        if form.mount_as_directory {
                            ui.label(localized_text!("msg_0083"));
                            ui.add(
                                egui::TextEdit::singleline(&mut form.directory_path)
                                    .desired_width(330.0)
                                    .hint_text("C:\\RamDisk"),
                            );
                        } else {
                            ui.label(localized_text!("msg_0249"));
                            egui::ComboBox::from_id_salt("drive_letter")
                                .selected_text(format!(
                                    "{}:",
                                    form.profile.drive_letter.to_ascii_uppercase()
                                ))
                                .show_ui(ui, |ui| {
                                    let mut letters = system::available_drive_letters();
                                    if !letters
                                        .contains(&form.profile.drive_letter.to_ascii_uppercase())
                                    {
                                        letters
                                            .push(form.profile.drive_letter.to_ascii_uppercase());
                                        letters.sort_unstable();
                                    }
                                    for letter in letters {
                                        ui.selectable_value(
                                            &mut form.profile.drive_letter,
                                            letter,
                                            format!("{letter}:"),
                                        );
                                    }
                                });
                        }
                        ui.end_row();

                        ui.label(localized_text!("msg_0176"));
                        egui::ComboBox::from_id_salt("file_system")
                            .selected_text(form.profile.file_system.label())
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut form.profile.file_system,
                                    FileSystem::Ntfs,
                                    "NTFS",
                                );
                                ui.selectable_value(
                                    &mut form.profile.file_system,
                                    FileSystem::ExFat,
                                    "exFAT",
                                );
                                ui.selectable_value(
                                    &mut form.profile.file_system,
                                    FileSystem::Fat32,
                                    "FAT32",
                                );
                            });
                        ui.end_row();

                        if form.profile.backend == DiskBackend::AimImageFile {
                            ui.label(localized_text!("msg_0417"));
                            ui.weak(localized_text!("msg_0627"));
                        } else {
                            ui.label(localized_text!("msg_0417"));
                            ui.add(
                                egui::TextEdit::singleline(&mut form.profile.volume_label)
                                    .desired_width(280.0),
                            );
                        }
                        ui.end_row();

                        if form.profile.backend != DiskBackend::AimImageFile {
                            ui.label(localized_text!("msg_0628"));
                            ui.vertical(|ui| {
                                ui.checkbox(
                                    &mut form.sync_directory_enabled,
                                    localized_text!("msg_0629"),
                                );
                                if form.sync_directory_enabled {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut form.sync_directory_path)
                                            .desired_width(330.0)
                                            .hint_text("C:\\RamDiskData"),
                                    );
                                    ui.weak(localized_text!("msg_0630"));
                                }
                            });
                            ui.end_row();
                        }
                    });

                form.profile.mount_directory = form
                    .mount_as_directory
                    .then(|| form.directory_path.trim().to_owned());

                let selected_adapter_is_available =
                    self.snapshot.as_ref().is_some_and(|snapshot| {
                        let port = form
                            .profile
                            .adapter_port
                            .or_else(|| snapshot.preferred_adapter_port());
                        snapshot.adapter_scan_complete
                            && port.is_some_and(|port| snapshot.adapter_ports.contains(&port))
                    });

                if let Err(error) = form.parsed_profile() {
                    if form.error.as_deref() != Some(error.as_str()) {
                        ui.colored_label(egui::Color32::from_rgb(190, 75, 65), error);
                    }
                }

                if let (Some(memory), Some(capacity)) =
                    (self.memory, form.requested_capacity_bytes())
                {
                    let warning = match form.profile.backend {
                        DiskBackend::RamDyn
                            if form.profile.memory_mode.requires_lock_memory_privilege()
                                && capacity > memory.available_physical_bytes =>
                        {
                            Some((localized_text!("msg_0008"), localized_text!("msg_0453")))
                        }
                        DiskBackend::RamDyn
                            if form.profile.memory_mode == MemoryMode::VirtualMemory
                                && capacity > memory.available_commit_bytes =>
                        {
                            Some((localized_text!("msg_0548"), localized_text!("msg_0489")))
                        }
                        DiskBackend::AimVirtualMemory
                            if capacity > memory.available_commit_bytes =>
                        {
                            Some((localized_text!("msg_0229"), localized_text!("msg_0450")))
                        }
                        DiskBackend::AimAweAlloc if capacity > memory.available_physical_bytes => {
                            Some((localized_text!("msg_0575"), localized_text!("msg_0453")))
                        }
                        DiskBackend::AimImageFile => None,
                        _ => None,
                    };
                    if let Some((message, explanation)) = warning {
                        ui.colored_label(egui::Color32::from_rgb(190, 100, 35), message)
                            .on_hover_text(explanation);
                    }
                }

                let target_key = form.profile.mount_target_key();
                let target_availability = system::mount_target_is_available(&form.profile);
                let target_available = target_availability
                    .as_ref()
                    .is_ok_and(|available| *available);
                if self.unresolved_create_targets.contains(&target_key) {
                    ui.colored_label(
                        egui::Color32::from_rgb(190, 100, 35),
                        localized_text!("msg_0346"),
                    );
                } else {
                    match &target_availability {
                        Ok(true) => {}
                        Ok(false) => {
                            let message = if form.mode == EditorMode::CreateAndMount {
                                localized_format!("msg_0125", form.profile.mount_target_display())
                            } else {
                                localized_format!("msg_0292", form.profile.mount_target_display())
                            };
                            ui.colored_label(egui::Color32::from_rgb(190, 100, 35), message);
                        }
                        Err(error) => {
                            let message = if form.mode == EditorMode::CreateAndMount {
                                localized_text!("msg_0436")
                            } else {
                                localized_text!("msg_0018")
                            };
                            ui.colored_label(egui::Color32::from_rgb(190, 100, 35), message)
                                .on_hover_text(error.as_str());
                        }
                    }
                }
                if form.mode == EditorMode::CreateAndMount {
                    if self.refresh_in_flight {
                        ui.weak(localized_text!("msg_0188"));
                    }
                    if let Some(error) = self.aim_state_unavailable_reason() {
                        ui.colored_label(
                            egui::Color32::from_rgb(190, 75, 65),
                            localized_format!("msg_0252", error = error),
                        );
                    }
                    if let Some(error) = self.runtime_error_for_profile(&form.profile) {
                        ui.colored_label(
                            egui::Color32::from_rgb(190, 75, 65),
                            localized_format!("msg_0284", error = error),
                        );
                    }
                    if let Some(error) = self.pending_creates.load_error() {
                        ui.colored_label(
                            egui::Color32::from_rgb(190, 75, 65),
                            localized_format!("msg_0072", error = error),
                        );
                    }
                    if !selected_adapter_is_available {
                        ui.colored_label(
                            egui::Color32::from_rgb(190, 100, 35),
                            localized_text!("msg_0583"),
                        );
                    }
                }
                if form.profile.backend == DiskBackend::AimImageFile {
                    ui.weak(localized_text!("msg_0645"));
                }

                ui.add_space(12.0);
                if form.profile.backend == DiskBackend::RamDyn {
                    ui.label(localized_text!("msg_0308"));
                    for mode in [
                        MemoryMode::Awe,
                        MemoryMode::LargePage,
                        MemoryMode::VirtualMemory,
                    ] {
                        let large_pages_supported = self.large_page_minimum_bytes.is_some();
                        let radio = if mode == MemoryMode::LargePage && !large_pages_supported {
                            ui.add_enabled(
                                false,
                                egui::RadioButton::new(
                                    form.profile.memory_mode == mode,
                                    mode.label(),
                                ),
                            )
                        } else {
                            ui.radio_value(&mut form.profile.memory_mode, mode, mode.label())
                        };
                        radio.on_hover_text(mode.description());
                        ui.weak(mode.description());
                        if mode == MemoryMode::LargePage {
                            if let Some(minimum_bytes) = self.large_page_minimum_bytes {
                                let block_bytes =
                                    minimum_bytes.max(1_u64 << form.profile.block_size_exponent);
                                ui.weak(localized_format!("msg_0128", format_size(block_bytes)));
                            }
                        }
                        if mode == MemoryMode::LargePage && !large_pages_supported {
                            ui.weak(localized_text!("msg_0477"));
                        }
                    }
                    ui.horizontal(|ui| {
                        ui.label(localized_text!("msg_0053"));
                        egui::ComboBox::from_id_salt("ramdyn_block_size")
                            .selected_text(format!(
                                "{} MiB",
                                1_u64 << form.profile.block_size_exponent.saturating_sub(20)
                            ))
                            .show_ui(ui, |ui| {
                                for exponent in 20..=26 {
                                    ui.selectable_value(
                                        &mut form.profile.block_size_exponent,
                                        exponent,
                                        format!("{} MiB", 1_u64 << (exponent - 20)),
                                    );
                                }
                            });
                    });
                    ui.weak(localized_text!("msg_0416"));
                    if form.profile.memory_mode.requires_lock_memory_privilege() {
                        match &self.lock_memory_privilege {
                            Ok(true) => {
                                ui.weak(localized_text!("msg_0241"));
                            }
                            Ok(false) => {
                                let message = if form.mode == EditorMode::CreateAndMount {
                                    localized_text!("msg_0033")
                                } else {
                                    localized_text!("msg_0437")
                                };
                                ui.colored_label(egui::Color32::from_rgb(190, 100, 35), message);
                            }
                            Err(error) => {
                                let message = if form.mode == EditorMode::CreateAndMount {
                                    localized_text!("msg_0064")
                                } else {
                                    localized_text!("msg_0098")
                                };
                                ui.colored_label(egui::Color32::from_rgb(190, 100, 35), message)
                                    .on_hover_text(error.as_str());
                            }
                        }
                        if matches!(self.lock_memory_privilege.as_ref(), Ok(false)) {
                            ui.collapsing(localized_text!("msg_0444"), |ui| {
                                ui.label(localized_text!("msg_0314"));
                                ui.label(localized_text!("msg_0055"));
                                ui.label(localized_text!("msg_0087"));
                            });
                        }
                    }
                } else {
                    ui.label(localized_text!("msg_0353"));
                    ui.weak(localized_text!("msg_0296"));
                }
                ui.checkbox(
                    &mut form.profile.removable_media,
                    localized_text!("msg_0054"),
                );
                ui.add_space(8.0);
                ui.weak(localized_format!(
                    "msg_0090",
                    form.profile.file_system.label(),
                    match form.profile.backend {
                        DiskBackend::RamDyn => localized_text!("msg_0066"),
                        DiskBackend::AimVirtualMemory => localized_text!("msg_0381"),
                        DiskBackend::AimAweAlloc => localized_text!("msg_0577"),
                        DiskBackend::AimImageFile => localized_text!("msg_0631"),
                    }
                ));
                ui.weak(localized_text!("msg_0280"));
                ui.weak(localized_text!("msg_0389"));
                if form.mode == EditorMode::CreateAndMount {
                    if let Some(error) = self.profiles.load_error() {
                        ui.weak(localized_text!("msg_0466")).on_hover_text(error);
                    } else {
                        ui.checkbox(&mut form.save_after_create, localized_text!("msg_0362"));
                        if form.save_after_create {
                            ui.horizontal(|ui| {
                                ui.label(localized_text!("msg_0341"));
                                ui.add(
                                    egui::TextEdit::singleline(&mut form.profile.name)
                                        .desired_width(280.0),
                                );
                            });
                        }
                    }
                }
                if form.mode != EditorMode::CreateAndMount || form.save_after_create {
                    ui.checkbox(&mut form.profile.auto_mount, localized_text!("msg_0073"));
                    ui.weak(match self.login_startup_enabled {
                        Some(true) => localized_text!("msg_0325"),
                        Some(false) => localized_text!("msg_0238"),
                        None => localized_text!("msg_0326"),
                    });
                }
                if let Some(error) = &form.error {
                    ui.colored_label(egui::Color32::from_rgb(190, 75, 65), error);
                }
                dialog_footer(ui, |ui| {
                    let target_key = form.profile.mount_target_key();
                    let profile_is_valid = form.parsed_profile().is_ok();
                    let runtime_available = self.runtime_error_for_profile(&form.profile).is_none();
                    let memory_mode_available = form.profile.backend != DiskBackend::RamDyn
                        || self
                            .memory_mode_unavailable_reason(form.profile.memory_mode)
                            .is_none();
                    let can_submit = profile_is_valid
                        && (form.mode != EditorMode::CreateAndMount
                            || (!self.refresh_in_flight
                                && self.aim_state_unavailable_reason().is_none()
                                && self.pending_creates.load_error().is_none()
                                && runtime_available
                                && memory_mode_available
                                && selected_adapter_is_available
                                && !self.unresolved_create_targets.contains(&target_key)
                                && target_available));
                    let submit_button =
                        primary_dialog_button(form.submit_label(), ui.visuals().dark_mode);
                    if ui.add_enabled(can_submit, submit_button).clicked() {
                        submit = true;
                    }
                    ui.add_space(8.0);
                    if ui
                        .add(secondary_dialog_button(localized_text!("msg_0113")))
                        .clicked()
                    {
                        cancel = true;
                    }
                });
            },
        );

        if submit {
            match form.parsed_profile() {
                Err(error) => {
                    form.error = Some(error);
                    self.modal = Some(Modal::ProfileEditor(form.clone()));
                }
                Ok(profile) => match form.mode {
                    EditorMode::CreateAndMount => {
                        if self
                            .unresolved_create_targets
                            .contains(&profile.mount_target_key())
                        {
                            form.error = Some(localized_text!("msg_0277").to_owned());
                            self.modal = Some(Modal::ProfileEditor(form.clone()));
                        } else {
                            self.start_create(profile, form.clone());
                        }
                    }
                    EditorMode::SaveConfig | EditorMode::Edit => {
                        let profile_id = profile.id.clone();
                        match self.profiles.upsert(profile) {
                            Ok(()) => {
                                self.unsaved_profiles
                                    .retain(|profile| profile.id != profile_id);
                                self.notice = Some(Notice {
                                    text: localized_text!("msg_0029").to_owned(),
                                    is_error: false,
                                });
                            }
                            Err(error) => {
                                form.error = Some(error);
                                self.modal = Some(Modal::ProfileEditor(form.clone()));
                            }
                        }
                    }
                },
            }
        } else if !cancel {
            self.modal = Some(Modal::ProfileEditor(form.clone()));
        }
    }

    fn show_unmount_confirmation(
        &mut self,
        context: &egui::Context,
        identity: DeviceIdentity,
        focus_cancel: bool,
    ) {
        let device = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| {
                snapshot
                    .devices
                    .iter()
                    .find(|device| &device.identity == &identity)
            })
            .cloned();
        let known_ram_disk = device
            .as_ref()
            .is_some_and(|device| self.created_modes.contains_key(&device.identity));
        let sync_directory = single_volume_guid_for_identity(&identity).and_then(|volume_guid| {
            self.active_disk_modes
                .sync_directory_for(volume_guid, identity.size_bytes)
                .map(str::to_owned)
                .or_else(|| {
                    self.unrecorded_sync_directories
                        .get(&volume_guid.to_ascii_lowercase())
                        .filter(|(size_bytes, _)| *size_bytes == identity.size_bytes)
                        .map(|(_, directory)| directory.clone())
                })
        });
        let snapshot_ready = self.snapshot_error.is_none()
            && self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.mount_scan_complete);
        let can_confirm_unmount = snapshot_ready
            && self.pending.is_none()
            && !self.refresh_in_flight
            && !self.unresolved_unmounts.contains(&identity)
            && device
                .as_ref()
                .is_some_and(crate::aim::AimDevice::can_unmount);
        let mut unmount_force = None;
        let mut close_requested = false;
        show_styled_dialog(
            context,
            "unmount_confirmation",
            localized_text!("msg_0025"),
            560.0,
            |ui| {
                if let Some(device) = &device {
                    let mounts = if device.mount_points.is_empty() {
                        device.display_mount_point()
                    } else {
                        device
                            .mount_points
                            .iter()
                            .map(crate::aim::MountPoint::display)
                            .collect::<Vec<_>>()
                            .join(localized_text!("msg_0562"))
                    };
                    if known_ram_disk {
                        ui.label(localized_text!("msg_0500"));
                    } else {
                        match device.identity.flags & 0x0000_0F00 {
                            0x0000_0100 => ui.label(localized_text!("msg_0542")),
                            0x0000_0200 => ui.label(localized_text!("msg_0533")),
                            0x0000_0300 => ui.label(localized_text!("msg_0144")),
                            _ => ui.label(localized_text!("msg_0058")),
                        };
                    };
                    ui.label(localized_format!("msg_0307", mounts = mounts));
                    if device.volume_guids.len() > 1 {
                        ui.colored_label(
                            egui::Color32::from_rgb(190, 100, 35),
                            localized_format!("msg_0430", device.volume_guids.len()),
                        );
                        for volume in &device.volume_associations {
                            let mount_points = device
                                .mount_points
                                .iter()
                                .filter(|mount| {
                                    mount.volume_guid.as_deref().is_some_and(|guid| {
                                        guid.eq_ignore_ascii_case(&volume.volume_guid)
                                    })
                                })
                                .map(crate::aim::MountPoint::display)
                                .collect::<Vec<_>>();
                            let mount_summary = if mount_points.is_empty() {
                                localized_text!("msg_0368").to_owned()
                            } else {
                                mount_points.join(localized_text!("msg_0562"))
                            };
                            ui.add(
                                egui::Label::new(localized_format!(
                                    "msg_0046",
                                    volume.volume_guid,
                                    mount_summary
                                ))
                                .wrap(),
                            );
                        }
                    }
                    ui.label(format!(
                        "{} · {}",
                        device.device_type_label(),
                        format_size(device.identity.size_bytes)
                    ));
                    ui.label(identity.label());
                    if let Some(directory) = &sync_directory {
                        ui.add(
                            egui::Label::new(localized_format!("msg_0643", directory = directory))
                                .wrap(),
                        );
                        ui.weak(localized_text!("msg_0644"));
                    }
                } else {
                    ui.label(localized_text!("msg_0169"));
                }
                ui.add_space(6.0);
                ui.weak(localized_text!("msg_0142"));
                ui.colored_label(
                    egui::Color32::from_rgb(190, 100, 35),
                    localized_text!("msg_0297"),
                );
                if let Some(error) = self.snapshot_error.as_deref() {
                    ui.colored_label(
                        egui::Color32::from_rgb(190, 75, 65),
                        localized_format!("msg_0315", error = error),
                    );
                } else if let Some(reason) = device
                    .as_ref()
                    .and_then(crate::aim::AimDevice::unmount_block_reason)
                {
                    ui.weak(reason);
                }
                if self.unresolved_unmounts.contains(&identity) {
                    ui.colored_label(
                        egui::Color32::from_rgb(190, 100, 35),
                        localized_text!("msg_0448"),
                    );
                }
                if self.refresh_in_flight {
                    ui.weak(localized_text!("msg_0010"));
                }
                dialog_footer(ui, |ui| {
                    let dark_mode = ui.visuals().dark_mode;
                    let cancel = ui.add(secondary_dialog_button(localized_text!("msg_0113")));
                    if focus_cancel {
                        cancel.request_focus();
                    }
                    if cancel.clicked() {
                        close_requested = true;
                    }
                    ui.add_space(8.0);
                    if ui
                        .add_enabled(
                            can_confirm_unmount,
                            danger_dialog_button(localized_text!("msg_0378"), dark_mode),
                        )
                        .clicked()
                    {
                        unmount_force = Some(true);
                    }
                    ui.add_space(8.0);
                    if ui
                        .add_enabled(
                            can_confirm_unmount,
                            warning_dialog_button(localized_text!("msg_0439"), dark_mode),
                        )
                        .clicked()
                    {
                        unmount_force = Some(false);
                    }
                });
            },
        );
        if let Some(force) = unmount_force {
            self.start_unmount(identity, force);
        } else if !close_requested {
            self.modal = Some(Modal::ConfirmUnmount {
                identity,
                focus_cancel: false,
            });
        }
    }

    fn show_delete_confirmation(&mut self, context: &egui::Context, id: &str, focus_cancel: bool) {
        let profile = self
            .profiles
            .profiles()
            .iter()
            .find(|profile| profile.id == id)
            .cloned();
        let mut confirm = false;
        let mut close_requested = false;
        show_styled_dialog(
            context,
            "delete_profile_confirmation",
            localized_text!("msg_0178"),
            480.0,
            |ui| {
                if let Some(profile) = &profile {
                    ui.label(localized_format!("msg_0049", profile.name));
                    ui.label(localized_text!("msg_0397"));
                } else {
                    ui.label(localized_text!("msg_0259"));
                }
                dialog_footer(ui, |ui| {
                    let dark_mode = ui.visuals().dark_mode;
                    if profile.is_some()
                        && ui
                            .add(danger_dialog_button(localized_text!("msg_0178"), dark_mode))
                            .clicked()
                    {
                        confirm = true;
                    }
                    ui.add_space(8.0);
                    let cancel = ui.add(secondary_dialog_button(localized_text!("msg_0113")));
                    if focus_cancel {
                        cancel.request_focus();
                    }
                    if cancel.clicked() {
                        close_requested = true;
                    }
                });
            },
        );
        if confirm {
            match self.profiles.delete(id) {
                Ok(()) => {
                    self.notice = Some(Notice {
                        text: localized_text!("msg_0209").to_owned(),
                        is_error: false,
                    });
                }
                Err(error) => {
                    self.notice = Some(Notice {
                        text: error,
                        is_error: true,
                    });
                }
            }
        } else if !close_requested {
            self.modal = Some(Modal::ConfirmDeleteProfile {
                id: id.to_owned(),
                focus_cancel: false,
            });
        }
    }

    fn show_clear_pending_create_confirmation(
        &mut self,
        context: &egui::Context,
        mount_target: MountTargetKey,
        focus_cancel: bool,
    ) {
        let target_display = mount_target.label();
        let pending = self
            .pending_creates
            .pending_creates()
            .iter()
            .find(|entry| entry.profile.mount_target_key() == mount_target)
            .cloned();
        let devices_at_target = self.snapshot.as_ref().map(|snapshot| {
            snapshot
                .devices
                .iter()
                .filter(|device| {
                    device
                        .mount_points
                        .iter()
                        .any(|mount| MountTargetKey::from_mount_path(&mount.path) == mount_target)
                })
                .map(|device| {
                    format!(
                        "{} · {} · {}",
                        device.display_mount_point(),
                        format_size(device.identity.size_bytes),
                        device.identity.label()
                    )
                })
                .collect::<Vec<_>>()
        });
        let can_confirm = pending.is_some()
            && self.pending.is_none()
            && !self.refresh_in_flight
            && self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.mount_scan_complete)
            && self.snapshot_error.is_none();
        let mut confirm = false;
        let mut close_requested = false;
        show_styled_dialog(
            context,
            "clear_pending_confirmation",
            localized_text!("msg_0290"),
            520.0,
            |ui| {
                if let Some(entry) = &pending {
                    ui.label(localized_format!(
                        "msg_0462",
                        target_display = target_display
                    ));
                    ui.label(localized_format!("msg_0172", entry.profile.name));
                    ui.label(localized_text!("msg_0360"));
                } else {
                    ui.label(localized_format!(
                        "msg_0442",
                        target_display = target_display
                    ));
                }

                let mount_scan_complete = self
                    .snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.mount_scan_complete);
                match devices_at_target.as_deref() {
                    Some(devices) if devices.is_empty() && mount_scan_complete => {
                        ui.weak(localized_text!("msg_0157"));
                    }
                    Some(devices) if devices.is_empty() => {
                        ui.colored_label(
                            egui::Color32::from_rgb(190, 100, 35),
                            localized_text!("msg_0537"),
                        );
                    }
                    Some(devices) => {
                        ui.label(if mount_scan_complete {
                            localized_text!("msg_0152")
                        } else {
                            localized_text!("msg_0093")
                        });
                        for device in devices {
                            ui.weak(device.as_str());
                        }
                    }
                    None => {
                        ui.colored_label(
                            egui::Color32::from_rgb(190, 75, 65),
                            localized_text!("msg_0243"),
                        );
                    }
                };
                if let Some(error) = &self.snapshot_error {
                    ui.colored_label(
                        egui::Color32::from_rgb(190, 75, 65),
                        localized_format!("msg_0319", error = error),
                    );
                }
                dialog_footer(ui, |ui| {
                    let dark_mode = ui.visuals().dark_mode;
                    if ui
                        .add_enabled(
                            can_confirm,
                            warning_dialog_button(localized_text!("msg_0522"), dark_mode),
                        )
                        .clicked()
                    {
                        confirm = true;
                    }
                    ui.add_space(8.0);
                    let cancel = ui.add(secondary_dialog_button(localized_text!("msg_0113")));
                    if focus_cancel {
                        cancel.request_focus();
                    }
                    if cancel.clicked() {
                        close_requested = true;
                    }
                });
            },
        );
        if confirm {
            match self.clear_pending_create_guard(&mount_target) {
                Ok(()) => {
                    self.notice = Some(Notice {
                        text: localized_format!("msg_0540", target_display = target_display),
                        is_error: false,
                    });
                }
                Err(error) => {
                    self.notice = Some(Notice {
                        text: localized_format!(
                            "msg_0060",
                            target_display = target_display,
                            error = error
                        ),
                        is_error: true,
                    });
                }
            }
        } else if !close_requested {
            self.modal = Some(Modal::ConfirmClearPendingCreate {
                mount_target,
                focus_cancel: false,
            });
        }
    }

    fn show_close_prompt(&mut self, context: &egui::Context) {
        if !self.close_prompt {
            return;
        }
        let startup_pending = self.startup_run_active || self.startup_waiting_for_snapshot;
        let startup_setting_pending = self.login_startup_change_pending;
        let operation_pending =
            self.pending.is_some() || startup_pending || startup_setting_pending;
        let startup_waiting = self.pending.is_none() && startup_pending;
        let title = if operation_pending {
            localized_text!("msg_0266")
        } else {
            localized_text!("msg_0205")
        };
        let message = if startup_waiting {
            localized_text!("msg_0495")
        } else if startup_setting_pending && self.pending.is_none() {
            localized_text!("msg_0267")
        } else if operation_pending {
            localized_text!("msg_0365")
        } else {
            localized_text!("msg_0427")
        };
        let return_label = if operation_pending {
            localized_text!("msg_0246")
        } else {
            localized_text!("msg_0026")
        };
        let mut return_to_manager = false;
        let mut exit_manager = false;
        show_styled_dialog(context, "close_confirmation", title, 480.0, |ui| {
            ui.label(message);
            dialog_footer(ui, |ui| {
                let dark_mode = ui.visuals().dark_mode;
                if ui
                    .add(danger_dialog_button(localized_text!("msg_0104"), dark_mode))
                    .clicked()
                {
                    exit_manager = true;
                }
                ui.add_space(8.0);
                if ui.add(secondary_dialog_button(return_label)).clicked() {
                    return_to_manager = true;
                }
            });
        });
        if return_to_manager || exit_manager {
            self.close_prompt = false;
        }
        if exit_manager {
            self.allow_close = true;
            context.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for ManagerApp {
    fn update(&mut self, context: &egui::Context, frame: &mut eframe::Frame) {
        self.consume_events();
        self.maybe_warn_slow_native_awe_create();
        self.handle_keyboard_shortcuts(context);

        let close_requested = context.input(|input| input.viewport().close_requested());
        let disk_operation_unresolved = self.pending.is_some()
            || self.startup_run_active
            || self.startup_waiting_for_snapshot
            || !self.unresolved_create_targets.is_empty()
            || !self.unresolved_unmounts.is_empty();
        if close_requested
            && (disk_operation_unresolved || self.login_startup_change_pending)
            && !self.allow_close
        {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.close_prompt = true;
        }

        let focused = context.input(|input| input.viewport().focused.unwrap_or(false));
        if focused && !self.window_was_focused {
            self.request_refresh();
        }
        self.window_was_focused = focused;

        let window_visible = native_window_is_visible(frame, context);
        if window_visible && self.last_memory_sample.elapsed() >= MEMORY_REFRESH_INTERVAL {
            self.refresh_memory();
        }

        self.show_top_bar(context);
        egui::TopBottomPanel::bottom("manager_feedback").show(context, |ui| {
            ui.add_enabled_ui(self.modal.is_none() && !self.close_prompt, |ui| {
                self.show_notice(ui);
            });
        });
        egui::CentralPanel::default().show(context, |ui| {
            ui.add_enabled_ui(self.modal.is_none() && !self.close_prompt, |ui| {
                let profiles_tab_label = if self.profiles.load_error().is_some() {
                    localized_text!("msg_0137").to_owned()
                } else {
                    localized_format!("msg_0423", self.profiles.profiles().len())
                };
                let mut language_override = self.language_override;
                ui.horizontal(|ui| {
                    if ui
                        .selectable_label(self.tab == MainTab::Disks, self.disk_tab_label())
                        .clicked()
                    {
                        self.tab = MainTab::Disks;
                    }
                    if ui
                        .selectable_label(self.tab == MainTab::Profiles, profiles_tab_label)
                        .clicked()
                    {
                        self.tab = MainTab::Profiles;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        egui::ComboBox::from_id_salt("manager-language")
                            .selected_text(match language_override {
                                None => localized_text!("msg_0086"),
                                Some(Language::Chinese) => localized_text!("msg_0343"),
                                Some(Language::English) => localized_text!("msg_0415"),
                            })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut language_override,
                                    None,
                                    localized_text!("msg_0086"),
                                );
                                ui.selectable_value(
                                    &mut language_override,
                                    Some(Language::Chinese),
                                    localized_text!("msg_0343"),
                                );
                                ui.selectable_value(
                                    &mut language_override,
                                    Some(Language::English),
                                    localized_text!("msg_0415"),
                                );
                            });
                    });
                });
                if language_override != self.language_override {
                    self.set_language_override(language_override, ui.ctx());
                }
                ui.separator();
                match self.tab {
                    MainTab::Disks => self.show_disks(ui),
                    MainTab::Profiles => self.show_profiles(ui),
                }
                if let Some(warning) = &self.font_warning {
                    ui.weak(warning);
                }
                if self.profiles.load_error().is_some()
                    && !self.profiles.path().as_os_str().is_empty()
                {
                    ui.weak(localized_format!(
                        "msg_0041",
                        self.profiles.path().display()
                    ));
                }
            });
        });

        self.show_modal(context);
        self.show_close_prompt(context);

        if window_visible {
            context.request_repaint_after(
                MEMORY_REFRESH_INTERVAL.saturating_sub(self.last_memory_sample.elapsed()),
            );
        }
    }
}

#[cfg(windows)]
fn native_window_is_visible(frame: &eframe::Frame, context: &egui::Context) -> bool {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible;

    if context.input(|input| input.viewport().minimized == Some(true)) {
        return false;
    }
    frame
        .window_handle()
        .ok()
        .and_then(|handle| match handle.as_raw() {
            RawWindowHandle::Win32(handle) => {
                Some(unsafe { IsWindowVisible(handle.hwnd.get() as _) != 0 })
            }
            _ => None,
        })
        .unwrap_or(true)
}

#[cfg(not(windows))]
fn native_window_is_visible(_frame: &eframe::Frame, context: &egui::Context) -> bool {
    context.input(|input| input.viewport().minimized != Some(true))
}

impl ManagerApp {
    fn maybe_warn_slow_native_awe_create(&mut self) {
        let should_warn = self.pending.as_ref().is_some_and(|operation| {
            !operation.slow_awe_warning_shown
                && operation.started_at.elapsed() >= NATIVE_AWE_CREATE_WARNING_AFTER
                && operation.create_form.as_ref().is_some_and(|form| {
                    form.profile.backend == DiskBackend::AimAweAlloc
                })
        });
        if !should_warn || self.modal.is_some() {
            return;
        }

        if let Some(operation) = self.pending.as_mut() {
            operation.slow_awe_warning_shown = true;
        }
        self.modal = Some(Modal::NativeAweCreateSlow);
    }

    fn disk_tab_label(&self) -> String {
        let count = self
            .snapshot
            .as_ref()
            .map_or(0, |snapshot| snapshot.devices.len());
        localized_format!("msg_0516", count = count)
    }
}

fn start_worker(
    context: egui::Context,
    runtime: RuntimePaths,
) -> Result<(Sender<WorkerCommand>, Receiver<WorkerEvent>), String> {
    let (command_sender, command_receiver) = mpsc::channel();
    let (event_sender, event_receiver) = mpsc::channel();
    thread::Builder::new()
        .name("aim-disk-worker".to_owned())
        .spawn(move || {
            while let Ok(command) = command_receiver.recv() {
                match command {
                    WorkerCommand::Refresh => {
                        let result = crate::aim::enumerate(&runtime.aimapi_backend_dll);
                        send_event(&event_sender, &context, WorkerEvent::Snapshot(result));
                    }
                    WorkerCommand::QueryLoginStartup => {
                        let result = crate::startup::login_startup_enabled();
                        send_event(
                            &event_sender,
                            &context,
                            WorkerEvent::LoginStartupState(result),
                        );
                    }
                    WorkerCommand::SetLoginStartup(enabled) => {
                        let result = crate::startup::set_login_startup_enabled(enabled);
                        send_event(
                            &event_sender,
                            &context,
                            WorkerEvent::LoginStartupChanged(enabled, result),
                        );
                    }
                    WorkerCommand::Create(profile) => {
                        if profile.backend == DiskBackend::AimAweAlloc {
                            // AIM's native AWE path is synchronous and has no safe cancel API.
                            // Keep a slow call off the command worker so inventory refresh can
                            // still run after the UI warns that the operation is overdue.
                            let action_profile = profile.clone();
                            let action_runtime = runtime.clone();
                            let action_context = context.clone();
                            let action_events = event_sender.clone();
                            let spawn_result = thread::Builder::new()
                                .name("aim-native-awe-create".to_owned())
                                .spawn(move || {
                                    let result = ramdyn::create_and_wait(
                                        &action_profile,
                                        &action_runtime,
                                    )
                                    .map(WorkerActionResult::Created)
                                    .map_err(WorkerActionError::Create);
                                    let snapshot =
                                        crate::aim::enumerate(&action_runtime.aimapi_backend_dll);
                                    send_event(
                                        &action_events,
                                        &action_context,
                                        WorkerEvent::Completed {
                                            operation: WorkerOperation::Create(action_profile),
                                            result,
                                            snapshot,
                                        },
                                    );
                                });
                            if let Err(error) = spawn_result {
                                let result = Err(WorkerActionError::Create(ramdyn::CreateError {
                                    message: localized_format!("msg_0173", error = error),
                                    outcome_unknown: false,
                                }));
                                let snapshot = crate::aim::enumerate(&runtime.aimapi_backend_dll);
                                send_event(
                                    &event_sender,
                                    &context,
                                    WorkerEvent::Completed {
                                        operation: WorkerOperation::Create(profile),
                                        result,
                                        snapshot,
                                    },
                                );
                            }
                        } else {
                            let result = ramdyn::create_and_wait(&profile, &runtime)
                                .map(WorkerActionResult::Created)
                                .map_err(WorkerActionError::Create);
                            let snapshot = crate::aim::enumerate(&runtime.aimapi_backend_dll);
                            send_event(
                                &event_sender,
                                &context,
                                WorkerEvent::Completed {
                                    operation: WorkerOperation::Create(profile),
                                    result,
                                    snapshot,
                                },
                            );
                        }
                    }
                    WorkerCommand::Unmount(identity, force, sync_directory) => {
                        let result = ramdyn::unmount(
                            identity.clone(),
                            force,
                            &runtime,
                            sync_directory.clone(),
                        )
                        .map(
                            |(mount_point, persistence_warning)| WorkerActionResult::Unmounted {
                                mount_point,
                                persistence_warning,
                            },
                        )
                        .map_err(WorkerActionError::Unmount);
                        let snapshot = crate::aim::enumerate(&runtime.aimapi_backend_dll);
                        send_event(
                            &event_sender,
                            &context,
                            WorkerEvent::Completed {
                                operation: WorkerOperation::Unmount(identity, force),
                                result,
                                snapshot,
                            },
                        );
                    }
                }
            }
        })
        .map_err(|error| localized_format!("msg_0173", error = error))?;
    Ok((command_sender, event_receiver))
}

fn send_event(sender: &Sender<WorkerEvent>, context: &egui::Context, event: WorkerEvent) {
    if sender.send(event).is_ok() {
        context.request_repaint();
    }
}

fn operation_label(operation: &PendingOperation) -> &'static str {
    if operation.slow_awe_warning_shown {
        localized_text!("msg_0651")
    } else {
        operation.label
    }
}

fn low_frequency_spinner(ui: &mut egui::Ui) {
    const DOT_COUNT: usize = 8;

    let size = ui.spacing().interact_size.y;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    response.widget_info(|| egui::WidgetInfo::new(egui::WidgetType::ProgressIndicator));
    if !ui.is_rect_visible(rect) {
        return;
    }

    ui.ctx().request_repaint_after(STATUS_SPINNER_INTERVAL);
    let time = ui.input(|input| input.time);
    let phase = ((time / STATUS_SPINNER_INTERVAL.as_secs_f64()).floor() as usize) % DOT_COUNT;
    let center = rect.center();
    let radius = rect.width().min(rect.height()) * 0.35;
    let dot_radius = (radius * 0.22).max(1.0);
    let color = ui.visuals().strong_text_color();

    for index in 0..DOT_COUNT {
        let angle = std::f32::consts::TAU * index as f32 / DOT_COUNT as f32;
        let position = center + egui::vec2(angle.cos(), angle.sin()) * radius;
        let age = (index + DOT_COUNT - phase) % DOT_COUNT;
        let opacity = 1.0 - 0.75 * age as f32 / DOT_COUNT as f32;
        ui.painter()
            .circle_filled(position, dot_radius, color.gamma_multiply(opacity));
    }
}

fn bytes_gib(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0 * 1024.0)
}

fn stable_table_button(
    ui: &mut egui::Ui,
    id: egui::Id,
    label: &str,
    desired_size: egui::Vec2,
    selected: bool,
    frame: bool,
) -> egui::Response {
    // Reserve the usual widget slot, but attach interaction state to the row identity.
    let (_, rect) = ui.allocate_space(desired_size);
    let response = ui.interact(rect, id, egui::Sense::click());
    response.widget_info(|| {
        egui::WidgetInfo::labeled(egui::WidgetType::Button, ui.is_enabled(), label)
    });

    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let button_padding = if frame {
            ui.spacing().button_padding
        } else {
            egui::Vec2::ZERO
        };
        let (frame_expansion, frame_corner_radius, frame_fill, frame_stroke) = if selected {
            let selection = ui.visuals().selection;
            (
                egui::Vec2::ZERO,
                egui::CornerRadius::ZERO,
                selection.bg_fill,
                selection.stroke,
            )
        } else if frame {
            (
                egui::Vec2::splat(visuals.expansion),
                visuals.corner_radius,
                visuals.weak_bg_fill,
                visuals.bg_stroke,
            )
        } else {
            (
                egui::Vec2::ZERO,
                egui::CornerRadius::ZERO,
                egui::Color32::TRANSPARENT,
                egui::Stroke::NONE,
            )
        };
        ui.painter().rect(
            rect.expand2(frame_expansion),
            frame_corner_radius,
            frame_fill,
            frame_stroke,
            egui::StrokeKind::Inside,
        );

        let available_text_width = (rect.width() - 2.0 * button_padding.x).max(0.0);
        let galley = egui::WidgetText::from(label).into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            available_text_width,
            egui::TextStyle::Button,
        );
        let text_rect = rect.shrink2(button_padding);
        let text_pos = ui
            .layout()
            .align_size_within_rect(galley.size(), text_rect)
            .min;
        ui.painter().galley(text_pos, galley, visuals.text_color());
    }

    response
}

fn format_size(bytes: u64) -> String {
    const MIB: u64 = 1024 * 1024;
    const GIB: u64 = 1024 * MIB;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB as f64)
    } else {
        format!("{} MiB", bytes / MIB)
    }
}

fn open_mount_point(path: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;

        let mut buffer = vec![0_u16; 260];
        let windows_directory = loop {
            let length =
                unsafe { GetWindowsDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
            if length == 0 {
                return Err(localized_format!(
                    "msg_0141",
                    std::io::Error::last_os_error()
                ));
            }
            if length < buffer.len() {
                break std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length]));
            }
            if length > 32_768 {
                return Err(localized_text!("msg_0094").to_owned());
            }
            buffer.resize(length.saturating_add(1), 0);
        };
        let explorer = windows_directory.join("explorer.exe");
        std::process::Command::new(&explorer)
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|error| {
                localized_format!("msg_0418", explorer.display(), path = path, error = error)
            })
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Err(localized_text!("msg_0216").to_owned())
    }
}

fn install_chinese_font(context: &egui::Context) -> Option<String> {
    #[cfg(windows)]
    {
        let Some(windows) = std::env::var_os("WINDIR").map(std::path::PathBuf::from) else {
            return Some(localized_text!("msg_0279").to_owned());
        };
        let candidates = ["Fonts\\msyh.ttc", "Fonts\\msyh.ttf", "Fonts\\simhei.ttf"];
        for candidate in candidates {
            let path = windows.join(candidate);
            if let Ok(bytes) = std::fs::read(path) {
                let mut fonts = egui::FontDefinitions::default();
                let name = "windows_cjk".to_owned();
                fonts
                    .font_data
                    .insert(name.clone(), egui::FontData::from_owned(bytes).into());
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, name.clone());
                fonts
                    .families
                    .entry(egui::FontFamily::Monospace)
                    .or_default()
                    .insert(0, name);
                context.set_fonts(fonts);
                return None;
            }
        }
        Some(localized_text!("msg_0225").to_owned())
    }
    #[cfg(not(windows))]
    {
        let _ = context;
        None
    }
}

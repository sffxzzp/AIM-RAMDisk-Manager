use crate::i18n::Language;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub const SCHEMA_VERSION: u32 = 4;
const PENDING_CREATE_SCHEMA_VERSION: u32 = 1;
const ACTIVE_DISK_MODE_SCHEMA_VERSION: u32 = 2;
const MANAGER_SETTINGS_SCHEMA_VERSION: u32 = 3;
static NEXT_PROFILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

// The referenced RamDyn source defines MBR_SPACE as 4096 bytes. This manager
// passes a 512-byte sector size; keep the last partition sector in MBR's u32 LBA range.
const RAMDYN_SECTOR_SIZE_BYTES: u64 = 512;
pub const RAMDYN_MBR_SPACE_BYTES: u64 = 4096;
const RAMDYN_SECTORS_PER_MIB: u64 = 1024 * 1024 / RAMDYN_SECTOR_SIZE_BYTES;
const RAMDYN_MAX_SIZE_MIB: u64 =
    (u32::MAX as u64 - RAMDYN_MBR_SPACE_BYTES / RAMDYN_SECTOR_SIZE_BYTES + 1)
        / RAMDYN_SECTORS_PER_MIB;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryMode {
    VirtualMemory,
    Awe,
    LargePage,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskBackend {
    #[default]
    RamDyn,
    AimVirtualMemory,
    AimAweAlloc,
    AimImageFile,
}

impl DiskBackend {
    pub fn label(self) -> &'static str {
        match self {
            Self::RamDyn => localized_text!("msg_0170"),
            Self::AimVirtualMemory => localized_text!("msg_0435"),
            Self::AimAweAlloc => localized_text!("msg_0563"),
            Self::AimImageFile => localized_text!("msg_0588"),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileSystem {
    #[default]
    Ntfs,
    ExFat,
    Fat32,
}

impl FileSystem {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ntfs => "NTFS",
            Self::ExFat => "exFAT",
            Self::Fat32 => "FAT32",
        }
    }

    pub fn format_argument(self) -> &'static str {
        match self {
            Self::Ntfs => "/fs:NTFS",
            Self::ExFat => "/fs:exFAT",
            Self::Fat32 => "/fs:FAT32",
        }
    }

    pub fn maximum_label_utf16_units(self) -> usize {
        match self {
            Self::Ntfs => 32,
            Self::ExFat => 15,
            Self::Fat32 => 11,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum MountTargetKey {
    DriveLetter(char),
    Directory(String),
}

impl MountTargetKey {
    pub fn from_mount_path(path: &str) -> Self {
        if let Some(letter) = drive_letter_from_path(path) {
            Self::DriveLetter(letter.to_ascii_uppercase())
        } else {
            Self::Directory(normalize_mount_directory(path))
        }
    }

    pub fn label(&self) -> String {
        match self {
            Self::DriveLetter(letter) => format!("{letter}:"),
            Self::Directory(path) => path.clone(),
        }
    }
}

impl MemoryMode {
    pub fn ramdyn_argument(self) -> &'static str {
        match self {
            Self::VirtualMemory => "0",
            Self::Awe => "1",
            Self::LargePage => "2",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::VirtualMemory => localized_text!("msg_0103"),
            Self::Awe => "AWE",
            Self::LargePage => "Large-page",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::VirtualMemory => localized_text!("msg_0100"),
            Self::Awe => localized_text!("msg_0286"),
            Self::LargePage => localized_text!("msg_0421"),
        }
    }

    pub fn requires_lock_memory_privilege(self) -> bool {
        matches!(self, Self::Awe | Self::LargePage)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DiskProfile {
    pub id: String,
    pub name: String,
    pub size_mib: u64,
    pub drive_letter: char,
    pub volume_label: String,
    pub memory_mode: MemoryMode,
    #[serde(default)]
    pub backend: DiskBackend,
    /// Preferred AIM SCSI port for creation. `None` selects the first
    /// currently available adapter and keeps older profiles usable.
    #[serde(default)]
    pub adapter_port: Option<u8>,
    #[serde(default)]
    pub file_system: FileSystem,
    #[serde(default = "default_block_size_exponent")]
    pub block_size_exponent: u8,
    #[serde(default)]
    pub removable_media: bool,
    #[serde(default)]
    pub mount_directory: Option<String>,
    #[serde(default)]
    pub auto_mount: bool,
    /// Optional directory loaded at creation and mirrored back on normal unmount.
    #[serde(default)]
    pub sync_directory: Option<String>,
    /// Existing AIM-supported disk image mounted directly from this file.
    #[serde(default)]
    pub image_file: Option<String>,
}

impl DiskProfile {
    pub fn validate(&self) -> Result<(), String> {
        if self.name.chars().any(|character| character.is_control()) {
            return Err(localized_text!("msg_0557").to_owned());
        }
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 64 {
            return Err(localized_text!("msg_0156").to_owned());
        }
        if self.backend != DiskBackend::AimImageFile
            && !(64..=RAMDYN_MAX_SIZE_MIB).contains(&self.size_mib)
        {
            return Err(localized_format!(
                "msg_0547",
                RAMDYN_MAX_SIZE_MIB = RAMDYN_MAX_SIZE_MIB
            ));
        }
        if self.backend == DiskBackend::RamDyn && !(20..=26).contains(&self.block_size_exponent) {
            return Err(localized_text!("msg_0012").to_owned());
        }
        if self.backend != DiskBackend::AimImageFile
            && self.file_system == FileSystem::Fat32
            && self.size_mib > 32 * 1024
        {
            return Err(localized_text!("msg_0062").to_owned());
        }
        match (self.backend, self.image_file.as_deref()) {
            (DiskBackend::AimImageFile, Some(path)) => validate_image_file_syntax(path)?,
            (DiskBackend::AimImageFile, None) => return Err(localized_text!("msg_0589").to_owned()),
            (_, Some(_)) => return Err(localized_text!("msg_0590").to_owned()),
            _ => {}
        }
        if let Some(directory) = self.sync_directory.as_deref() {
            if self.backend == DiskBackend::AimImageFile {
                return Err(localized_text!("msg_0591").to_owned());
            }
            validate_sync_directory_syntax(directory)?;
        }
        if !self.drive_letter.is_ascii_alphabetic() {
            return Err(localized_text!("msg_0023").to_owned());
        }
        let label = self.volume_label.trim();
        if label.encode_utf16().count() > self.file_system.maximum_label_utf16_units() {
            return Err(localized_format!(
                "msg_0234",
                self.file_system.label(),
                self.file_system.maximum_label_utf16_units()
            ));
        }
        if label.chars().any(|c| {
            c.is_control() || matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        }) {
            return Err(localized_text!("msg_0396").to_owned());
        }
        if self.id.trim().is_empty() {
            return Err(localized_text!("msg_0322").to_owned());
        }
        if let Some(directory) = &self.mount_directory {
            validate_mount_directory_syntax(directory)?;
        }
        Ok(())
    }

    pub fn ramdyn_size_kib(&self) -> Result<u64, String> {
        if self.size_mib > RAMDYN_MAX_SIZE_MIB {
            return Err(localized_format!(
                "msg_0227",
                RAMDYN_MAX_SIZE_MIB = RAMDYN_MAX_SIZE_MIB
            ));
        }
        let kib = self
            .size_mib
            .checked_mul(1024)
            .ok_or_else(|| localized_text!("msg_0349").to_owned())?;
        Ok(kib)
    }

    pub fn normalized_drive_letter(&self) -> char {
        self.drive_letter.to_ascii_uppercase()
    }

    pub fn mount_target_key(&self) -> MountTargetKey {
        match self.mount_directory.as_deref() {
            Some(directory) => MountTargetKey::Directory(normalize_mount_directory(directory)),
            None => MountTargetKey::DriveLetter(self.normalized_drive_letter()),
        }
    }

    pub fn mount_target(&self) -> String {
        match self.mount_directory.as_deref() {
            Some(directory) => directory
                .trim_end_matches(|character| character == '\\' || character == '/')
                .to_owned(),
            None => format!("{}:", self.normalized_drive_letter()),
        }
    }

    pub fn mount_target_display(&self) -> String {
        self.mount_target()
    }

    pub fn expected_device_size_bytes(&self) -> Result<u64, String> {
        match self.backend {
            DiskBackend::RamDyn => self
                .ramdyn_size_kib()?
                .checked_mul(1024)
                .and_then(|size| size.checked_add(RAMDYN_MBR_SPACE_BYTES))
                .ok_or_else(|| localized_text!("msg_0349").to_owned()),
            DiskBackend::AimVirtualMemory => self
                .size_mib
                .checked_mul(1024 * 1024)
                .ok_or_else(|| localized_text!("msg_0349").to_owned()),
            DiskBackend::AimAweAlloc => self
                .size_mib
                .checked_mul(1024 * 1024)
                .ok_or_else(|| localized_text!("msg_0349").to_owned()),
            DiskBackend::AimImageFile => self
                .size_mib
                .checked_mul(1024 * 1024)
                .ok_or_else(|| localized_text!("msg_0349").to_owned()),
        }
    }
}

fn validate_image_file_syntax(path: &str) -> Result<(), String> {
    let path = path.trim().replace('/', "\\");
    let bytes = path.as_bytes();
    if bytes.len() < 4
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || bytes[2] != b'\\'
        || path.starts_with("\\\\")
        || path.chars().any(char::is_control)
    {
        return Err(localized_text!("msg_0592").to_owned());
    }
    Ok(())
}

fn validate_sync_directory_syntax(path: &str) -> Result<(), String> {
    validate_mount_directory_syntax(path).map_err(|_| localized_text!("msg_0593").to_owned())
}

fn default_block_size_exponent() -> u8 {
    20
}

fn drive_letter_from_path(path: &str) -> Option<char> {
    let path = path.trim_end_matches(|character| character == '\\' || character == '/');
    let mut chars = path.chars();
    let letter = chars.next()?;
    (letter.is_ascii_alphabetic() && chars.next() == Some(':') && chars.next().is_none())
        .then_some(letter)
}

fn normalize_mount_directory(path: &str) -> String {
    path.trim()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_lowercase()
}

fn validate_mount_directory_syntax(path: &str) -> Result<(), String> {
    let path = path.trim().replace('/', "\\");
    let bytes = path.as_bytes();
    if bytes.len() < 4
        || !bytes[0].is_ascii_alphabetic()
        || bytes[1] != b':'
        || bytes[2] != b'\\'
        || path.starts_with("\\\\")
    {
        return Err(localized_text!("msg_0089").to_owned());
    }
    let components = path[3..]
        .trim_end_matches('\\')
        .split('\\')
        .collect::<Vec<_>>();
    if components.is_empty()
        || components.iter().any(|component| {
            component.is_empty()
                || *component == "."
                || *component == ".."
                || component.ends_with('.')
                || component.ends_with(' ')
                || component.chars().any(|character| {
                    character.is_control()
                        || matches!(character, ':' | '*' | '?' | '"' | '<' | '>' | '|')
                })
        })
    {
        return Err(localized_text!("msg_0369").to_owned());
    }
    if path.chars().any(char::is_control) {
        return Err(localized_text!("msg_0344").to_owned());
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProfileDocument {
    schema_version: u32,
    profiles: Vec<DiskProfile>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyDiskProfile {
    id: String,
    name: String,
    size_mib: u64,
    drive_letter: char,
    volume_label: String,
    memory_mode: MemoryMode,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyProfileDocument {
    #[serde(rename = "schema_version")]
    _schema_version: u32,
    profiles: Vec<LegacyDiskProfile>,
}

pub struct ProfileStore {
    path: PathBuf,
    profiles: Vec<DiskProfile>,
    load_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PendingCreate {
    pub profile: DiskProfile,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PendingCreateDocument {
    schema_version: u32,
    pending_creates: Vec<PendingCreate>,
}

pub struct PendingCreateStore {
    path: PathBuf,
    pending_creates: Vec<PendingCreate>,
    load_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ActiveDiskMode {
    volume_guid: String,
    size_bytes: u64,
    #[serde(default)]
    memory_mode: Option<MemoryMode>,
    #[serde(default)]
    sync_directory: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ActiveDiskModeDocument {
    schema_version: u32,
    active_disks: Vec<ActiveDiskMode>,
}

pub struct ActiveDiskModeStore {
    path: PathBuf,
    active_disks: Vec<ActiveDiskMode>,
    load_error: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StartupRunSummary {
    pub results: Vec<String>,
    pub has_errors: bool,
    #[serde(default = "default_startup_result_language")]
    pub language: crate::i18n::Language,
}

fn default_startup_result_language() -> crate::i18n::Language {
    // Existing settings files only stored Chinese startup-result text.
    crate::i18n::Language::Chinese
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ManagerSettingsDocument {
    schema_version: u32,
    close_after_startup: bool,
    last_startup_run: Option<StartupRunSummary>,
    #[serde(default)]
    language_override: Option<Language>,
}

pub struct ManagerSettingsStore {
    path: PathBuf,
    close_after_startup: bool,
    last_startup_run: Option<StartupRunSummary>,
    language_override: Option<Language>,
    load_error: Option<String>,
}

impl ManagerSettingsStore {
    pub fn load() -> Self {
        let path = match application_data_path("manager-settings.json") {
            Ok(path) => path,
            Err(error) => {
                return Self {
                    path: PathBuf::new(),
                    close_after_startup: false,
                    last_startup_run: None,
                    language_override: None,
                    load_error: Some(error),
                };
            }
        };
        match Self::read(&path) {
            Ok((close_after_startup, last_startup_run, language_override)) => Self {
                path,
                close_after_startup,
                last_startup_run,
                language_override,
                load_error: None,
            },
            Err(error) => Self {
                path,
                close_after_startup: false,
                last_startup_run: None,
                language_override: None,
                load_error: Some(error),
            },
        }
    }

    pub fn close_after_startup(&self) -> bool {
        self.close_after_startup
    }

    pub fn last_startup_run(&self) -> Option<&StartupRunSummary> {
        self.last_startup_run.as_ref()
    }

    pub fn language_override(&self) -> Option<Language> {
        self.language_override
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn set_language_override(
        &mut self,
        language_override: Option<Language>,
    ) -> Result<(), String> {
        self.write_candidate(
            self.close_after_startup,
            self.last_startup_run.clone(),
            language_override,
        )
    }

    pub fn set_close_after_startup(&mut self, enabled: bool) -> Result<(), String> {
        let last_startup_run = self.last_startup_run.clone();
        self.write_candidate(enabled, last_startup_run, self.language_override)
    }

    pub fn record_startup_run(
        &mut self,
        results: Vec<String>,
        has_errors: bool,
        language: crate::i18n::Language,
    ) -> Result<(), String> {
        let close_after_startup = self.close_after_startup;
        let last_startup_run = Some(StartupRunSummary {
            results,
            has_errors,
            language,
        });
        self.write_candidate(
            close_after_startup,
            last_startup_run,
            self.language_override,
        )
    }

    fn write_candidate(
        &mut self,
        close_after_startup: bool,
        last_startup_run: Option<StartupRunSummary>,
        language_override: Option<Language>,
    ) -> Result<(), String> {
        if let Some(error) = &self.load_error {
            return Err(localized_format!("msg_0300", error = error));
        }
        let document = ManagerSettingsDocument {
            schema_version: MANAGER_SETTINGS_SCHEMA_VERSION,
            close_after_startup,
            last_startup_run: last_startup_run.clone(),
            language_override,
        };
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|error| localized_format!("msg_0329", error = error))?;
        write_atomically(&self.path, &bytes)
            .map_err(|error| localized_format!("msg_0099", error = error))?;
        self.close_after_startup = close_after_startup;
        self.last_startup_run = last_startup_run;
        self.language_override = language_override;
        Ok(())
    }

    fn read(path: &Path) -> Result<(bool, Option<StartupRunSummary>, Option<Language>), String> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok((false, None, None))
            }
            Err(error) => return Err(localized_format!("msg_0377", path.display(), error = error)),
        };
        let document: ManagerSettingsDocument = serde_json::from_slice(&bytes)
            .map_err(|error| localized_format!("msg_0403", error = error))?;
        if !matches!(document.schema_version, 1..=MANAGER_SETTINGS_SCHEMA_VERSION) {
            return Err(localized_format!("msg_0457", document.schema_version));
        }
        Ok((
            document.close_after_startup,
            document.last_startup_run,
            document.language_override,
        ))
    }
}
impl ProfileStore {
    pub fn load() -> Self {
        let path = match configuration_path() {
            Ok(path) => path,
            Err(error) => {
                return Self {
                    path: PathBuf::new(),
                    profiles: Vec::new(),
                    load_error: Some(error),
                };
            }
        };
        match Self::read_profiles(&path) {
            Ok(profiles) => Self {
                path,
                profiles,
                load_error: None,
            },
            Err(error) => Self {
                path,
                profiles: Vec::new(),
                load_error: Some(error),
            },
        }
    }

    pub fn profiles(&self) -> &[DiskProfile] {
        &self.profiles
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn upsert(&mut self, profile: DiskProfile) -> Result<(), String> {
        self.ensure_writable()?;
        profile.validate()?;
        let mut candidate = self.profiles.clone();
        if let Some(existing) = candidate.iter_mut().find(|entry| entry.id == profile.id) {
            *existing = profile;
        } else {
            candidate.push(profile);
        }
        self.write_candidate(candidate)
    }

    pub fn delete(&mut self, id: &str) -> Result<(), String> {
        self.ensure_writable()?;
        let mut candidate = self.profiles.clone();
        candidate.retain(|entry| entry.id != id);
        self.write_candidate(candidate)
    }

    fn ensure_writable(&self) -> Result<(), String> {
        if let Some(error) = &self.load_error {
            return Err(localized_format!("msg_0043", error = error));
        }
        Ok(())
    }

    fn write_candidate(&mut self, profiles: Vec<DiskProfile>) -> Result<(), String> {
        validate_profiles(&profiles)?;
        let document = ProfileDocument {
            schema_version: SCHEMA_VERSION,
            profiles: profiles.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|error| localized_format!("msg_0235", error = error))?;
        write_atomically(&self.path, &bytes)
            .map_err(|error| localized_format!("msg_0115", error = error))?;
        self.profiles = profiles;
        Ok(())
    }

    fn read_profiles(path: &Path) -> Result<Vec<DiskProfile>, String> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(localized_format!("msg_0377", path.display(), error = error)),
        };
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| localized_format!("msg_0400", error = error))?;
        let schema_version = value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| localized_text!("msg_0040").to_owned())?
            as u32;
        let profiles = match schema_version {
            1 => {
                let document: LegacyProfileDocument = serde_json::from_value(value)
                    .map_err(|error| localized_format!("msg_0295", error = error))?;
                document
                    .profiles
                    .into_iter()
                    .map(|profile| DiskProfile {
                        id: profile.id,
                        name: profile.name,
                        size_mib: profile.size_mib,
                        drive_letter: profile.drive_letter,
                        volume_label: profile.volume_label,
                        memory_mode: profile.memory_mode,
                        backend: DiskBackend::RamDyn,
                        adapter_port: None,
                        file_system: FileSystem::Ntfs,
                        block_size_exponent: default_block_size_exponent(),
                        removable_media: false,
                        mount_directory: None,
                        auto_mount: false,
                        sync_directory: None,
                        image_file: None,
                    })
                    .collect::<Vec<_>>()
            }
            2..=SCHEMA_VERSION => {
                let document: ProfileDocument = serde_json::from_value(value)
                    .map_err(|error| localized_format!("msg_0400", error = error))?;
                document.profiles
            }
            _ => {
                return Err(localized_format!(
                    "msg_0373",
                    schema_version = schema_version
                ));
            }
        };
        validate_profiles(&profiles)
            .map_err(|error| localized_format!("msg_0459", error = error))?;
        Ok(profiles)
    }
}

impl PendingCreateStore {
    pub fn load() -> Self {
        let path = match application_data_path("pending-creates.json") {
            Ok(path) => path,
            Err(error) => {
                return Self {
                    path: PathBuf::new(),
                    pending_creates: Vec::new(),
                    load_error: Some(error),
                };
            }
        };
        match Self::read_pending_creates(&path) {
            Ok(pending_creates) => Self {
                path,
                pending_creates,
                load_error: None,
            },
            Err(error) => Self {
                path,
                pending_creates: Vec::new(),
                load_error: Some(error),
            },
        }
    }

    pub fn pending_creates(&self) -> &[PendingCreate] {
        &self.pending_creates
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_error(&self) -> Option<&str> {
        self.load_error.as_deref()
    }

    pub fn begin(&mut self, profile: DiskProfile) -> Result<(), String> {
        self.ensure_writable()?;
        profile.validate()?;
        let target = profile.mount_target_key();
        if self
            .pending_creates
            .iter()
            .any(|entry| entry.profile.mount_target_key() == target)
        {
            return Err(localized_format!(
                "msg_0186",
                profile.mount_target_display()
            ));
        }

        let mut candidate = self.pending_creates.clone();
        candidate.push(PendingCreate { profile });
        self.write_candidate(candidate)
    }

    pub fn resolve(&mut self, target: &MountTargetKey) -> Result<bool, String> {
        self.ensure_writable()?;
        if !self
            .pending_creates
            .iter()
            .any(|entry| &entry.profile.mount_target_key() == target)
        {
            return Ok(false);
        }
        let mut candidate = self.pending_creates.clone();
        candidate.retain(|entry| &entry.profile.mount_target_key() != target);
        self.write_candidate(candidate)?;
        Ok(true)
    }

    fn ensure_writable(&self) -> Result<(), String> {
        if let Some(error) = &self.load_error {
            return Err(localized_format!("msg_0558", error = error));
        }
        Ok(())
    }

    fn write_candidate(&mut self, pending_creates: Vec<PendingCreate>) -> Result<(), String> {
        validate_pending_creates(&pending_creates)?;
        let document = PendingCreateDocument {
            schema_version: PENDING_CREATE_SCHEMA_VERSION,
            pending_creates: pending_creates.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|error| localized_format!("msg_0077", error = error))?;
        write_atomically(&self.path, &bytes)
            .map_err(|error| localized_format!("msg_0211", error = error))?;
        self.pending_creates = pending_creates;
        Ok(())
    }

    fn read_pending_creates(path: &Path) -> Result<Vec<PendingCreate>, String> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(localized_format!("msg_0377", path.display(), error = error)),
        };
        let document: PendingCreateDocument = serde_json::from_slice(&bytes)
            .map_err(|error| localized_format!("msg_0213", path.display(), error = error))?;
        if document.schema_version != PENDING_CREATE_SCHEMA_VERSION {
            return Err(localized_format!(
                "msg_0525",
                path.display(),
                document.schema_version,
            ));
        }
        validate_pending_creates(&document.pending_creates)
            .map_err(|error| localized_format!("msg_0493", path.display(), error = error))?;
        Ok(document.pending_creates)
    }
}

impl ActiveDiskModeStore {
    pub fn load() -> Self {
        let path = match application_data_path("active-disk-modes.json") {
            Ok(path) => path,
            Err(error) => {
                return Self {
                    path: PathBuf::new(),
                    active_disks: Vec::new(),
                    load_error: Some(error),
                };
            }
        };
        match Self::read_active_disks(&path) {
            Ok(active_disks) => Self {
                path,
                active_disks,
                load_error: None,
            },
            Err(error) => Self {
                path,
                active_disks: Vec::new(),
                load_error: Some(error),
            },
        }
    }

    pub fn contains_volume_guid(&self, volume_guid: &str) -> bool {
        let key = normalize_volume_guid(volume_guid);
        !key.is_empty()
            && self
                .active_disks
                .iter()
                .any(|entry| normalize_volume_guid(&entry.volume_guid) == key)
    }

    pub fn mode_for(&self, volume_guid: &str, size_bytes: u64) -> Option<MemoryMode> {
        let key = normalize_volume_guid(volume_guid);
        self.active_disks
            .iter()
            .find(|entry| {
                normalize_volume_guid(&entry.volume_guid) == key && entry.size_bytes == size_bytes
            })
            .and_then(|entry| entry.memory_mode)
    }

    pub fn remember(
        &mut self,
        volume_guid: &str,
        size_bytes: u64,
        memory_mode: MemoryMode,
    ) -> Result<(), String> {
        self.ensure_writable()?;
        let volume_guid = volume_guid.trim();
        if volume_guid.is_empty() {
            return Err(localized_text!("msg_0511").to_owned());
        }
        if size_bytes == 0 {
            return Err(localized_text!("msg_0440").to_owned());
        }

        let key = normalize_volume_guid(volume_guid);
        let mut candidate = self.active_disks.clone();
        if let Some(existing) = candidate
            .iter_mut()
            .find(|entry| normalize_volume_guid(&entry.volume_guid) == key)
        {
            if existing.size_bytes == size_bytes && existing.memory_mode == Some(memory_mode) {
                return Ok(());
            }
            let sync_directory = existing.sync_directory.clone();
            *existing = ActiveDiskMode {
                volume_guid: volume_guid.to_owned(),
                size_bytes,
                memory_mode: Some(memory_mode),
                sync_directory,
            };
        } else {
            candidate.push(ActiveDiskMode {
                volume_guid: volume_guid.to_owned(),
                size_bytes,
                memory_mode: Some(memory_mode),
                sync_directory: None,
            });
        }
        self.write_candidate(candidate)
    }

    pub fn remember_managed_disk(
        &mut self,
        volume_guid: &str,
        size_bytes: u64,
        memory_mode: Option<MemoryMode>,
        sync_directory: Option<&str>,
    ) -> Result<(), String> {
        self.ensure_writable()?;
        let volume_guid = volume_guid.trim();
        if volume_guid.is_empty() {
            return Err(localized_text!("msg_0511").to_owned());
        }
        if size_bytes == 0 {
            return Err(localized_text!("msg_0440").to_owned());
        }
        if memory_mode.is_none() && sync_directory.is_none() {
            return Ok(());
        }

        let key = normalize_volume_guid(volume_guid);
        let mut candidate = self.active_disks.clone();
        if let Some(existing) = candidate
            .iter_mut()
            .find(|entry| normalize_volume_guid(&entry.volume_guid) == key)
        {
            if existing.size_bytes == size_bytes
                && existing.memory_mode == memory_mode
                && existing.sync_directory.as_deref() == sync_directory
            {
                return Ok(());
            }
            *existing = ActiveDiskMode {
                volume_guid: volume_guid.to_owned(),
                size_bytes,
                memory_mode,
                sync_directory: sync_directory.map(str::to_owned),
            };
        } else {
            candidate.push(ActiveDiskMode {
                volume_guid: volume_guid.to_owned(),
                size_bytes,
                memory_mode,
                sync_directory: sync_directory.map(str::to_owned),
            });
        }
        self.write_candidate(candidate)
    }

    pub fn sync_directory_for(&self, volume_guid: &str, size_bytes: u64) -> Option<&str> {
        let key = normalize_volume_guid(volume_guid);
        self.active_disks
            .iter()
            .find(|entry| {
                normalize_volume_guid(&entry.volume_guid) == key && entry.size_bytes == size_bytes
            })
            .and_then(|entry| entry.sync_directory.as_deref())
    }

    pub fn remove(&mut self, volume_guid: &str) -> Result<(), String> {
        self.ensure_writable()?;
        let key = normalize_volume_guid(volume_guid);
        let mut candidate = self.active_disks.clone();
        candidate.retain(|entry| normalize_volume_guid(&entry.volume_guid) != key);
        if candidate.len() == self.active_disks.len() {
            return Ok(());
        }
        self.write_candidate(candidate)
    }

    pub fn retain_live_volumes(&mut self, live_volumes: &[(String, u64)]) -> Result<(), String> {
        self.ensure_writable()?;
        let live = live_volumes
            .iter()
            .map(|(volume_guid, size_bytes)| (normalize_volume_guid(volume_guid), *size_bytes))
            .collect::<HashSet<_>>();
        let mut candidate = self.active_disks.clone();
        candidate.retain(|entry| {
            live.contains(&(normalize_volume_guid(&entry.volume_guid), entry.size_bytes))
        });
        if candidate.len() == self.active_disks.len() {
            return Ok(());
        }
        self.write_candidate(candidate)
    }

    fn ensure_writable(&self) -> Result<(), String> {
        if let Some(error) = &self.load_error {
            return Err(localized_format!("msg_0479", error = error));
        }
        Ok(())
    }

    fn write_candidate(&mut self, active_disks: Vec<ActiveDiskMode>) -> Result<(), String> {
        validate_active_disk_modes(&active_disks)?;
        let document = ActiveDiskModeDocument {
            schema_version: ACTIVE_DISK_MODE_SCHEMA_VERSION,
            active_disks: active_disks.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&document)
            .map_err(|error| localized_format!("msg_0486", error = error))?;
        write_atomically(&self.path, &bytes)
            .map_err(|error| localized_format!("msg_0498", error = error))?;
        self.active_disks = active_disks;
        Ok(())
    }

    fn read_active_disks(path: &Path) -> Result<Vec<ActiveDiskMode>, String> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(localized_format!("msg_0377", path.display(), error = error)),
        };
        let document: ActiveDiskModeDocument = serde_json::from_slice(&bytes)
            .map_err(|error| localized_format!("msg_0354", path.display(), error = error))?;
        if !(1..=ACTIVE_DISK_MODE_SCHEMA_VERSION).contains(&document.schema_version) {
            return Err(localized_format!(
                "msg_0468",
                path.display(),
                document.schema_version,
            ));
        }
        validate_active_disk_modes(&document.active_disks)
            .map_err(|error| localized_format!("msg_0203", path.display(), error = error))?;
        Ok(document.active_disks)
    }
}

pub fn new_profile_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let sequence = NEXT_PROFILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{:x}-{:x}-{:x}", std::process::id(), nanos, sequence)
}

fn validate_profiles(profiles: &[DiskProfile]) -> Result<(), String> {
    let mut ids = HashSet::with_capacity(profiles.len());
    for profile in profiles {
        profile.validate()?;
        if !ids.insert(profile.id.as_str()) {
            return Err(localized_format!("msg_0185", profile.id));
        }
    }
    Ok(())
}

fn validate_pending_creates(pending_creates: &[PendingCreate]) -> Result<(), String> {
    let mut mount_targets = HashSet::with_capacity(pending_creates.len());
    for pending in pending_creates {
        pending.profile.validate()?;
        let target = pending.profile.mount_target_key();
        if !mount_targets.insert(target.clone()) {
            return Err(localized_format!("msg_0106", target.label()));
        }
    }
    Ok(())
}

fn validate_active_disk_modes(active_disks: &[ActiveDiskMode]) -> Result<(), String> {
    let mut volume_guids = HashSet::with_capacity(active_disks.len());
    for entry in active_disks {
        let key = normalize_volume_guid(&entry.volume_guid);
        if key.is_empty() {
            return Err(localized_text!("msg_0084").to_owned());
        }
        if entry.size_bytes == 0 {
            return Err(localized_format!("msg_0527", entry.volume_guid));
        }
        if let Some(directory) = entry.sync_directory.as_deref() {
            validate_sync_directory_syntax(directory)?;
        }
        if !volume_guids.insert(key) {
            return Err(localized_format!("msg_0473", entry.volume_guid));
        }
    }
    Ok(())
}

fn normalize_volume_guid(volume_guid: &str) -> String {
    volume_guid.trim().to_ascii_lowercase()
}

fn configuration_path() -> Result<PathBuf, String> {
    application_data_path("profiles.json")
}

fn application_data_path(file_name: &str) -> Result<PathBuf, String> {
    let executable =
        std::env::current_exe().map_err(|error| localized_format!("msg_0559", error = error))?;
    let application_directory = executable
        .parent()
        .ok_or_else(|| localized_text!("msg_0196").to_owned())?;

    Ok(application_directory.join("data").join(file_name))
}

fn write_atomically(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, localized_text!("msg_0520")))?;
    fs::create_dir_all(parent)?;
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = path.with_extension(format!("tmp-{}-{suffix:x}", std::process::id()));
    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        replace_file(&temporary, path)
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

#[cfg(windows)]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };

    fn wide(path: &Path) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;

        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    let source = wide(source);
    let destination = wide(destination);
    let ok = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if ok == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(windows))]
fn replace_file(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

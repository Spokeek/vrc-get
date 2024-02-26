use reqwest::Url;
use std::io;
use std::num::Wrapping;
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};

use serde::Serialize;
use specta::specta;
use tauri::async_runtime::Mutex;
use tauri::{generate_handler, Invoke, Runtime, State};

use vrc_get_vpm::environment::UserProject;
use vrc_get_vpm::unity_project::pending_project_changes::{
    ConflictInfo, PackageChange, RemoveReason,
};
use vrc_get_vpm::unity_project::{AddPackageOperation, PendingProjectChanges};
use vrc_get_vpm::version::Version;
use vrc_get_vpm::{PackageCollection, PackageInfo, PackageJson, ProjectType};

pub(crate) fn handlers<R: Runtime>() -> impl Fn(Invoke<R>) + Send + Sync + 'static {
    generate_handler![
        environment_projects,
        environment_packages,
        environment_repositories_info,
        environment_hide_repository,
        environment_show_repository,
        environment_set_hide_local_user_packages,
        project_details,
        project_install_package,
        project_apply_pending_changes,
    ]
}

#[cfg(debug_assertions)]
pub(crate) fn export_ts() {
    tauri_specta::ts::export_with_cfg(
        specta::collect_types![
            environment_projects,
            environment_packages,
            environment_repositories_info,
            environment_hide_repository,
            environment_show_repository,
            environment_set_hide_local_user_packages,
            project_details,
            project_install_package,
            project_apply_pending_changes,
        ]
        .unwrap(),
        specta::ts::ExportConfiguration::new().bigint(specta::ts::BigIntExportBehavior::Number),
        "web/lib/bindings.ts",
    )
    .unwrap();
}

pub(crate) fn new_env_state() -> impl Send + Sync + 'static {
    Mutex::new(EnvironmentState::new())
}

type Environment = vrc_get_vpm::Environment<reqwest::Client, vrc_get_vpm::io::DefaultEnvironmentIo>;
type UnityProject = vrc_get_vpm::UnityProject<vrc_get_vpm::io::DefaultProjectIo>;

async fn new_environment() -> io::Result<Environment> {
    let client = reqwest::Client::new();
    let io = vrc_get_vpm::io::DefaultEnvironmentIo::new_default();
    Environment::load(Some(client), io).await
}

#[derive(Debug, Clone, Serialize, specta::Type)]
#[specta(export)]
enum RustError {
    Unrecoverable(String),
}

impl From<io::Error> for RustError {
    fn from(value: io::Error) -> Self {
        RustError::Unrecoverable(format!("io error: {value}"))
    }
}

unsafe impl Send for EnvironmentState {}

unsafe impl Sync for EnvironmentState {}

struct EnvironmentState {
    environment: EnvironmentHolder,
    packages: Option<NonNull<[PackageInfo<'static>]>>,
    // null or reference to
    projects: Box<[UserProject]>,
    projects_version: Wrapping<u32>,
    changes_info: Option<NonNull<PendingProjectChangesInfo<'static>>>,
}

static CHANGES_GLOBAL_INDEXER: AtomicU32 = AtomicU32::new(0);

struct PendingProjectChangesInfo<'env> {
    environment_version: u32,
    changes_version: u32,
    changes: PendingProjectChanges<'env>,
}

struct EnvironmentHolder {
    environment: Option<Environment>,
    environment_version: Wrapping<u32>,
}

impl EnvironmentHolder {
    fn new() -> Self {
        Self {
            environment: None,
            environment_version: Wrapping(0),
        }
    }

    async fn get_environment_mut(&mut self, inc_version: bool) -> io::Result<&mut Environment> {
        if let Some(ref mut environment) = self.environment {
            println!("reloading settings files");
            // reload settings files
            environment.reload().await?;
            if inc_version {
                self.environment_version += Wrapping(1);
            }
            Ok(environment)
        } else {
            self.environment = Some(new_environment().await?);
            self.environment_version += Wrapping(1);
            Ok(self.environment.as_mut().unwrap())
        }
    }
}

impl EnvironmentState {
    fn new() -> Self {
        Self {
            environment: EnvironmentHolder::new(),
            packages: None,
            projects: Box::new([]),
            projects_version: Wrapping(0),
            changes_info: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, specta::Type)]
struct TauriProject {
    // the project identifier
    list_version: u32,
    index: usize,

    // projet information
    name: String,
    path: String,
    project_type: TauriProjectType,
    unity: String,
    last_modified: u64,
    created_at: u64,
}

#[derive(Debug, Clone, Serialize, specta::Type)]
enum TauriProjectType {
    Unknown,
    LegacySdk2,
    LegacyWorlds,
    LegacyAvatars,
    UpmWorlds,
    UpmAvatars,
    UpmStarter,
    Worlds,
    Avatars,
    VpmStarter,
}

impl From<ProjectType> for TauriProjectType {
    fn from(value: ProjectType) -> Self {
        match value {
            ProjectType::Unknown => Self::Unknown,
            ProjectType::LegacySdk2 => Self::LegacySdk2,
            ProjectType::LegacyWorlds => Self::LegacyWorlds,
            ProjectType::LegacyAvatars => Self::LegacyAvatars,
            ProjectType::UpmWorlds => Self::UpmWorlds,
            ProjectType::UpmAvatars => Self::UpmAvatars,
            ProjectType::UpmStarter => Self::UpmStarter,
            ProjectType::Worlds => Self::Worlds,
            ProjectType::Avatars => Self::Avatars,
            ProjectType::VpmStarter => Self::VpmStarter,
        }
    }
}

impl TauriProject {
    fn new(list_version: u32, index: usize, project: &UserProject) -> Self {
        Self {
            list_version,
            index,

            name: project.name().to_string(),
            path: project.path().to_string(),
            project_type: project.project_type().into(),
            unity: project
                .unity_version()
                .map(|v| v.to_string())
                .unwrap_or_else(|| "unknown".into()),
            last_modified: project.last_modified().as_millis_since_epoch(),
            created_at: project.crated_at().as_millis_since_epoch(),
        }
    }
}

#[tauri::command]
#[specta::specta]
async fn environment_projects(
    state: State<'_, Mutex<EnvironmentState>>,
) -> Result<Vec<TauriProject>, RustError> {
    let mut state = state.lock().await;
    let environment = state.environment.get_environment_mut(false).await?;

    println!("migrating projects from settings.json");
    // migrate from settings json
    environment.migrate_from_settings_json().await?;
    environment.save().await?;

    println!("fetching projects");

    state.projects = environment.get_projects()?.into_boxed_slice();
    state.projects_version += Wrapping(1);

    let version = (state.environment.environment_version + state.projects_version).0;

    let vec = state
        .projects
        .iter()
        .enumerate()
        .map(|(index, value)| TauriProject::new(version, index, value))
        .collect::<Vec<_>>();

    Ok(vec)
}

#[derive(Serialize, specta::Type)]
struct TauriVersion {
    major: u64,
    minor: u64,
    patch: u64,
    pre: String,
    build: String,
}

impl From<&Version> for TauriVersion {
    fn from(value: &Version) -> Self {
        Self {
            major: value.major,
            minor: value.minor,
            patch: value.patch,
            pre: value.pre.as_str().to_string(),
            build: value.build.as_str().to_string(),
        }
    }
}

#[derive(Serialize, specta::Type)]
struct TauriBasePackageInfo {
    name: String,
    display_name: Option<String>,
    version: TauriVersion,
    unity: Option<(u16, u8)>,
    is_yanked: bool,
}

impl TauriBasePackageInfo {
    fn new(package: &PackageJson) -> Self {
        Self {
            name: package.name().to_string(),
            display_name: package.display_name().map(|v| v.to_string()),
            version: package.version().into(),
            unity: package.unity().map(|v| (v.major(), v.minor())),
            is_yanked: package.is_yanked(),
        }
    }
}

#[derive(Serialize, specta::Type)]
struct TauriPackage {
    env_version: u32,
    index: usize,

    #[serde(flatten)]
    base: TauriBasePackageInfo,

    source: TauriPackageSource,
}

#[derive(Serialize, specta::Type)]
enum TauriPackageSource {
    LocalUser,
    Remote { id: String, display_name: String },
}

impl TauriPackage {
    fn new(env_version: u32, index: usize, package: &PackageInfo) -> Self {
        let source = if let Some(repo) = package.repo() {
            let id = repo.id().or(repo.url().map(|x| x.as_str())).unwrap();
            TauriPackageSource::Remote {
                id: id.to_string(),
                display_name: repo.name().unwrap_or(id).to_string(),
            }
        } else {
            TauriPackageSource::LocalUser
        };

        Self {
            env_version,
            index,
            base: TauriBasePackageInfo::new(package.package_json()),
            source,
        }
    }
}

#[tauri::command]
#[specta::specta]
async fn environment_packages(
    state: State<'_, Mutex<EnvironmentState>>,
) -> Result<Vec<TauriPackage>, RustError> {
    let mut env_state = state.lock().await;
    let env_state = &mut *env_state;
    let environment = env_state.environment.get_environment_mut(true).await?;

    println!("loading package infos");
    environment.load_package_infos(true).await?;

    let packages = environment
        .get_all_packages()
        .collect::<Vec<_>>()
        .into_boxed_slice();
    if let Some(ptr) = env_state.packages {
        env_state.packages = None; // avoid a double drop
        unsafe { drop(Box::from_raw(ptr.as_ptr())) }
    }
    env_state.packages = NonNull::new(Box::into_raw(packages) as *mut _);
    let packages = unsafe { &*env_state.packages.unwrap().as_ptr() };
    let version = env_state.environment.environment_version.0;

    Ok(packages
        .iter()
        .enumerate()
        .map(|(index, value)| TauriPackage::new(version, index, value))
        .collect::<Vec<_>>())
}

#[derive(Serialize, specta::Type)]
struct TauriUserRepository {
    id: String,
    display_name: String,
}

#[derive(Serialize, specta::Type)]
struct TauriRepositoriesInfo {
    user_repositories: Vec<TauriUserRepository>,
    hidden_user_repositories: Vec<String>,
    hide_local_user_packages: bool,
}

#[tauri::command]
#[specta::specta]
async fn environment_repositories_info(
    state: State<'_, Mutex<EnvironmentState>>,
) -> Result<TauriRepositoriesInfo, RustError> {
    let mut env_state = state.lock().await;
    let env_state = &mut *env_state;
    let environment = env_state.environment.get_environment_mut(false).await?;

    Ok(TauriRepositoriesInfo {
        user_repositories: environment
            .get_user_repos()
            .iter()
            .map(|x| {
                let id = x.id().or(x.url().map(Url::as_str)).unwrap();
                TauriUserRepository {
                    id: id.to_string(),
                    display_name: x.name().unwrap_or(id).to_string(),
                }
            })
            .collect(),
        hidden_user_repositories: environment
            .gui_hidden_repositories()
            .map(Into::into)
            .collect(),
        hide_local_user_packages: environment.hide_local_user_packages(),
    })
}

#[tauri::command]
#[specta::specta]
async fn environment_hide_repository(
    state: State<'_, Mutex<EnvironmentState>>,
    repository: String,
) -> Result<(), RustError> {
    let mut env_state = state.lock().await;
    let env_state = &mut *env_state;
    let environment = env_state.environment.get_environment_mut(false).await?;
    environment.add_gui_hidden_repositories(repository);
    environment.save().await?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
async fn environment_show_repository(
    state: State<'_, Mutex<EnvironmentState>>,
    repository: String,
) -> Result<(), RustError> {
    let mut env_state = state.lock().await;
    let env_state = &mut *env_state;
    let environment = env_state.environment.get_environment_mut(false).await?;
    environment.remove_gui_hidden_repositories(&repository);
    environment.save().await?;

    Ok(())
}

#[tauri::command]
#[specta::specta]
async fn environment_set_hide_local_user_packages(
    state: State<'_, Mutex<EnvironmentState>>,
    value: bool,
) -> Result<(), RustError> {
    let mut env_state = state.lock().await;
    let env_state = &mut *env_state;
    let environment = env_state.environment.get_environment_mut(false).await?;
    environment.set_hide_local_user_packages(value);
    environment.save().await?;

    Ok(())
}

#[derive(Serialize, specta::Type)]
struct TauriProjectDetails {
    unity: Option<(u16, u8)>,
    unity_str: String,
    installed_packages: Vec<(String, TauriBasePackageInfo)>,
}

async fn load_project(project_path: String) -> Result<UnityProject, RustError> {
    Ok(UnityProject::load(vrc_get_vpm::io::DefaultProjectIo::new(
        PathBuf::from(project_path).into(),
    ))
    .await?)
}

#[tauri::command]
#[specta::specta]
async fn project_details(project_path: String) -> Result<TauriProjectDetails, RustError> {
    let unity_project = load_project(project_path).await?;

    Ok(TauriProjectDetails {
        unity: unity_project
            .unity_version()
            .map(|v| (v.major(), v.minor())),
        unity_str: unity_project
            .unity_version()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "unknown".into()),
        installed_packages: unity_project
            .installed_packages()
            .map(|(k, p)| (k.to_string(), TauriBasePackageInfo::new(p)))
            .collect(),
    })
}

#[derive(Serialize, specta::Type)]
struct TauriPendingProjectChanges {
    changes_version: u32,
    package_changes: Vec<(String, TauriPackageChange)>,

    remove_legacy_files: Vec<String>,
    remove_legacy_folders: Vec<String>,

    conflicts: Vec<(String, TauriConflictInfo)>,
}

impl TauriPendingProjectChanges {
    fn new(version: u32, changes: &PendingProjectChanges) -> Self {
        TauriPendingProjectChanges {
            changes_version: version,
            package_changes: changes
                .package_changes()
                .iter()
                .filter_map(|(name, change)| Some((name.to_string(), change.try_into().ok()?)))
                .collect(),
            remove_legacy_files: changes
                .remove_legacy_files()
                .iter()
                .map(|x| x.to_string_lossy().into_owned())
                .collect(),
            remove_legacy_folders: changes
                .remove_legacy_folders()
                .iter()
                .map(|x| x.to_string_lossy().into_owned())
                .collect(),
            conflicts: changes
                .conflicts()
                .iter()
                .map(|(name, info)| (name.to_string(), info.into()))
                .collect(),
        }
    }
}

#[derive(Serialize, specta::Type)]
enum TauriPackageChange {
    InstallNew(TauriBasePackageInfo),
    Remove(TauriRemoveReason),
}

impl TryFrom<&PackageChange<'_>> for TauriPackageChange {
    type Error = ();

    fn try_from(value: &PackageChange) -> Result<Self, ()> {
        Ok(match value {
            PackageChange::Install(install) => TauriPackageChange::InstallNew(
                TauriBasePackageInfo::new(install.install_package().ok_or(())?.package_json()),
            ),
            PackageChange::Remove(remove) => TauriPackageChange::Remove(remove.reason().into()),
        })
    }
}

#[derive(Serialize, specta::Type)]
enum TauriRemoveReason {
    Requested,
    Legacy,
    Unused,
}

impl From<RemoveReason> for TauriRemoveReason {
    fn from(value: RemoveReason) -> Self {
        match value {
            RemoveReason::Requested => Self::Requested,
            RemoveReason::Legacy => Self::Legacy,
            RemoveReason::Unused => Self::Unused,
        }
    }
}

#[derive(Serialize, specta::Type)]
struct TauriConflictInfo {
    packages: Vec<String>,
    unity_conflict: bool,
}

impl From<&ConflictInfo> for TauriConflictInfo {
    fn from(value: &ConflictInfo) -> Self {
        Self {
            packages: value
                .conflicting_packages()
                .iter()
                .map(|x| x.to_string())
                .collect(),
            unity_conflict: value.conflicts_with_unity(),
        }
    }
}

#[tauri::command]
#[specta::specta]
async fn project_install_package(
    state: State<'_, Mutex<EnvironmentState>>,
    project_path: String,
    env_version: u32,
    package_index: usize,
) -> Result<TauriPendingProjectChanges, RustError> {
    let mut env_state = state.lock().await;
    let env_state = &mut *env_state;
    if env_state.environment.environment_version != Wrapping(env_version) {
        return Err(RustError::Unrecoverable(
            "environment version mismatch".into(),
        ));
    }

    let environment = env_state.environment.get_environment_mut(false).await?;
    let packages = unsafe { &*env_state.packages.unwrap().as_mut() };
    let installing_package = packages[package_index];

    let unity_project = load_project(project_path).await?;

    let operation = if let Some(locked) = unity_project.get_locked(installing_package.name()) {
        if installing_package.version() < locked.version() {
            AddPackageOperation::Downgrade
        } else {
            AddPackageOperation::UpgradeLocked
        }
    } else {
        AddPackageOperation::InstallToDependencies
    };

    let changes = match unity_project
        .add_package_request(environment, vec![installing_package], operation, false)
        .await
    {
        Ok(request) => request,
        Err(e) => return Err(RustError::Unrecoverable(format!("{e}"))),
    };

    let new_version = CHANGES_GLOBAL_INDEXER.fetch_add(1, Ordering::SeqCst);

    let result = TauriPendingProjectChanges::new(new_version, &changes);

    let changes_info = Box::new(PendingProjectChangesInfo {
        environment_version: env_version,
        changes_version: new_version,
        changes,
    });

    if let Some(ptr) = env_state.changes_info {
        env_state.changes_info = None;
        unsafe { drop(Box::from_raw(ptr.as_ptr())) }
    }
    env_state.changes_info = NonNull::new(Box::into_raw(changes_info) as *mut _);

    Ok(result)
}

#[tauri::command]
#[specta::specta]
async fn project_apply_pending_changes(
    state: State<'_, Mutex<EnvironmentState>>,
    project_path: String,
    changes_version: u32,
) -> Result<(), RustError> {
    let mut env_state = state.lock().await;
    let env_state = &mut *env_state;
    let changes = unsafe { Box::from_raw(env_state.changes_info.take().unwrap().as_mut()) };
    if changes.changes_version != changes_version {
        return Err(RustError::Unrecoverable("changes version mismatch".into()));
    }
    let changes = *changes;
    if changes.environment_version != env_state.environment.environment_version.0 {
        return Err(RustError::Unrecoverable(
            "environment version mismatch".into(),
        ));
    }

    let environment = env_state.environment.get_environment_mut(false).await?;

    let mut unity_project = load_project(project_path).await?;

    unity_project
        .apply_pending_changes(environment, changes.changes)
        .await?;

    unity_project.save().await?;
    Ok(())
}

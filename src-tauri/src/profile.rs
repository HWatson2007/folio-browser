use crate::history::unix_millis;
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};
use uuid::Uuid;

const REGISTRY_FILE: &str = "profiles.json";
const REGISTRY_LOCK_FILE: &str = "profiles.json.lock";
const LOCK_FILE: &str = "profile.lock";
const LEGACY_HISTORY_FILE: &str = "history.sqlite3";
const LEGACY_WEBVIEW_MARKER: &str = "EBWebView";
const LAUNCHER_DIR: &str = "launcher";
const PROFILES_DIR: &str = "profiles";
const WEBVIEW_DIR: &str = "webview";

/// A validated, UUID-shaped profile identifier. The only type permitted in path joins.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ProfileId(String);

impl ProfileId {
    pub fn new() -> Self {
        Self(Uuid::new_v4().to_string())
    }

    /// Parses a strict hyphenated UUID (8-4-4-4-12) and rejects anything else.
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.len() != 36 {
            return Err("Profile id must be a UUID.".to_owned());
        }
        let bytes = raw.as_bytes();
        if !bytes
            .iter()
            .all(|byte| byte.is_ascii_hexdigit() || *byte == b'-')
        {
            return Err("Profile id contains invalid characters.".to_owned());
        }
        for index in [8usize, 13, 18, 23] {
            if bytes[index] != b'-' {
                return Err("Profile id is not a well-formed UUID.".to_owned());
            }
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ProfileId {
    fn default() -> Self {
        Self::new()
    }
}

/// A profile as recorded in the shared registry.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileRecord {
    pub id: ProfileId,
    pub name: String,
    pub color: String,
    pub created_at: u64,
    pub last_used_at: Option<u64>,
}

/// A small, frontend-friendly view of a profile including its live/running status.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSummary {
    pub id: String,
    pub name: String,
    pub color: String,
    pub created_at: u64,
    pub last_used_at: Option<u64>,
    pub running: bool,
}

impl ProfileSummary {
    pub fn from(record: &ProfileRecord, running: bool) -> Self {
        Self {
            id: record.id.as_str().to_owned(),
            name: record.name.clone(),
            color: record.color.clone(),
            created_at: record.created_at,
            last_used_at: record.last_used_at,
            running,
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RegistryFile {
    version: u32,
    profiles: Vec<ProfileRecord>,
}

const PALETTE: [&str; 6] = [
    "#8a3d2f", "#3f5d7a", "#3f6f52", "#8a6a2f", "#6a4d8a", "#a0523d",
];

pub struct ProfileRegistry {
    app_data_root: PathBuf,
    local_data_root: PathBuf,
}

impl ProfileRegistry {
    pub fn new(app_data_root: PathBuf, local_data_root: PathBuf) -> Self {
        Self {
            app_data_root,
            local_data_root,
        }
    }

    pub fn registry_path(&self) -> PathBuf {
        self.app_data_root.join(REGISTRY_FILE)
    }

    pub fn profile_dir(&self, id: &ProfileId) -> PathBuf {
        self.app_data_root.join(PROFILES_DIR).join(id.as_str())
    }

    pub fn webview_dir(&self, id: &ProfileId) -> PathBuf {
        self.local_data_root
            .join(PROFILES_DIR)
            .join(id.as_str())
            .join(WEBVIEW_DIR)
    }

    pub fn history_path(&self, id: &ProfileId) -> PathBuf {
        self.profile_dir(id).join("history.sqlite3")
    }

    pub fn lock_path(&self, id: &ProfileId) -> PathBuf {
        self.profile_dir(id).join(LOCK_FILE)
    }

    pub fn launcher_dir(&self) -> PathBuf {
        self.local_data_root.join(LAUNCHER_DIR)
    }

    pub fn load(&self) -> Result<Vec<ProfileRecord>, String> {
        let path = self.registry_path();
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(&path).map_err(|error| error.to_string())?;
        let registry: RegistryFile =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        Ok(registry.profiles)
    }

    pub fn find(&self, id: &ProfileId) -> Result<Option<ProfileRecord>, String> {
        Ok(self.load()?.into_iter().find(|record| &record.id == id))
    }

    pub fn create(&self, name: &str) -> Result<ProfileRecord, String> {
        let mut profiles = self.load()?;
        let record = ProfileRecord {
            id: ProfileId::new(),
            name: name.trim().to_owned(),
            color: PALETTE[profiles.len() % PALETTE.len()].to_owned(),
            created_at: unix_millis(),
            last_used_at: Some(unix_millis()),
        };
        fs::create_dir_all(self.profile_dir(&record.id)).map_err(|error| error.to_string())?;
        profiles.push(record.clone());
        self.save(&profiles)?;
        Ok(record)
    }

    pub fn rename(&self, id: &ProfileId, name: &str) -> Result<(), String> {
        let mut profiles = self.load()?;
        let record = profiles
            .iter_mut()
            .find(|record| &record.id == id)
            .ok_or_else(|| "That profile no longer exists.".to_owned())?;
        record.name = name.trim().to_owned();
        self.save(&profiles)
    }

    pub fn touch_last_used(&self, id: &ProfileId) -> Result<(), String> {
        let mut profiles = self.load()?;
        let record = profiles
            .iter_mut()
            .find(|record| &record.id == id)
            .ok_or_else(|| "That profile no longer exists.".to_owned())?;
        record.last_used_at = Some(unix_millis());
        self.save(&profiles)
    }

    /// Removes a profile entirely. Refuses if its window is currently open.
    pub fn delete(&self, id: &ProfileId) -> Result<(), String> {
        if ProfileLock::is_locked(&self.lock_path(id)) {
            return Err("Close this profile's window before deleting it.".to_owned());
        }
        let mut profiles = self.load()?;
        let before = profiles.len();
        profiles.retain(|record| &record.id != id);
        if profiles.len() == before {
            return Err("That profile no longer exists.".to_owned());
        }
        self.save(&profiles)?;
        let _ = fs::remove_dir_all(self.profile_dir(id));
        let _ = fs::remove_dir_all(self.webview_dir(id));
        Ok(())
    }

    fn save(&self, profiles: &[ProfileRecord]) -> Result<(), String> {
        let lock_path = self.app_data_root.join(REGISTRY_LOCK_FILE);
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)
            .map_err(|error| error.to_string())?;
        lock.try_lock_exclusive().map_err(|_| {
            "Another Folio launcher is editing profiles right now. Try again in a moment."
                .to_owned()
        })?;

        fs::create_dir_all(&self.app_data_root).map_err(|error| error.to_string())?;
        let payload = serde_json::to_vec_pretty(&RegistryFile {
            version: 1,
            profiles: profiles.to_vec(),
        })
        .map_err(|error| error.to_string())?;
        let temporary = self.registry_path().with_extension("json.tmp");
        fs::write(&temporary, &payload).map_err(|error| error.to_string())?;
        fs::rename(&temporary, self.registry_path()).map_err(|error| error.to_string())?;
        let _ = lock.unlock();
        Ok(())
    }

    /// One-shot migration from the pre-profile layout. Returns true if it ran.
    pub fn migrate_legacy(&self) -> Result<bool, String> {
        if self.registry_path().exists() {
            return Ok(false);
        }
        let profile = self.create("Default")?;

        let legacy_history = self.app_data_root.join(LEGACY_HISTORY_FILE);
        if legacy_history.exists() {
            move_with_fallback(&legacy_history, &self.history_path(&profile.id));
            for suffix in ["-wal", "-shm"] {
                let source = append_suffix(&legacy_history, suffix);
                if source.exists() {
                    move_with_fallback(
                        &source,
                        &append_suffix(&self.history_path(&profile.id), suffix),
                    );
                }
            }
        }

        let legacy_local = &self.local_data_root;
        if legacy_local.exists()
            && !legacy_local.join(LAUNCHER_DIR).exists()
            && !legacy_local.join(PROFILES_DIR).exists()
            && legacy_local.join(LEGACY_WEBVIEW_MARKER).exists()
        {
            // Move each old child into the new profile's webview folder. The destination
            // is itself inside this root, so the root cannot be renamed wholesale.
            let destination = self.webview_dir(&profile.id);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            fs::create_dir_all(&destination).map_err(|error| error.to_string())?;
            let entries = fs::read_dir(legacy_local).map_err(|error| error.to_string())?;
            for entry in entries {
                let entry = entry.map_err(|error| error.to_string())?;
                let name = entry.file_name();
                if name == LAUNCHER_DIR || name == PROFILES_DIR {
                    continue;
                }
                move_with_fallback(&entry.path(), &destination.join(&name));
            }
        }

        Ok(true)
    }
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.to_string_lossy().into_owned();
    value.push_str(suffix);
    PathBuf::from(value)
}

/// Best-effort relocation with a copy fallback so a locked WebView2 or SQLite file on
/// Windows cannot corrupt the launch; on failure the source is left untouched.
fn move_with_fallback(source: &Path, destination: &Path) {
    if let Some(parent) = destination.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if fs::rename(source, destination).is_ok() {
        return;
    }
    if copy_item(source, destination).is_ok() {
        let _ = fs::remove_dir_all(source);
        let _ = fs::remove_file(source);
    } else {
        eprintln!(
            "Folio could not migrate data from {} to {} and will leave it in place.",
            source.display(),
            destination.display()
        );
    }
}

fn copy_item(source: &Path, destination: &Path) -> Result<(), std::io::Error> {
    if source.is_dir() {
        fs::create_dir_all(destination)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_item(&entry.path(), &destination.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
        Ok(())
    }
}

/// An exclusive advisory lock held for the lifetime of a profile's browser process.
/// The operating system releases it automatically if the process dies.
pub struct ProfileLock {
    _file: File,
}

impl ProfileLock {
    pub fn acquire(path: &Path, pid: u32) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|error| error.to_string())?;
        file.try_lock_exclusive()
            .map_err(|_| "This profile is already open in another window.".to_owned())?;
        let _ = file.write_all(format!("{pid}\n").as_bytes());
        Ok(Self { _file: file })
    }

    /// Probes whether some other process currently holds a profile open.
    pub fn is_locked(path: &Path) -> bool {
        let file = match OpenOptions::new().read(true).write(true).open(path) {
            Ok(file) => file,
            Err(_) => return false,
        };
        if file.try_lock_exclusive().is_ok() {
            let _ = file.unlock();
            false
        } else {
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn temp_registry(name: &str) -> ProfileRegistry {
        let root = std::env::temp_dir().join(format!(
            "folio-profile-test-{}-{}-{}",
            name,
            std::process::id(),
            unix_millis()
        ));
        ProfileRegistry {
            app_data_root: root.join("app"),
            local_data_root: root.join("local"),
        }
    }

    fn cleanup(registry: &ProfileRegistry) {
        if let Some(parent) = registry.app_data_root.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn parses_only_well_formed_uuids() {
        assert!(ProfileId::parse("8f2ceb3a-1c44-44a1-9b3e-5f6b4f6d3c21").is_ok());
        assert!(ProfileId::parse("").is_err());
        assert!(ProfileId::parse("../evil").is_err());
        assert!(ProfileId::parse("8f2ceb3a1c4444a19b3e5f6b4f6d3c21").is_err());
        assert!(ProfileId::parse("8f2ceb3a-1c44-44a1-9b3e-5f6b4f6d3c2!").is_err());
        assert!(ProfileId::parse("not-a-uuid-at-all----").is_err());
        assert!(ProfileId::parse("8f2ceb3a/1c44/44a1/9b3e/5f6b4f6d3c21").is_err());
    }

    #[test]
    fn distinct_profiles_resolve_to_distinct_paths() {
        let registry = temp_registry("paths");
        let a = ProfileId::new();
        let b = ProfileId::new();
        assert_ne!(a, b);
        assert_ne!(registry.history_path(&a), registry.history_path(&b));
        assert_ne!(registry.webview_dir(&a), registry.webview_dir(&b));
        let history = registry.history_path(&a);
        assert_eq!(history, registry.profile_dir(&a).join("history.sqlite3"));
    }

    #[test]
    fn registry_create_rename_delete_roundtrip() {
        let registry = temp_registry("registry");
        let first = registry.create("Work").unwrap();
        let second = registry.create("Personal").unwrap();
        assert!(registry.find(&first.id).unwrap().is_some());
        assert_eq!(registry.load().unwrap().len(), 2);

        registry.rename(&first.id, "Office").unwrap();
        assert_eq!(registry.find(&first.id).unwrap().unwrap().name, "Office");

        registry.delete(&second.id).unwrap();
        assert_eq!(registry.load().unwrap().len(), 1);
        assert!(registry.find(&second.id).unwrap().is_none());
        cleanup(&registry);
    }

    #[test]
    fn delete_refuses_a_live_locked_profile() {
        // Runs a child of the test binary that holds the lock, proving cross-process
        // exclusivity (the path a real picker + browser pair would take).
        if std::env::var_os("FOLIO_LOCK_CHILD").is_some() {
            let dir = std::env::var("FOLIO_LOCK_DIR").unwrap();
            let lock =
                ProfileLock::acquire(&Path::new(&dir).join(LOCK_FILE), std::process::id()).unwrap();
            std::thread::sleep(Duration::from_secs(60));
            drop(lock);
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "folio-lock-test-{}-{}",
            std::process::id(),
            unix_millis()
        ));
        fs::create_dir_all(&root).unwrap();
        let lock_path = root.join(LOCK_FILE);

        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .arg("delete_refuses_a_live_locked_profile")
            .env("FOLIO_LOCK_CHILD", "1")
            .env("FOLIO_LOCK_DIR", &root)
            .spawn()
            .unwrap();

        // Wait for the child to acquire the lock.
        let start = Instant::now();
        while !ProfileLock::is_locked(&lock_path) {
            if start.elapsed() > Duration::from_secs(15) {
                panic!("child never acquired the lock");
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        assert!(ProfileLock::is_locked(&lock_path));

        child.kill().unwrap();
        child.wait().unwrap();
        // The OS releases the lock when the child dies.
        let start = Instant::now();
        while ProfileLock::is_locked(&lock_path) {
            if start.elapsed() > Duration::from_secs(15) {
                panic!("lock was not released after child exit");
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        assert!(!ProfileLock::is_locked(&lock_path));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn migrates_legacy_history_and_webview_only_once() {
        let registry = temp_registry("migration");
        fs::create_dir_all(&registry.app_data_root).unwrap();
        fs::create_dir_all(&registry.local_data_root).unwrap();
        fs::write(registry.app_data_root.join("history.sqlite3"), b"h1").unwrap();
        fs::create_dir_all(registry.local_data_root.join("EBWebView")).unwrap();
        fs::write(registry.local_data_root.join("EBWebView").join("x"), b"w").unwrap();

        assert!(registry.migrate_legacy().unwrap());
        let profiles = registry.load().unwrap();
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Default");
        let id = &profiles[0].id;
        assert_eq!(fs::read(registry.history_path(id)).unwrap(), b"h1".to_vec());
        assert!(!registry.app_data_root.join("history.sqlite3").exists());
        assert!(
            registry
                .webview_dir(id)
                .join("EBWebView")
                .join("x")
                .exists()
        );
        assert!(!registry.local_data_root.join("EBWebView").exists());

        // Second migration must be a no-op.
        assert!(!registry.migrate_legacy().unwrap());
        cleanup(&registry);
    }
}

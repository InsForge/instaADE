//! The sandboxes this computer connected to T3 Code, per instabox user. The server's
//! `instabox-bridge` mark only says a sandbox was connected from somewhere; this set says it
//! was connected from here, by the signed-in user, which is what lets the app run commands on
//! it after a desktop T3 update. Another user signed in on this computer gets their own set.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use serde::Deserialize;

pub trait ConnectedStore: Send + Sync {
    /// The sandboxes `user` connected from this computer.
    fn load(&self, user: i64) -> HashSet<String>;
    fn add(&self, user: i64, id: &str) -> Result<(), String>;
    fn remove(&self, user: i64, id: &str) -> Result<(), String>;
}

type ByUser = BTreeMap<String, BTreeSet<String>>;

/// `connected.json` as written now (`{"<userId>": [ids]}`) or by the first builds (`[ids]`).
#[derive(Deserialize)]
#[serde(untagged)]
enum OnDisk {
    ByUser(ByUser),
    Flat(Vec<String>),
}

/// A JSON map from user id to sandbox ids, such as `connected.json` in the app config dir.
pub struct FileConnectedStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileConnectedStore {
    pub fn new(path: PathBuf) -> Self {
        FileConnectedStore {
            path,
            lock: Mutex::new(()),
        }
    }

    /// The map on disk. A flat list from an older build belongs to `user`, the first user
    /// signed in to read it; the flag says it must be written back as a map. A missing file
    /// is an empty map. Any other read problem, or text that is not either format, is an
    /// error, so a caller never writes over a file it could not understand.
    fn read(&self, user: i64) -> Result<(ByUser, bool), String> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((ByUser::new(), false));
            }
            Err(e) => return Err(self.unreadable(&e.to_string())),
        };
        if text.trim().is_empty() {
            return Ok((ByUser::new(), false));
        }
        match serde_json::from_str::<OnDisk>(&text) {
            Ok(OnDisk::ByUser(m)) => Ok((m, false)),
            Ok(OnDisk::Flat(ids)) => {
                let set: BTreeSet<String> = ids.into_iter().collect();
                Ok((BTreeMap::from([(user.to_string(), set)]), true))
            }
            Err(e) => Err(self.unreadable(&e.to_string())),
        }
    }

    fn unreadable(&self, reason: &str) -> String {
        format!(
            "Could not read the list of connected sandboxes ({}): {reason}. It was left unchanged.",
            self.path.display()
        )
    }

    /// Writes a temp file, then renames it over the real one.
    fn write(&self, map: &ByUser) -> Result<(), String> {
        let json = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| e.to_string())
    }

    /// Reads, lets `f` change the user's set, and writes when anything changed.
    fn change(
        &self,
        user: i64,
        f: impl FnOnce(&mut BTreeSet<String>) -> bool,
    ) -> Result<(), String> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let (mut map, migrated) = self.read(user)?;
        let set = map.entry(user.to_string()).or_default();
        let changed = f(set);
        if set.is_empty() {
            map.remove(&user.to_string());
        }
        if changed || migrated {
            self.write(&map)?;
        }
        Ok(())
    }
}

impl ConnectedStore for FileConnectedStore {
    fn load(&self, user: i64) -> HashSet<String> {
        let _g = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let Ok((map, migrated)) = self.read(user) else {
            return HashSet::new();
        };
        if migrated {
            // Best effort: a failed write migrates again on the next load.
            let _ = self.write(&map);
        }
        map.get(&user.to_string())
            .map(|s| s.iter().cloned().collect())
            .unwrap_or_default()
    }

    fn add(&self, user: i64, id: &str) -> Result<(), String> {
        self.change(user, |s| s.insert(id.to_string()))
    }

    fn remove(&self, user: i64, id: &str) -> Result<(), String> {
        self.change(user, |s| s.remove(id))
    }
}

/// In-memory store, for tests.
#[derive(Default)]
pub struct MemoryConnectedStore(Mutex<HashMap<i64, HashSet<String>>>);

impl ConnectedStore for MemoryConnectedStore {
    fn load(&self, user: i64) -> HashSet<String> {
        self.0
            .lock()
            .unwrap()
            .get(&user)
            .cloned()
            .unwrap_or_default()
    }
    fn add(&self, user: i64, id: &str) -> Result<(), String> {
        self.0
            .lock()
            .unwrap()
            .entry(user)
            .or_default()
            .insert(id.to_string());
        Ok(())
    }
    fn remove(&self, user: i64, id: &str) -> Result<(), String> {
        if let Some(s) = self.0.lock().unwrap().get_mut(&user) {
            s.remove(id);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn file_store_survives_a_reload_and_removes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("connected.json");
        let store = FileConnectedStore::new(path.clone());
        assert!(store.load(7).is_empty(), "missing file is empty");
        store.add(7, "m1").unwrap();
        store.add(7, "m2").unwrap();
        store.add(7, "m1").unwrap();
        let again = FileConnectedStore::new(path.clone());
        assert_eq!(again.load(7), set(&["m1", "m2"]));
        again.remove(7, "m1").unwrap();
        assert_eq!(FileConnectedStore::new(path).load(7), set(&["m2"]));
    }

    #[test]
    fn each_user_has_their_own_set() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connected.json");
        let store = FileConnectedStore::new(path.clone());
        store.add(7, "m1").unwrap();
        store.add(8, "m2").unwrap();
        assert_eq!(store.load(7), set(&["m1"]));
        assert_eq!(store.load(8), set(&["m2"]));
        assert!(store.load(9).is_empty());
        let text = std::fs::read_to_string(&path).unwrap();
        let on_disk: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(on_disk, serde_json::json!({"7": ["m1"], "8": ["m2"]}));
    }

    #[test]
    fn a_flat_list_from_an_older_build_goes_to_the_first_user() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connected.json");
        std::fs::write(&path, r#"["m1","m2"]"#).unwrap();
        let store = FileConnectedStore::new(path.clone());
        assert_eq!(store.load(7), set(&["m1", "m2"]));
        let on_disk: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk, serde_json::json!({"7": ["m1", "m2"]}));
        assert!(store.load(8).is_empty(), "another user does not get them");
        assert_eq!(store.load(7), set(&["m1", "m2"]));
    }

    #[test]
    fn a_corrupt_file_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connected.json");
        std::fs::write(&path, "{not json").unwrap();
        let store = FileConnectedStore::new(path.clone());
        assert!(store.load(7).is_empty());
        let err = store.add(7, "m1").unwrap_err();
        assert!(err.contains("Could not read the list of connected sandboxes"));
        assert!(err.contains("It was left unchanged."));
        assert!(store.remove(7, "m1").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"{not json");
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_never_overwritten() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connected.json");
        let original = r#"{"8":["m9"]}"#;
        std::fs::write(&path, original).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_ok() {
            return; // running as root: permissions do not apply
        }
        let store = FileConnectedStore::new(path.clone());
        assert!(store.load(7).is_empty());
        assert!(store.add(7, "m1").is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn an_empty_file_counts_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connected.json");
        std::fs::write(&path, "  \n").unwrap();
        let store = FileConnectedStore::new(path);
        assert!(store.load(7).is_empty());
        store.add(7, "m1").unwrap();
        assert_eq!(store.load(7), set(&["m1"]));
    }

    #[test]
    fn a_missing_file_is_created_by_add() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connected.json");
        let store = FileConnectedStore::new(path.clone());
        store.add(7, "m1").unwrap();
        assert_eq!(store.load(7), set(&["m1"]));
    }

    #[test]
    fn add_keeps_other_users_ids() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connected.json");
        std::fs::write(&path, r#"{"7":["a1"],"8":["b1","b2"]}"#).unwrap();
        let store = FileConnectedStore::new(path.clone());
        store.add(7, "a2").unwrap();
        assert_eq!(store.load(7), set(&["a1", "a2"]));
        assert_eq!(store.load(8), set(&["b1", "b2"]));
    }
}

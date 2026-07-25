use crate::connection::profile::ConnectionProfile;
use anyhow::{Context as _, Result};
use gpui::{App, AppContext as _, Task};
use std::path::{Path, PathBuf};

pub fn connections_path() -> PathBuf {
    paths::config_dir()
        .join("database_client")
        .join("connections.json")
}

pub fn load_profiles() -> Vec<ConnectionProfile> {
    load_profiles_from(&connections_path())
}

pub(crate) fn load_profiles_from(path: &Path) -> Vec<ConnectionProfile> {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            log::error!("failed to parse {path:?}: {e}");
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}

pub fn save_profiles(profiles: &[ConnectionProfile]) -> Result<()> {
    save_profiles_to(&connections_path(), profiles)
}

pub(crate) fn save_profiles_to(path: &Path, profiles: &[ConnectionProfile]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {parent:?}"))?;
    }
    let json = serde_json::to_string_pretty(profiles)?;
    std::fs::write(path, json).with_context(|| format!("writing {path:?}"))?;
    Ok(())
}

pub fn store_password(cx: &App, profile: &ConnectionProfile, password: &str) -> Task<Result<()>> {
    cx.write_credentials(&profile.keychain_url(), &profile.user, password.as_bytes())
}

pub fn read_password(cx: &App, profile: &ConnectionProfile) -> Task<Result<Option<String>>> {
    let task = cx.read_credentials(&profile.keychain_url());
    cx.background_spawn(async move {
        match task.await? {
            Some((_user, bytes)) => Ok(Some(String::from_utf8(bytes)?)),
            None => Ok(None),
        }
    })
}

pub fn delete_password(cx: &App, profile: &ConnectionProfile) -> Task<Result<()>> {
    cx.delete_credentials(&profile.keychain_url())
}

/// Replace the entry matching `updated.id` in-place. No-op if no entry has that id.
pub(crate) fn replace_profile_by_id(
    profiles: &mut Vec<ConnectionProfile>,
    updated: ConnectionProfile,
) {
    if let Some(entry) = profiles.iter_mut().find(|p| p.id == updated.id) {
        *entry = updated;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::profile::SslMode;

    fn sample() -> ConnectionProfile {
        ConnectionProfile {
            id: "p1".into(),
            name: "Local".into(),
            host: "localhost".into(),
            port: 5432,
            database: "mydb".into(),
            user: "alice".into(),
            ssl_mode: SslMode::Prefer,
            read_only: false,
        }
    }

    #[test]
    fn replace_profile_by_id_updates_matching_entry() {
        let mut profiles = vec![sample()];
        let mut updated = sample();
        updated.name = "Updated".into();
        replace_profile_by_id(&mut profiles, updated);
        assert_eq!(profiles[0].name, "Updated");
    }

    #[test]
    fn replace_profile_by_id_ignores_unknown_id() {
        let mut profiles = vec![sample()];
        let mut unknown = sample();
        unknown.id = "other".into();
        unknown.name = "Other".into();
        replace_profile_by_id(&mut profiles, unknown);
        assert_eq!(profiles[0].name, "Local");
    }

    #[test]
    fn save_then_load_round_trips_via_explicit_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("connections.json");
        let profiles = vec![sample()];
        save_profiles_to(&path, &profiles).unwrap();
        let loaded = load_profiles_from(&path);
        assert_eq!(loaded, profiles);
    }

    #[test]
    fn load_missing_file_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.json");
        assert!(load_profiles_from(&path).is_empty());
    }
}

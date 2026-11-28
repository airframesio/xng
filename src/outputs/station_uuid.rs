//! Station UUIDs: a stable id for each station xng feeds to Airframes, sent
//! as `station_uuid` next to the station ident in every feed. The aggregator
//! finds the station by it before the ident, so renaming a station (on the
//! website, or its `station-id` here) keeps its feed and history.
//!
//! Where a station's UUID comes from, first match wins:
//! 1. `station-uuid` in station.toml (top level or `[outputs.airframes]` for
//!    the base station, `[outputs.airframes.<mode>]`, or a session's
//!    `airframes-station-uuid`), or `XNG_STATION_UUID` for the base station.
//!    Airwaves OS sets this to the UUID the platform answered at sign-in.
//! 2. The UUID saved for the station's slot in `station-uuids.json` (in
//!    `XNG_STATE_DIR`, else `~/.config/xng`). A slot is where the station's
//!    name is set, not the name, so changing `station-id` keeps the UUID.
//! 3. New: UUID v5 of the station ident, saved for the slot. Being derived
//!    from the ident, it is the same on every run even where nothing can be
//!    saved (a container without a volume).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Namespace for UUIDs derived from a station ident.
pub const NAMESPACE: Uuid = Uuid::from_u128(0x2c9a_51d4_7e0b_4f3a_9c61_d84e_3b7f_a205);

/// Environment variable with the base station's UUID.
pub const ENV: &str = "XNG_STATION_UUID";

/// Namespace of the ATC station UUIDs Airframes derives from a feeder id
/// (api-server `ATC_STATION_UUID_NAMESPACE`). Never change it.
pub const ATC_NAMESPACE: Uuid = Uuid::from_u128(0x6f7a_0c52_3f0e_4c8e_9d4a_1b2a_7c4e_5d60);

/// Slot of the ATC feeder's station in the saved map.
pub const ATC_SLOT: &str = "atc";

/// The saved slot → UUID map.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Saved {
    #[serde(default)]
    stations: BTreeMap<String, String>,
}

static LOCK: Mutex<()> = Mutex::new(());

/// `XNG_STATE_DIR`, else the xng config directory.
pub fn state_dir() -> PathBuf {
    std::env::var_os("XNG_STATE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(crate::airband::atc_auth::config_dir)
}

pub fn default_path() -> PathBuf {
    // Unit tests build routers too; keep them out of the real config.
    if cfg!(test) {
        return std::env::temp_dir().join(format!(
            "xng-test-station-uuids-{}.json",
            std::process::id()
        ));
    }
    state_dir().join("station-uuids.json")
}

/// `s` as a canonical (lower-case, hyphenated) UUID, if it is one.
pub fn parse(s: &str) -> Option<String> {
    Uuid::parse_str(s.trim())
        .ok()
        .filter(|u| !u.is_nil())
        .map(|u| u.hyphenated().to_string())
}

/// The UUID derived from a station ident (case and surrounding spaces
/// ignored, as the aggregator matches idents).
pub fn derived(ident: &str) -> String {
    Uuid::new_v5(&NAMESPACE, ident.trim().to_ascii_uppercase().as_bytes())
        .hyphenated()
        .to_string()
}

/// The ATC station UUID Airframes derives from `feeder_id`: the UUID of the
/// station every ATC feeder had before it sent one.
pub fn atc_derived(feeder_id: &str) -> String {
    Uuid::new_v5(
        &ATC_NAMESPACE,
        feeder_id.trim().to_ascii_lowercase().as_bytes(),
    )
    .hyphenated()
    .to_string()
}

/// The ATC feeder's station UUID, sent in HELLO `station.uuid`: `explicit`
/// (`station-uuid` in the ATC settings) when it is a UUID, else the saved
/// one, else the one derived from `feeder_id`, which is then saved. A
/// feeder that already had a station therefore keeps it, and keeps it
/// when its feeder id changes later (a new `atc.json`).
pub fn resolve_atc(feeder_id: &str, explicit: Option<&str>) -> String {
    let env = std::env::var("XNG_ATC_STATION_UUID")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let explicit = explicit.or(env.as_deref());
    resolve_with(&default_path(), ATC_SLOT, "ATC feeder", explicit, || {
        atc_derived(feeder_id)
    })
}

/// `XNG_STATION_UUID`, when set to a UUID.
pub fn from_env() -> Option<String> {
    let v = std::env::var(ENV).ok()?;
    let u = parse(&v);
    if u.is_none() && !v.trim().is_empty() {
        tracing::warn!("{ENV} is not a UUID, ignored: {v:?}");
    }
    u
}

/// The UUID for the station named `ident` in `slot`, from the saved map at
/// the default path (see the module docs). `explicit` wins when it is a UUID.
pub fn resolve(slot: &str, ident: &str, explicit: Option<&str>) -> String {
    resolve_at(&default_path(), slot, ident, explicit)
}

/// As [`resolve`], with the saved map at `path`. Never fails: a map that
/// cannot be read or written leaves the derived UUID.
pub fn resolve_at(path: &Path, slot: &str, ident: &str, explicit: Option<&str>) -> String {
    resolve_with(path, slot, ident, explicit, || derived(ident))
}

fn resolve_with(
    path: &Path,
    slot: &str,
    ident: &str,
    explicit: Option<&str>,
    default: impl FnOnce() -> String,
) -> String {
    if let Some(raw) = explicit {
        match parse(raw) {
            Some(u) => return u,
            None => tracing::warn!("station-uuid for {ident} is not a UUID, ignored: {raw:?}"),
        }
    }
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    resolve_locked(path, slot, ident, default)
}

/// Read, update and save the map under an exclusive lock on
/// `station-uuids.json.lock`, so two xng processes sharing one state
/// directory never drop each other's slots. `LOCK` above covers threads of
/// this process; this covers other processes.
fn resolve_locked(
    path: &Path,
    slot: &str,
    ident: &str,
    default: impl FnOnce() -> String,
) -> String {
    let file_lock = lock_file(path);
    let mut saved = load(path);
    if let Some(u) = saved.stations.get(slot).and_then(|u| parse(u)) {
        return u;
    }
    let u = default();
    // Without the lock, a save could drop slots another process just
    // added: use the UUID for this run and leave the map as it is.
    if file_lock.is_none() {
        return u;
    }
    saved.stations.insert(slot.to_string(), u.clone());
    if let Err(e) = save(path, &saved) {
        tracing::debug!(
            "station uuid for {ident}: not saved to {}: {e}",
            path.display()
        );
    }
    u
}

/// Held until dropped. `None` when the lock can't be taken (the directory or
/// lock file can't be written, or the file system has no locks); nothing is
/// saved then.
fn lock_file(path: &Path) -> Option<std::fs::File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok()?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path.with_extension("json.lock"))
        .ok()?;
    match file.lock() {
        Ok(()) => Some(file),
        Err(e) => {
            tracing::warn!(
                "station uuid map {}: not locked, so not saved: {e}",
                path.display()
            );
            None
        }
    }
}

fn load(path: &Path) -> Saved {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default()
}

fn save(path: &Path, saved: &Saved) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    // One temp file per process, so two writers never share one.
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(saved).unwrap_or_default())?;
    std::fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!("xng-station-uuid-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn derived_is_stable_and_ignores_case() {
        assert_eq!(derived("XX-KSEA"), derived(" xx-ksea "));
        assert_ne!(derived("XX-KSEA"), derived("XX-KSEA-VDL2"));
        assert!(parse(&derived("XX-KSEA")).is_some());
    }

    #[test]
    fn parse_accepts_only_uuids() {
        assert_eq!(
            parse(" 3F2B8C1E-7D4A-4C9E-9B1F-2A6E5D4C3B2A ").as_deref(),
            Some("3f2b8c1e-7d4a-4c9e-9b1f-2a6e5d4c3b2a")
        );
        assert!(parse("XX-KSEA").is_none());
        assert!(parse("00000000-0000-0000-0000-000000000000").is_none());
    }

    #[test]
    fn a_slot_keeps_its_uuid_when_the_ident_changes() {
        let dir = tempdir();
        let path = dir.join("station-uuids.json");
        let first = resolve_at(&path, "base", "XX-OLD", None);
        assert_eq!(first, derived("XX-OLD"));
        assert_eq!(resolve_at(&path, "base", "XX-NEW", None), first);
        // Another slot gets its own.
        assert_eq!(
            resolve_at(&path, "mode:vdl2", "XX-NEW-VDL2", None),
            derived("XX-NEW-VDL2")
        );
        // An explicit UUID wins and is not saved over the slot.
        let set = "3f2b8c1e-7d4a-4c9e-9b1f-2a6e5d4c3b2a";
        assert_eq!(resolve_at(&path, "base", "XX-NEW", Some(set)), set);
        assert_eq!(resolve_at(&path, "base", "XX-NEW", Some("nope")), first);
        assert_eq!(resolve_at(&path, "base", "XX-NEW", None), first);
    }

    #[test]
    fn atc_uuid_matches_the_one_airframes_derives() {
        // api-server: uuidv5(feederId.toLowerCase(), ATC_STATION_UUID_NAMESPACE)
        assert_eq!(
            atc_derived("AAAAAAAA-AAAA-4AAA-8AAA-AAAAAAAAAAAA"),
            "be0d6d34-e87d-53c1-9609-da58d850b485"
        );
        let dir = tempdir();
        let path = dir.join("station-uuids.json");
        let first = resolve_with(&path, ATC_SLOT, "atc", None, || {
            atc_derived("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa")
        });
        // A new feeder id later (atc.json lost) keeps the station.
        let later = resolve_with(&path, ATC_SLOT, "atc", None, || {
            atc_derived("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb")
        });
        assert_eq!(first, later);
    }

    #[test]
    fn concurrent_writers_keep_every_slot() {
        // Each thread opens the lock file itself and skips the in-process
        // mutex, like separate xng processes do. The slow default holds the
        // read-to-save window open so that unlocked writers would overwrite
        // each other.
        let dir = tempdir();
        let path = dir.join("station-uuids.json");
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let path = path.clone();
                std::thread::spawn(move || {
                    let ident = format!("XX-KSEA-{i}");
                    resolve_locked(&path, &format!("mode:{i}"), &ident, || {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        derived(&ident)
                    })
                })
            })
            .collect();
        let got: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        let saved = load(&path);
        for (i, u) in got.iter().enumerate() {
            assert_eq!(saved.stations.get(&format!("mode:{i}")), Some(u));
            // Renamed later: the saved slot still wins.
            assert_eq!(
                resolve_at(&path, &format!("mode:{i}"), "XX-RENAMED", None),
                *u
            );
        }
    }

    #[test]
    fn nothing_is_saved_without_the_lock() {
        let dir = tempdir();
        let path = dir.join("station-uuids.json");
        let theirs = "3f2b8c1e-7d4a-4c9e-9b1f-2a6e5d4c3b2a";
        let mut s = Saved::default();
        s.stations.insert("mode:vdl2".into(), theirs.into());
        save(&path, &s).unwrap();
        // The lock file can't be opened (a directory is in its place),
        // while the map itself stays writable.
        std::fs::create_dir(path.with_extension("json.lock")).unwrap();
        assert_eq!(
            resolve_at(&path, "base", "XX-KSEA", None),
            derived("XX-KSEA")
        );
        // Saved slots are still read; the map is left unchanged.
        assert_eq!(resolve_at(&path, "mode:vdl2", "XX-OTHER", None), theirs);
        let saved = load(&path);
        assert_eq!(saved.stations.len(), 1);
        assert_eq!(
            saved.stations.get("mode:vdl2").map(String::as_str),
            Some(theirs)
        );
    }

    #[test]
    fn an_unwritable_map_still_gives_the_derived_uuid() {
        let dir = tempdir();
        let file = dir.join("file");
        std::fs::write(&file, "x").unwrap();
        let path = file.join("station-uuids.json");
        assert_eq!(
            resolve_at(&path, "base", "XX-KSEA", None),
            derived("XX-KSEA")
        );
    }
}

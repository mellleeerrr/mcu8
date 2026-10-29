//! T10b: one-shot migration of the legacy PNM directory / address book from the
//! configuration-document JSON into normalized SQL rows.
//!
//! Why this lives in `vks-admin` and not in storage:
//! - the legacy format is defined by the admin types (`DirectoryState`,
//!   `AddressRecord`) and by their tolerant parsers;
//! - `vks-admin` has **no runtime dependency** on `vks-storage` (P1-1), so this
//!   module only talks to the neutral [`vks_core::ports::Storage`] methods;
//! - the SQL transaction (marker + rows + document strip) lives in the storage
//!   backends, so a crash can never leave a partial state (P0-1).
//!
//! The document is handled as raw [`serde_json::Value`]: the legacy keys are
//! removed while every unknown/future key is preserved verbatim (P0-5). We
//! never round-trip the whole document through `ResourceState`.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Map, Value};
use tracing::{info, warn};

#[cfg(test)]
use vks_core::ports::storage::Storage;
use vks_core::ports::storage::{
    DirectoryMigrationOutcome, DirectorySnapshot, StoredDirectoryAddress, StoredDirectoryGroup, StoredDirectoryProfile,
    StoredDirectoryRole,
};

use super::directory::{DirectoryGroup, DirectoryRole, DirectoryState, DirectoryUser};
use super::resources::{self, deserialize_address_book, AddressRecord, ResourceState, CONFIGURATION_KEY};
use crate::state::AppState;

/// Legacy `resources.*` keys removed by the migration. Nothing else is touched:
/// `contacts`, `layouts`, `links`, `history`, `admin_settings` stay in the JSON.
pub const LEGACY_DIRECTORY_KEYS: [&str; 2] = ["directory", "addresses"];

/// Counts + outcome of one migration run (logged; also returned to callers).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MigrationReport {
    /// `applied` / `already_migrated` / `no_storage`.
    pub outcome: &'static str,
    /// Retries caused by a concurrent document writer.
    pub attempts: usize,
    /// Whether this run actually imported rows from the legacy document. False
    /// when non-empty SQL rows were adopted, the marker was already present, or
    /// the document had no legacy keys; `already_migrated` also zeroes the
    /// parsed counts because nothing was written.
    pub imported: bool,
    /// Profiles in the parsed legacy document.
    pub profiles: usize,
    /// Groups in the parsed legacy document.
    pub groups: usize,
    /// Custom/override roles in the parsed legacy document.
    pub roles: usize,
    /// Global addresses in the parsed legacy document.
    pub global_addresses: usize,
    /// Personal address copies in the parsed legacy document.
    pub personal_addresses: usize,
}

/// Parsed legacy document: the SQL snapshot plus the stripped document JSON.
#[derive(Debug)]
pub(crate) struct LegacyParse {
    pub snapshot: DirectorySnapshot,
    /// Document with `resources.directory`/`resources.addresses` removed;
    /// `None` when the document had neither key (or did not exist), so no
    /// pointless rewrite happens.
    pub new_document: Option<String>,
    pub report: MigrationReport,
}

/// Reads the legacy keys out of the raw document (tolerant, per record).
///
/// * a malformed JSON document is a hard error — it is never truncated;
/// * a single malformed profile/group/role/address is skipped with a warning
///   instead of aborting the whole migration (P1-12);
/// * duplicate global canonical URIs are collapsed and personal references
///   rewritten before insert (P1-3/dedup).
pub(crate) fn parse_legacy_document(raw: Option<&str>) -> Result<LegacyParse, String> {
    let Some(raw) = raw else {
        return Ok(LegacyParse {
            snapshot: DirectorySnapshot::default(),
            new_document: None,
            report: MigrationReport::default(),
        });
    };
    let mut document: Value =
        serde_json::from_str(raw).map_err(|e| format!("legacy document is not valid JSON: {e}"))?;
    let Some(resources) = document.get_mut("resources").and_then(Value::as_object_mut) else {
        return Ok(LegacyParse {
            snapshot: DirectorySnapshot::default(),
            new_document: None,
            report: MigrationReport::default(),
        });
    };
    let had_directory = resources.contains_key("directory");
    let had_addresses = resources.contains_key("addresses");
    if !had_directory && !had_addresses {
        return Ok(LegacyParse {
            snapshot: DirectorySnapshot::default(),
            new_document: None,
            report: MigrationReport::default(),
        });
    }

    // Raw address objects are kept aside so unknown/future address fields
    // survive into `directory_addresses.address_json` (P0-5).
    let raw_global = resources.get("addresses").and_then(Value::as_object).cloned().unwrap_or_default();
    let raw_personal = resources
        .get("directory")
        .and_then(|directory| directory.get("user_addresses"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut addresses = resources.get("addresses").map(deserialize_address_book).unwrap_or_default();
    let mut directory = parse_directory(resources.get("directory"));
    dedup_address_book(&mut addresses, &mut directory.user_addresses, &mut directory.users);

    let mut report = MigrationReport {
        profiles: directory.users.len(),
        groups: directory.groups.len(),
        roles: directory.roles.len(),
        global_addresses: addresses.len(),
        personal_addresses: directory.user_addresses.values().map(BTreeMap::len).sum(),
        ..MigrationReport::default()
    };
    let mut snapshot = snapshot_from_parts(&addresses, &directory);
    overlay_raw_address_json(&mut snapshot, &raw_global, &raw_personal);

    resources.remove("directory");
    resources.remove("addresses");
    report.imported = true;
    let new_document = Some(serde_json::to_string(&document).map_err(|e| format!("legacy document reserialize: {e}"))?);
    Ok(LegacyParse { snapshot, new_document, report })
}

/// Overlays the original raw address objects on the stored snapshot so
/// unknown/future address fields are not dropped by `AddressRecord` parsing.
/// Known columns always win (the dedup step may have merged duplicates).
fn overlay_raw_address_json(
    snapshot: &mut DirectorySnapshot,
    raw_global: &Map<String, Value>,
    raw_personal: &Map<String, Value>,
) {
    for address in &mut snapshot.addresses {
        let raw = if address.owner_id.is_empty() {
            raw_global.get(&address.id)
        } else {
            raw_personal.get(&address.owner_id).and_then(Value::as_object).and_then(|book| book.get(&address.id))
        };
        let Some(mut object) = raw.and_then(Value::as_object).cloned() else { continue };
        // Unknown fields merged into the kept record by dedup must survive the
        // raw overlay as well (the raw object only knows this record's fields).
        if let Ok(stored) = serde_json::from_str::<Value>(&address.address_json) {
            if let Some(stored) = stored.as_object() {
                for (key, value) in stored {
                    if !matches!(key.as_str(), "id" | "uri" | "display_name" | "created_at" | "payload") {
                        object.entry(key.clone()).or_insert_with(|| value.clone());
                    }
                }
            }
        }
        object.insert("id".to_string(), json!(address.id));
        object.insert("uri".to_string(), json!(address.uri));
        object.insert("display_name".to_string(), json!(address.display_name));
        object.insert("created_at".to_string(), json!(address.created_at));
        object.insert("payload".to_string(), serde_json::from_str(&address.payload_json).unwrap_or_else(|_| json!({})));
        address.address_json = Value::Object(object).to_string();
    }
}

/// Tolerant directory parser: each entry is parsed independently; a broken
/// entry is logged and skipped rather than failing the whole migration.
fn parse_directory(raw: Option<&Value>) -> DirectoryState {
    let mut directory = DirectoryState::default();
    let Some(raw) = raw.and_then(Value::as_object) else {
        return directory;
    };
    if let Some(users) = raw.get("users").and_then(Value::as_object) {
        for (id, value) in users {
            match serde_json::from_value::<DirectoryUser>(value.clone()) {
                Ok(user) => {
                    directory.users.insert(id.clone(), user);
                }
                Err(error) => warn!(%id, %error, "T10 migration: skipping malformed directory user"),
            }
        }
    }
    if let Some(groups) = raw.get("groups").and_then(Value::as_object) {
        for (id, value) in groups {
            match serde_json::from_value::<DirectoryGroup>(value.clone()) {
                Ok(group) => {
                    directory.groups.insert(id.clone(), group);
                }
                Err(error) => warn!(%id, %error, "T10 migration: skipping malformed directory group"),
            }
        }
    }
    if let Some(roles) = raw.get("roles").and_then(Value::as_object) {
        for (id, value) in roles {
            match serde_json::from_value::<DirectoryRole>(value.clone()) {
                Ok(role) => {
                    directory.roles.insert(id.clone(), role);
                }
                Err(error) => warn!(%id, %error, "T10 migration: skipping malformed directory role"),
            }
        }
    }
    if let Some(books) = raw.get("user_addresses").and_then(Value::as_object) {
        for (user_id, book) in books {
            let parsed = deserialize_address_book(book);
            if !parsed.is_empty() {
                directory.user_addresses.insert(user_id.clone(), parsed);
            }
        }
    }
    directory
}

/// Collapses duplicate global addresses by canonical URI (earliest `created_at`
/// wins, payloads merge) and points personal copies/primary references at the
/// surviving id. Then deduplicates each personal book independently.
pub(crate) fn dedup_address_book(
    addresses: &mut BTreeMap<String, AddressRecord>,
    user_addresses: &mut BTreeMap<String, BTreeMap<String, AddressRecord>>,
    users: &mut BTreeMap<String, DirectoryUser>,
) {
    let mut keep_by_uri: BTreeMap<String, String> = BTreeMap::new();
    let mut aliases: Vec<(String, String)> = Vec::new();
    let mut ids: Vec<String> = addresses.keys().cloned().collect();
    ids.sort_by_key(|id| {
        addresses.get(id).map(|record| (record.created_at, record.id.clone())).unwrap_or((u64::MAX, id.clone()))
    });
    for id in ids {
        let Some(key) = addresses.get(&id).map(|record| vks_common::uri::canonical_uri(&record.uri)) else { continue };
        match keep_by_uri.get(&key).cloned() {
            Some(keep) => aliases.push((id, keep)),
            None => {
                keep_by_uri.insert(key, id);
            }
        }
    }
    for (duplicate_id, keep_id) in aliases {
        let Some(duplicate) = addresses.remove(&duplicate_id) else { continue };
        let Some(keep) = addresses.get_mut(&keep_id) else { continue };
        keep.created_at = keep.created_at.min(duplicate.created_at);
        // The tolerant legacy parser fills a missing display_name with the URI.
        // A real name from the duplicate must replace that placeholder.
        if (keep.display_name.trim().is_empty() || keep.display_name == keep.uri)
            && !duplicate.display_name.trim().is_empty()
            && duplicate.display_name != duplicate.uri
        {
            keep.display_name = duplicate.display_name;
        }
        resources::merge_json(&mut keep.payload, &duplicate.payload);
        for (key, value) in duplicate.extra {
            keep.extra.entry(key).or_insert(value);
        }
        for book in user_addresses.values_mut() {
            if book.remove(&duplicate_id).is_some() {
                book.insert(keep_id.clone(), keep.clone());
            }
        }
        for profile in users.values_mut() {
            if profile.primary_address.as_deref() == Some(duplicate_id.as_str()) {
                profile.primary_address = Some(keep_id.clone());
            }
        }
    }
    // Per-user canonical dedup (idempotent; personal copies may share a URI
    // with the global book, which is legitimate). Duplicate ids are rewritten
    // in `primary_address` exactly like the global aliases above, so a dedup
    // cannot leave the profile pointing at a removed row (T10 P3-1).
    for (user_id, book) in user_addresses.iter_mut() {
        let aliases = dedup_personal_book(book);
        let Some(profile) = users.get_mut(user_id) else { continue };
        for (duplicate_id, keep_id) in aliases {
            if profile.primary_address.as_deref() == Some(duplicate_id.as_str()) {
                profile.primary_address = Some(keep_id);
            }
        }
    }
}

/// Deduplicates one personal book by canonical URI; earliest `created_at` wins.
/// Returns `(duplicate_id, keep_id)` aliases so the caller can rewrite
/// `users[].primary_address`.
fn dedup_personal_book(book: &mut BTreeMap<String, AddressRecord>) -> Vec<(String, String)> {
    let mut ids: Vec<String> = book.keys().cloned().collect();
    ids.sort_by_key(|id| {
        book.get(id).map(|record| (record.created_at, record.id.clone())).unwrap_or((u64::MAX, id.clone()))
    });
    let mut keep_by_uri: BTreeMap<String, String> = BTreeMap::new();
    let mut aliases: Vec<(String, String)> = Vec::new();
    for id in ids {
        let Some(key) = book.get(&id).map(|record| vks_common::uri::canonical_uri(&record.uri)) else { continue };
        match keep_by_uri.get(&key).cloned() {
            Some(keep) => aliases.push((id, keep)),
            None => {
                keep_by_uri.insert(key, id);
            }
        }
    }
    for (duplicate_id, keep_id) in &aliases {
        let Some(duplicate) = book.remove(duplicate_id) else { continue };
        if let Some(keep) = book.get_mut(keep_id) {
            keep.created_at = keep.created_at.min(duplicate.created_at);
            if (keep.display_name.trim().is_empty() || keep.display_name == keep.uri)
                && !duplicate.display_name.trim().is_empty()
                && duplicate.display_name != duplicate.uri
            {
                keep.display_name = duplicate.display_name;
            }
            resources::merge_json(&mut keep.payload, &duplicate.payload);
            for (key, value) in duplicate.extra {
                keep.extra.entry(key).or_insert(value);
            }
        }
    }
    aliases
}

/// Admin-level state → neutral SQL snapshot.
pub(crate) fn snapshot_from_parts(
    addresses: &BTreeMap<String, AddressRecord>,
    directory: &DirectoryState,
) -> DirectorySnapshot {
    let mut group_members: BTreeSet<(String, String)> = BTreeSet::new();
    let mut user_roles: BTreeSet<(String, String)> = BTreeSet::new();
    let mut profiles = Vec::with_capacity(directory.users.len());
    for (user_id, user) in &directory.users {
        profiles.push(StoredDirectoryProfile {
            user_id: user_id.clone(),
            profile_json: serde_json::to_string(user).unwrap_or_else(|_| "{}".to_string()),
            updated_at: user.created_at,
        });
        for group_id in &user.groups {
            group_members.insert((group_id.clone(), user_id.clone()));
        }
        for role_id in &user.roles {
            user_roles.insert((user_id.clone(), role_id.clone()));
        }
    }
    let groups = directory
        .groups
        .iter()
        .map(|(id, group)| StoredDirectoryGroup {
            id: id.clone(),
            group_json: serde_json::to_string(group).unwrap_or_else(|_| "{}".to_string()),
            updated_at: group.created_at,
        })
        .collect();
    let roles = directory
        .roles
        .iter()
        .map(|(id, role)| StoredDirectoryRole {
            id: id.clone(),
            role_json: serde_json::to_string(role).unwrap_or_else(|_| "{}".to_string()),
            updated_at: role.created_at,
        })
        .collect();
    let mut stored_addresses: Vec<StoredDirectoryAddress> =
        addresses.iter().map(|(id, record)| stored_address("", id, record)).collect();
    for (user_id, book) in &directory.user_addresses {
        stored_addresses.extend(book.iter().map(|(id, record)| stored_address(user_id, id, record)));
    }
    stored_addresses.sort_by(|left, right| left.owner_id.cmp(&right.owner_id).then_with(|| left.id.cmp(&right.id)));
    DirectorySnapshot {
        profiles,
        groups,
        roles,
        group_members: group_members.into_iter().collect(),
        user_roles: user_roles.into_iter().collect(),
        addresses: stored_addresses,
    }
}

fn stored_address(owner_id: &str, id: &str, record: &AddressRecord) -> StoredDirectoryAddress {
    StoredDirectoryAddress {
        owner_id: owner_id.to_string(),
        id: id.to_string(),
        uri: record.uri.clone(),
        uri_canonical: vks_common::uri::canonical_uri(&record.uri),
        display_name: record.display_name.clone(),
        payload_json: serde_json::to_string(&record.payload).unwrap_or_else(|_| "{}".to_string()),
        address_json: serde_json::to_string(record).unwrap_or_else(|_| "{}".to_string()),
        created_at: record.created_at,
    }
}

/// Neutral SQL snapshot → admin RAM cache. Malformed JSON rows are skipped with
/// a warning (a corrupt row must not take the whole directory down).
pub(crate) fn apply_snapshot(resources: &mut ResourceState, snapshot: &DirectorySnapshot) {
    let mut directory = DirectoryState::default();
    for profile in &snapshot.profiles {
        match serde_json::from_str::<DirectoryUser>(&profile.profile_json) {
            Ok(user) => {
                directory.users.insert(profile.user_id.clone(), user);
            }
            Err(error) => warn!(user_id = %profile.user_id, %error, "T10: skipping malformed stored profile"),
        }
    }
    for group in &snapshot.groups {
        match serde_json::from_str::<DirectoryGroup>(&group.group_json) {
            Ok(value) => {
                directory.groups.insert(group.id.clone(), value);
            }
            Err(error) => warn!(group_id = %group.id, %error, "T10: skipping malformed stored group"),
        }
    }
    for role in &snapshot.roles {
        match serde_json::from_str::<DirectoryRole>(&role.role_json) {
            Ok(value) => {
                directory.roles.insert(role.id.clone(), value);
            }
            Err(error) => warn!(role_id = %role.id, %error, "T10: skipping malformed stored role"),
        }
    }
    for (group_id, user_id) in &snapshot.group_members {
        if let Some(user) = directory.users.get_mut(user_id) {
            if !user.groups.contains(group_id) {
                user.groups.push(group_id.clone());
            }
        }
    }
    for (user_id, role_id) in &snapshot.user_roles {
        if let Some(user) = directory.users.get_mut(user_id) {
            if !user.roles.contains(role_id) {
                user.roles.push(role_id.clone());
            }
        }
    }
    for address in &snapshot.addresses {
        let record = serde_json::from_str::<AddressRecord>(&address.address_json).unwrap_or_else(|_| AddressRecord {
            id: address.id.clone(),
            uri: address.uri.clone(),
            display_name: address.display_name.clone(),
            created_at: address.created_at,
            payload: serde_json::from_str(&address.payload_json).unwrap_or_else(|_| json!({})),
            extra: BTreeMap::new(),
        });
        if address.owner_id.is_empty() {
            resources.addresses.insert(address.id.clone(), record);
        } else {
            directory.user_addresses.entry(address.owner_id.clone()).or_default().insert(address.id.clone(), record);
        }
    }
    resources.directory = directory;
}

/// Builds the neutral snapshot from the live RAM cache (used by every mutation
/// commit, so SQL rows and memory can never diverge).
pub(crate) fn snapshot_from_resources(resources: &ResourceState) -> DirectorySnapshot {
    snapshot_from_parts(&resources.addresses, &resources.directory)
}

/// Runs the one-shot migration against the configured storage.
///
/// Retries up to 3 times when a concurrent writer changed the configuration
/// document between read and CAS. A malformed document or any storage error is
/// propagated: startup must fail rather than serve a partial directory
/// (`storage_allow_fallback` must not mask it; P1-5).
pub async fn run(state: &AppState) -> Result<MigrationReport, String> {
    let Some(storage) = state.storage.as_ref() else {
        return Ok(MigrationReport { outcome: "no_storage", ..MigrationReport::default() });
    };
    let mut attempts = 0usize;
    loop {
        attempts += 1;
        let old = storage
            .get_configuration_document(CONFIGURATION_KEY)
            .await
            .map_err(|error| format!("T10 directory migration: cannot read configuration document: {error}"))?;
        let parsed =
            parse_legacy_document(old.as_deref()).map_err(|error| format!("T10 directory migration: {error}"))?;
        // Only the storage decides whether the legacy document is imported or
        // existing SQL rows are adopted; reading the rows before the migration
        // lets the report say which branch actually ran (T10 P3-2).
        let existing_rows = storage
            .load_directory_snapshot()
            .await
            .map_err(|error| format!("T10 directory migration: cannot read directory snapshot: {error}"))?;
        let outcome = storage
            .migrate_directory_document(
                CONFIGURATION_KEY,
                old.as_deref(),
                parsed.new_document.as_deref(),
                &parsed.snapshot,
            )
            .await
            .map_err(|error| format!("T10 directory migration: cannot persist directory snapshot: {error}"))?;
        match outcome {
            DirectoryMigrationOutcome::Applied => {
                let report = applied_report(attempts, parsed.report, existing_rows.is_empty());
                log_report(&report);
                return Ok(report);
            }
            DirectoryMigrationOutcome::AlreadyMigrated => {
                let report = already_migrated_report(attempts);
                log_report(&report);
                return Ok(report);
            }
            DirectoryMigrationOutcome::DocumentChanged => {
                if attempts >= 3 {
                    return Err(
                        "T10 directory migration: configuration document kept changing during migration; retry on next start"
                            .to_string(),
                    );
                }
                warn!(attempt = attempts, "T10 directory migration: document changed concurrently, retrying");
            }
        }
    }
}

/// Honest report for an `Applied` outcome: `imported` is false when non-empty
/// SQL rows won (adopt) or the document had no legacy keys.
fn applied_report(attempts: usize, parsed: MigrationReport, sql_was_empty: bool) -> MigrationReport {
    MigrationReport { outcome: "applied", attempts, imported: parsed.imported && sql_was_empty, ..parsed }
}

/// `already_migrated` writes nothing, so it must not report legacy counts as if
/// they had been imported (T10 P3-2).
fn already_migrated_report(attempts: usize) -> MigrationReport {
    MigrationReport { outcome: "already_migrated", attempts, ..MigrationReport::default() }
}

fn log_report(report: &MigrationReport) {
    info!(
        outcome = report.outcome,
        attempts = report.attempts,
        imported = report.imported,
        profiles = report.profiles,
        groups = report.groups,
        roles = report.roles,
        global_addresses = report.global_addresses,
        personal_addresses = report.personal_addresses,
        "T10 directory migration finished"
    );
}

/// Test helper: migrate a document directly through a storage implementation,
/// without constructing an `AppState`. Mirrors [`run`] but is storage-only.
#[cfg(test)]
pub(crate) async fn run_on_storage(storage: &dyn Storage, document: Option<&str>) -> Result<MigrationReport, String> {
    let parsed = parse_legacy_document(document)?;
    let existing_rows = storage.load_directory_snapshot().await.map_err(|error| error.to_string())?;
    let outcome = storage
        .migrate_directory_document(CONFIGURATION_KEY, document, parsed.new_document.as_deref(), &parsed.snapshot)
        .await
        .map_err(|error| error.to_string())?;
    Ok(match outcome {
        DirectoryMigrationOutcome::Applied => applied_report(1, parsed.report, existing_rows.is_empty()),
        DirectoryMigrationOutcome::AlreadyMigrated => already_migrated_report(1),
        DirectoryMigrationOutcome::DocumentChanged => {
            MigrationReport { outcome: "document_changed", attempts: 1, ..MigrationReport::default() }
        }
    })
}

#[cfg(test)]
#[path = "directory_migrate_tests.rs"]
mod directory_migrate_tests;

//! Адаптер реализаций хранилища под порт `vks_core::ports::Storage`.
//!
//! Связывает конкретные backend-хранилища (`MemoryStorage`, `SqliteStorage`)
//! с доменным трейтом ядра `vks_core::ports::Storage`.

use async_trait::async_trait;
use std::str::FromStr;

use vks_core::participant::{Participant, ParticipantId, UserId};
use vks_core::ports::storage::{
    ApiKey, AuditEntry, DirectoryMigrationOutcome, DirectorySnapshot, Layout, RecordEntry, Storage, StorageError,
    StoredDirectoryAddress, StoredDirectoryGroup, StoredDirectoryProfile, StoredDirectoryRole, StoredUser,
    UserCredentials,
};
use vks_core::room::{Room, RoomId, RoomMode};

use crate::memory::MemoryStorage;
use crate::postgres::PostgresStorage;
use crate::sqlite::SqliteStorage;

/// T10 marker: `schema_state('directory')` version written by the one-shot
/// legacy-document migration.
const DIRECTORY_MARKER_KEY: &str = "directory";
const DIRECTORY_MARKER_VERSION: i64 = 1;

fn unix_now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Converts an SQL directory row into the neutral snapshot record.
#[allow(clippy::too_many_arguments)]
fn address_from_columns(
    (owner_id, id, uri, display_name, payload_json, address_json, created_at): (
        String,
        String,
        String,
        String,
        String,
        String,
        i64,
    ),
) -> StoredDirectoryAddress {
    StoredDirectoryAddress {
        owner_id,
        uri_canonical: vks_common::uri::canonical_uri(&uri),
        id,
        uri,
        display_name,
        payload_json,
        address_json,
        created_at: created_at.max(0) as u64,
    }
}

fn validate_snapshot(snapshot: &DirectorySnapshot) -> Result<(), StorageError> {
    // The global partial unique index would reject duplicate canonical URIs;
    // fail with an actionable storage error before the transaction dies with a
    // raw constraint violation. Personal books are intentionally not unique.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for address in snapshot.addresses.iter().filter(|address| address.owner_id.is_empty()) {
        if !seen.insert(address.uri_canonical.as_str()) {
            return Err(StorageError::Other(format!(
                "directory snapshot has duplicate global canonical URI {} ({})",
                address.uri_canonical, address.id
            )));
        }
    }
    Ok(())
}

// --- SQLite helpers (T10) --------------------------------------------------

async fn sqlite_replace_snapshot_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    snapshot: &DirectorySnapshot,
) -> Result<(), StorageError> {
    validate_snapshot(snapshot)?;
    // Children first, then parents: no FKs in v1, but keep the order explicit.
    for table in [
        "directory_group_members",
        "directory_user_roles",
        "directory_addresses",
        "directory_profiles",
        "directory_groups",
        "directory_roles",
    ] {
        sqlx::query(&format!("DELETE FROM {table}")).execute(&mut **tx).await.map_err(to_storage_err)?;
    }
    for profile in &snapshot.profiles {
        sqlx::query("INSERT INTO directory_profiles(user_id, profile_json, updated_at) VALUES($1,$2,$3)")
            .bind(&profile.user_id)
            .bind(&profile.profile_json)
            .bind(profile.updated_at.min(i64::MAX as u64) as i64)
            .execute(&mut **tx)
            .await
            .map_err(to_storage_err)?;
    }
    for group in &snapshot.groups {
        sqlx::query("INSERT INTO directory_groups(id, group_json, updated_at) VALUES($1,$2,$3)")
            .bind(&group.id)
            .bind(&group.group_json)
            .bind(group.updated_at.min(i64::MAX as u64) as i64)
            .execute(&mut **tx)
            .await
            .map_err(to_storage_err)?;
    }
    for role in &snapshot.roles {
        sqlx::query("INSERT INTO directory_roles(id, role_json, updated_at) VALUES($1,$2,$3)")
            .bind(&role.id)
            .bind(&role.role_json)
            .bind(role.updated_at.min(i64::MAX as u64) as i64)
            .execute(&mut **tx)
            .await
            .map_err(to_storage_err)?;
    }
    for (group_id, user_id) in &snapshot.group_members {
        sqlx::query("INSERT INTO directory_group_members(group_id, user_id) VALUES($1,$2)")
            .bind(group_id)
            .bind(user_id)
            .execute(&mut **tx)
            .await
            .map_err(to_storage_err)?;
    }
    for (user_id, role_id) in &snapshot.user_roles {
        sqlx::query("INSERT INTO directory_user_roles(user_id, role_id) VALUES($1,$2)")
            .bind(user_id)
            .bind(role_id)
            .execute(&mut **tx)
            .await
            .map_err(to_storage_err)?;
    }
    for address in &snapshot.addresses {
        sqlx::query(
            "INSERT INTO directory_addresses(owner_id,id,uri,uri_canonical,display_name,payload_json,address_json,created_at)
             VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(&address.owner_id)
        .bind(&address.id)
        .bind(&address.uri)
        .bind(&address.uri_canonical)
        .bind(&address.display_name)
        .bind(&address.payload_json)
        .bind(&address.address_json)
        .bind(address.created_at.min(i64::MAX as u64) as i64)
        .execute(&mut **tx)
        .await
        .map_err(to_storage_err)?;
    }
    Ok(())
}

async fn sqlite_load_snapshot(pool: &sqlx::SqlitePool) -> Result<DirectorySnapshot, StorageError> {
    let mut snapshot = DirectorySnapshot::default();
    let profiles: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT user_id, profile_json, updated_at FROM directory_profiles ORDER BY user_id")
            .fetch_all(pool)
            .await
            .map_err(to_storage_err)?;
    snapshot.profiles = profiles
        .into_iter()
        .map(|(user_id, profile_json, updated_at)| StoredDirectoryProfile {
            user_id,
            profile_json,
            updated_at: updated_at.max(0) as u64,
        })
        .collect();
    let groups: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT id, group_json, updated_at FROM directory_groups ORDER BY id")
            .fetch_all(pool)
            .await
            .map_err(to_storage_err)?;
    snapshot.groups = groups
        .into_iter()
        .map(|(id, group_json, updated_at)| StoredDirectoryGroup {
            id,
            group_json,
            updated_at: updated_at.max(0) as u64,
        })
        .collect();
    let roles: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT id, role_json, updated_at FROM directory_roles ORDER BY id")
            .fetch_all(pool)
            .await
            .map_err(to_storage_err)?;
    snapshot.roles = roles
        .into_iter()
        .map(|(id, role_json, updated_at)| StoredDirectoryRole { id, role_json, updated_at: updated_at.max(0) as u64 })
        .collect();
    snapshot.group_members =
        sqlx::query_as("SELECT group_id, user_id FROM directory_group_members ORDER BY group_id, user_id")
            .fetch_all(pool)
            .await
            .map_err(to_storage_err)?;
    snapshot.user_roles = sqlx::query_as("SELECT user_id, role_id FROM directory_user_roles ORDER BY user_id, role_id")
        .fetch_all(pool)
        .await
        .map_err(to_storage_err)?;
    let addresses: Vec<(String, String, String, String, String, String, i64)> = sqlx::query_as(
        "SELECT owner_id,id,uri,display_name,payload_json,address_json,created_at
         FROM directory_addresses ORDER BY owner_id, id",
    )
    .fetch_all(pool)
    .await
    .map_err(to_storage_err)?;
    snapshot.addresses = addresses.into_iter().map(address_from_columns).collect();
    Ok(snapshot)
}

async fn sqlite_directory_row_count(tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>) -> Result<i64, StorageError> {
    sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM directory_profiles) + (SELECT COUNT(*) FROM directory_groups)
              + (SELECT COUNT(*) FROM directory_roles) + (SELECT COUNT(*) FROM directory_group_members)
              + (SELECT COUNT(*) FROM directory_user_roles) + (SELECT COUNT(*) FROM directory_addresses)",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(to_storage_err)
}

async fn sqlite_schema_version(pool: &sqlx::SqlitePool) -> Result<Option<u64>, StorageError> {
    let version: Option<i64> = sqlx::query_scalar("SELECT version FROM schema_state WHERE state_key = $1")
        .bind(DIRECTORY_MARKER_KEY)
        .fetch_optional(pool)
        .await
        .map_err(to_storage_err)?;
    Ok(version.map(|value| value.max(0) as u64))
}

// --- PostgreSQL helpers (T10) ---------------------------------------------

async fn postgres_replace_snapshot_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    snapshot: &DirectorySnapshot,
) -> Result<(), StorageError> {
    validate_snapshot(snapshot)?;
    for table in [
        "directory_group_members",
        "directory_user_roles",
        "directory_addresses",
        "directory_profiles",
        "directory_groups",
        "directory_roles",
    ] {
        sqlx::query(&format!("DELETE FROM {table}")).execute(&mut **tx).await.map_err(to_storage_err)?;
    }
    for profile in &snapshot.profiles {
        sqlx::query("INSERT INTO directory_profiles(user_id, profile_json, updated_at) VALUES($1,$2,$3)")
            .bind(&profile.user_id)
            .bind(&profile.profile_json)
            .bind(profile.updated_at.min(i64::MAX as u64) as i64)
            .execute(&mut **tx)
            .await
            .map_err(to_storage_err)?;
    }
    for group in &snapshot.groups {
        sqlx::query("INSERT INTO directory_groups(id, group_json, updated_at) VALUES($1,$2,$3)")
            .bind(&group.id)
            .bind(&group.group_json)
            .bind(group.updated_at.min(i64::MAX as u64) as i64)
            .execute(&mut **tx)
            .await
            .map_err(to_storage_err)?;
    }
    for role in &snapshot.roles {
        sqlx::query("INSERT INTO directory_roles(id, role_json, updated_at) VALUES($1,$2,$3)")
            .bind(&role.id)
            .bind(&role.role_json)
            .bind(role.updated_at.min(i64::MAX as u64) as i64)
            .execute(&mut **tx)
            .await
            .map_err(to_storage_err)?;
    }
    for (group_id, user_id) in &snapshot.group_members {
        sqlx::query("INSERT INTO directory_group_members(group_id, user_id) VALUES($1,$2)")
            .bind(group_id)
            .bind(user_id)
            .execute(&mut **tx)
            .await
            .map_err(to_storage_err)?;
    }
    for (user_id, role_id) in &snapshot.user_roles {
        sqlx::query("INSERT INTO directory_user_roles(user_id, role_id) VALUES($1,$2)")
            .bind(user_id)
            .bind(role_id)
            .execute(&mut **tx)
            .await
            .map_err(to_storage_err)?;
    }
    for address in &snapshot.addresses {
        sqlx::query(
            "INSERT INTO directory_addresses(owner_id,id,uri,uri_canonical,display_name,payload_json,address_json,created_at)
             VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
        )
        .bind(&address.owner_id)
        .bind(&address.id)
        .bind(&address.uri)
        .bind(&address.uri_canonical)
        .bind(&address.display_name)
        .bind(&address.payload_json)
        .bind(&address.address_json)
        .bind(address.created_at.min(i64::MAX as u64) as i64)
        .execute(&mut **tx)
        .await
        .map_err(to_storage_err)?;
    }
    Ok(())
}

async fn postgres_load_snapshot(pool: &sqlx::PgPool) -> Result<DirectorySnapshot, StorageError> {
    let mut snapshot = DirectorySnapshot::default();
    let profiles: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT user_id, profile_json, updated_at FROM directory_profiles ORDER BY user_id")
            .fetch_all(pool)
            .await
            .map_err(to_storage_err)?;
    snapshot.profiles = profiles
        .into_iter()
        .map(|(user_id, profile_json, updated_at)| StoredDirectoryProfile {
            user_id,
            profile_json,
            updated_at: updated_at.max(0) as u64,
        })
        .collect();
    let groups: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT id, group_json, updated_at FROM directory_groups ORDER BY id")
            .fetch_all(pool)
            .await
            .map_err(to_storage_err)?;
    snapshot.groups = groups
        .into_iter()
        .map(|(id, group_json, updated_at)| StoredDirectoryGroup {
            id,
            group_json,
            updated_at: updated_at.max(0) as u64,
        })
        .collect();
    let roles: Vec<(String, String, i64)> =
        sqlx::query_as("SELECT id, role_json, updated_at FROM directory_roles ORDER BY id")
            .fetch_all(pool)
            .await
            .map_err(to_storage_err)?;
    snapshot.roles = roles
        .into_iter()
        .map(|(id, role_json, updated_at)| StoredDirectoryRole { id, role_json, updated_at: updated_at.max(0) as u64 })
        .collect();
    snapshot.group_members =
        sqlx::query_as("SELECT group_id, user_id FROM directory_group_members ORDER BY group_id, user_id")
            .fetch_all(pool)
            .await
            .map_err(to_storage_err)?;
    snapshot.user_roles = sqlx::query_as("SELECT user_id, role_id FROM directory_user_roles ORDER BY user_id, role_id")
        .fetch_all(pool)
        .await
        .map_err(to_storage_err)?;
    let addresses: Vec<(String, String, String, String, String, String, i64)> = sqlx::query_as(
        "SELECT owner_id,id,uri,display_name,payload_json,address_json,created_at
         FROM directory_addresses ORDER BY owner_id, id",
    )
    .fetch_all(pool)
    .await
    .map_err(to_storage_err)?;
    snapshot.addresses = addresses.into_iter().map(address_from_columns).collect();
    Ok(snapshot)
}

async fn postgres_directory_row_count(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>) -> Result<i64, StorageError> {
    sqlx::query_scalar(
        "SELECT (SELECT COUNT(*) FROM directory_profiles) + (SELECT COUNT(*) FROM directory_groups)
              + (SELECT COUNT(*) FROM directory_roles) + (SELECT COUNT(*) FROM directory_group_members)
              + (SELECT COUNT(*) FROM directory_user_roles) + (SELECT COUNT(*) FROM directory_addresses)",
    )
    .fetch_one(&mut **tx)
    .await
    .map_err(to_storage_err)
}

async fn postgres_schema_version(pool: &sqlx::PgPool) -> Result<Option<u64>, StorageError> {
    let version: Option<i64> = sqlx::query_scalar("SELECT version FROM schema_state WHERE state_key = $1")
        .bind(DIRECTORY_MARKER_KEY)
        .fetch_optional(pool)
        .await
        .map_err(to_storage_err)?;
    Ok(version.map(|value| value.max(0) as u64))
}

fn record_from_row(
    (id, room_id, created_ms, duration_sec, size_bytes, bitrate_kbps, width, height): (
        String,
        String,
        i64,
        i64,
        i64,
        i64,
        i64,
        i64,
    ),
) -> Result<RecordEntry, StorageError> {
    let room_uuid = uuid::Uuid::parse_str(&room_id)
        .map_err(|error| StorageError::Other(format!("invalid video_records.room_id {room_id}: {error}")))?;
    Ok(RecordEntry {
        id,
        room_id: RoomId::from_uuid(room_uuid),
        created_ms: created_ms.max(0) as u64,
        duration_sec: duration_sec.clamp(0, i64::from(u32::MAX)) as u32,
        size_bytes: size_bytes.max(0) as u64,
        bitrate_kbps: bitrate_kbps.clamp(0, i64::from(u32::MAX)) as u32,
        width: width.clamp(0, i64::from(u32::MAX)) as u32,
        height: height.clamp(0, i64::from(u32::MAX)) as u32,
    })
}

fn to_storage_err(err: impl std::fmt::Display) -> StorageError {
    StorageError::Other(err.to_string())
}

#[async_trait]
impl Storage for MemoryStorage {
    async fn put_configuration_document(&self, key: &str, document: &str) -> Result<(), StorageError> {
        self.configuration_documents().write().map_err(to_storage_err)?.insert(key.to_string(), document.to_string());
        Ok(())
    }

    async fn get_configuration_document(&self, key: &str) -> Result<Option<String>, StorageError> {
        Ok(self.configuration_documents().read().map_err(to_storage_err)?.get(key).cloned())
    }

    async fn load_directory_snapshot(&self) -> Result<DirectorySnapshot, StorageError> {
        Ok(self.directory_state().read().map_err(to_storage_err)?.snapshot.clone())
    }

    async fn replace_directory_snapshot(&self, snapshot: &DirectorySnapshot) -> Result<(), StorageError> {
        validate_snapshot(snapshot)?;
        self.directory_state().write().map_err(to_storage_err)?.snapshot = snapshot.clone();
        Ok(())
    }

    async fn put_configuration_document_with_directory(
        &self,
        key: &str,
        document: &str,
        snapshot: &DirectorySnapshot,
    ) -> Result<(), StorageError> {
        validate_snapshot(snapshot)?;
        // Fixed lock order (document then directory) matches the migration path.
        let mut documents = self.configuration_documents().write().map_err(to_storage_err)?;
        let mut directory = self.directory_state().write().map_err(to_storage_err)?;
        documents.insert(key.to_string(), document.to_string());
        directory.snapshot = snapshot.clone();
        Ok(())
    }

    async fn migrate_directory_document(
        &self,
        key: &str,
        expected_document: Option<&str>,
        new_document: Option<&str>,
        snapshot: &DirectorySnapshot,
    ) -> Result<DirectoryMigrationOutcome, StorageError> {
        validate_snapshot(snapshot)?;
        let mut documents = self.configuration_documents().write().map_err(to_storage_err)?;
        let mut directory = self.directory_state().write().map_err(to_storage_err)?;
        if directory.version.is_some_and(|version| version >= 1) {
            return Ok(DirectoryMigrationOutcome::AlreadyMigrated);
        }
        // Existing rows win over a legacy JSON document (lost marker must not
        // resurrect deleted records).
        if directory.snapshot.is_empty() {
            directory.snapshot = snapshot.clone();
        }
        if let Some(new_document) = new_document {
            let unchanged =
                expected_document.is_some_and(|expected| documents.get(key).map(String::as_str) == Some(expected));
            if !unchanged {
                return Ok(DirectoryMigrationOutcome::DocumentChanged);
            }
            documents.insert(key.to_string(), new_document.to_string());
        }
        directory.version = Some(1);
        Ok(DirectoryMigrationOutcome::Applied)
    }

    async fn directory_schema_version(&self) -> Result<Option<u64>, StorageError> {
        Ok(self.directory_state().read().map_err(to_storage_err)?.version)
    }

    async fn upsert_room(&self, room: &Room) -> Result<(), StorageError> {
        let id = *room.id.as_uuid();
        let mode = room.mode;
        let mut rooms = self.rooms_map().write().unwrap();
        let created_at = if let Some(existing) = rooms.get(&id) { existing.created_at } else { crate::rows::now() };
        let r = crate::rows::Room { id, name: room.name.clone(), mode, created_at };
        rooms.insert(id, r);
        Ok(())
    }

    async fn get_room(&self, id: RoomId) -> Result<Option<Room>, StorageError> {
        let rooms = self.rooms_map().read().unwrap();
        if let Some(r) = rooms.get(id.as_uuid()) {
            let created_ms = (r.created_at.as_u128() >> 80) as u64;
            let mut room = Room::new(r.name.clone(), r.mode);
            room.id = id;
            room.created_ms = created_ms;
            Ok(Some(room))
        } else {
            Ok(None)
        }
    }

    async fn delete_room(&self, id: RoomId) -> Result<(), StorageError> {
        let mut rooms = self.rooms_map().write().unwrap();
        rooms.remove(id.as_uuid());
        self.room_mode_overrides_map().write().unwrap().remove(id.as_uuid());
        Ok(())
    }

    async fn list_rooms(&self) -> Result<Vec<Room>, StorageError> {
        let rooms = self.rooms_map().read().unwrap();
        let res = rooms
            .values()
            .map(|r| {
                let created_ms = (r.created_at.as_u128() >> 80) as u64;
                let mut room = Room::new(r.name.clone(), r.mode);
                room.id = RoomId::from_uuid(r.id);
                room.created_ms = created_ms;
                room
            })
            .collect();
        Ok(res)
    }

    async fn list_room_mode_overrides(&self) -> Result<std::collections::HashMap<RoomId, RoomMode>, StorageError> {
        let overrides = self.room_mode_overrides_map().read().unwrap();
        let mut res = std::collections::HashMap::new();
        for (&uuid, &m) in overrides.iter() {
            res.insert(RoomId::from_uuid(uuid), m);
        }
        Ok(res)
    }

    async fn set_room_mode_override(&self, room_id: RoomId, mode: RoomMode) -> Result<(), StorageError> {
        self.room_mode_overrides_map().write().unwrap().insert(*room_id.as_uuid(), mode);
        Ok(())
    }

    async fn delete_room_mode_override(&self, room_id: RoomId) -> Result<(), StorageError> {
        self.room_mode_overrides_map().write().unwrap().remove(room_id.as_uuid());
        Ok(())
    }

    async fn upsert_participant(&self, p: &Participant) -> Result<(), StorageError> {
        self.participants_map().write().unwrap().insert(p.id, p.clone());
        Ok(())
    }

    async fn get_participant(&self, id: ParticipantId) -> Result<Option<Participant>, StorageError> {
        Ok(self.participants_map().read().unwrap().get(&id).cloned())
    }

    async fn delete_participant(&self, id: ParticipantId) -> Result<(), StorageError> {
        self.participants_map().write().unwrap().remove(&id);
        Ok(())
    }

    async fn list_participants(&self) -> Result<Vec<Participant>, StorageError> {
        Ok(self.participants_map().read().unwrap().values().cloned().collect())
    }

    async fn upsert_layout(&self, layout: &Layout) -> Result<(), StorageError> {
        self.layouts_map().write().unwrap().insert(layout.id.clone(), layout.clone());
        Ok(())
    }

    async fn get_layout(&self, id: &str) -> Result<Option<Layout>, StorageError> {
        Ok(self.layouts_map().read().unwrap().get(id).cloned())
    }

    async fn delete_layout(&self, id: &str) -> Result<(), StorageError> {
        self.layouts_map().write().unwrap().remove(id);
        Ok(())
    }

    async fn list_layouts(&self) -> Result<Vec<Layout>, StorageError> {
        Ok(self.layouts_map().read().unwrap().values().cloned().collect())
    }

    async fn upsert_api_key(&self, key: &ApiKey) -> Result<(), StorageError> {
        self.api_keys_map().write().unwrap().insert(key.id.clone(), key.clone());
        Ok(())
    }

    async fn get_api_key(&self, id: &str) -> Result<Option<ApiKey>, StorageError> {
        Ok(self.api_keys_map().read().unwrap().get(id).cloned())
    }

    async fn delete_api_key(&self, id: &str) -> Result<(), StorageError> {
        self.api_keys_map().write().unwrap().remove(id);
        Ok(())
    }

    async fn list_api_keys(&self) -> Result<Vec<ApiKey>, StorageError> {
        Ok(self.api_keys_map().read().unwrap().values().cloned().collect())
    }

    async fn upsert_user(&self, user: &StoredUser) -> Result<(), StorageError> {
        self.users_map().write().unwrap().insert(user.id, user.clone());
        Ok(())
    }

    async fn get_user(&self, id: UserId) -> Result<Option<StoredUser>, StorageError> {
        Ok(self.users_map().read().unwrap().get(&id).cloned())
    }

    async fn delete_user(&self, id: UserId) -> Result<(), StorageError> {
        self.users_map().write().unwrap().remove(&id);
        Ok(())
    }

    async fn list_users(&self) -> Result<Vec<StoredUser>, StorageError> {
        Ok(self.users_map().read().unwrap().values().cloned().collect())
    }

    async fn append_audit(&self, entry: &AuditEntry) -> Result<(), StorageError> {
        self.audit_log().write().unwrap().push(entry.clone());
        Ok(())
    }

    async fn list_audit(&self) -> Result<Vec<AuditEntry>, StorageError> {
        Ok(self.audit_log().read().unwrap().clone())
    }

    async fn upsert_user_credentials(&self, creds: &UserCredentials) -> Result<(), StorageError> {
        self.credentials_map().write().unwrap().insert(creds.user_id, creds.clone());
        Ok(())
    }

    async fn get_user_credentials(&self, user_id: UserId) -> Result<Option<UserCredentials>, StorageError> {
        Ok(self.credentials_map().read().unwrap().get(&user_id).cloned())
    }

    async fn list_user_credentials(&self) -> Result<Vec<UserCredentials>, StorageError> {
        Ok(self.credentials_map().read().unwrap().values().cloned().collect())
    }

    async fn delete_user_credentials(&self, user_id: UserId) -> Result<(), StorageError> {
        self.credentials_map().write().unwrap().remove(&user_id);
        Ok(())
    }

    async fn upsert_settings(&self, settings_json: &str) -> Result<(), StorageError> {
        *self.server_settings_slot().write().unwrap() = Some(settings_json.to_string());
        Ok(())
    }

    async fn get_settings(&self) -> Result<Option<String>, StorageError> {
        Ok(self.server_settings_slot().read().unwrap().clone())
    }

    async fn upsert_record(&self, record: &RecordEntry) -> Result<(), StorageError> {
        self.records_map().write().unwrap().insert(record.id.clone(), record.clone());
        Ok(())
    }

    async fn get_record(&self, id: &str) -> Result<Option<RecordEntry>, StorageError> {
        Ok(self.records_map().read().unwrap().get(id).cloned())
    }

    async fn list_records(&self, room_id: Option<RoomId>) -> Result<Vec<RecordEntry>, StorageError> {
        let records = self.records_map().read().unwrap();
        let res = records.values().filter(|r| room_id.is_none() || Some(r.room_id) == room_id).cloned().collect();
        Ok(res)
    }

    async fn delete_record(&self, id: &str) -> Result<(), StorageError> {
        self.records_map().write().unwrap().remove(id);
        Ok(())
    }

    async fn reset(&self) -> Result<(), StorageError> {
        self.configuration_documents().write().map_err(to_storage_err)?.clear();
        self.rooms_map().write().unwrap().clear();
        self.room_mode_overrides_map().write().unwrap().clear();
        self.participants_map().write().unwrap().clear();
        self.layouts_map().write().unwrap().clear();
        self.api_keys_map().write().unwrap().clear();
        self.users_map().write().unwrap().clear();
        self.credentials_map().write().unwrap().clear();
        self.records_map().write().unwrap().clear();
        self.audit_log().write().unwrap().clear();
        *self.server_settings_slot().write().unwrap() = None;
        *self.directory_state().write().map_err(to_storage_err)? = Default::default();
        Ok(())
    }
}

#[async_trait]
impl Storage for SqliteStorage {
    async fn put_configuration_document(&self, key: &str, document: &str) -> Result<(), StorageError> {
        sqlx::query("INSERT INTO configuration_documents(document_key,document_json) VALUES($1,$2) ON CONFLICT(document_key) DO UPDATE SET document_json=excluded.document_json")
            .bind(key).bind(document).execute(self.pool()).await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_configuration_document(&self, key: &str) -> Result<Option<String>, StorageError> {
        sqlx::query_scalar::<_, String>("SELECT document_json FROM configuration_documents WHERE document_key=$1")
            .bind(key)
            .fetch_optional(self.pool())
            .await
            .map_err(to_storage_err)
    }

    // --- Directory / address book (T10) ---

    async fn load_directory_snapshot(&self) -> Result<DirectorySnapshot, StorageError> {
        sqlite_load_snapshot(self.pool()).await
    }

    async fn replace_directory_snapshot(&self, snapshot: &DirectorySnapshot) -> Result<(), StorageError> {
        let mut tx = self.pool().begin().await.map_err(to_storage_err)?;
        sqlite_replace_snapshot_tx(&mut tx, snapshot).await?;
        tx.commit().await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn put_configuration_document_with_directory(
        &self,
        key: &str,
        document: &str,
        snapshot: &DirectorySnapshot,
    ) -> Result<(), StorageError> {
        validate_snapshot(snapshot)?;
        let mut tx = self.pool().begin().await.map_err(to_storage_err)?;
        sqlx::query("INSERT INTO configuration_documents(document_key,document_json) VALUES($1,$2) ON CONFLICT(document_key) DO UPDATE SET document_json=excluded.document_json")
            .bind(key).bind(document).execute(&mut *tx).await.map_err(to_storage_err)?;
        sqlite_replace_snapshot_tx(&mut tx, snapshot).await?;
        tx.commit().await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn migrate_directory_document(
        &self,
        key: &str,
        expected_document: Option<&str>,
        new_document: Option<&str>,
        snapshot: &DirectorySnapshot,
    ) -> Result<DirectoryMigrationOutcome, StorageError> {
        validate_snapshot(snapshot)?;
        let mut tx = self.pool().begin().await.map_err(to_storage_err)?;
        // Step 1: reserve the marker. Owning the row serializes concurrent
        // migrators (SQLite write lock / PG unique-index lock).
        let owned = sqlx::query(
            "INSERT INTO schema_state(state_key, version, updated_at) VALUES($1,$2,$3)
             ON CONFLICT(state_key) DO UPDATE SET version=excluded.version, updated_at=excluded.updated_at
             WHERE schema_state.version < excluded.version",
        )
        .bind(DIRECTORY_MARKER_KEY)
        .bind(DIRECTORY_MARKER_VERSION)
        .bind(unix_now_secs())
        .execute(&mut *tx)
        .await
        .map_err(to_storage_err)?
        .rows_affected()
            == 1;
        if !owned {
            tx.rollback().await.map_err(to_storage_err)?;
            return Ok(DirectoryMigrationOutcome::AlreadyMigrated);
        }
        // Step 2: adopt non-empty SQL rows instead of importing stale JSON.
        let existing = sqlite_directory_row_count(&mut tx).await?;
        if existing == 0 {
            sqlite_replace_snapshot_tx(&mut tx, snapshot).await?;
        }
        // Step 3: CAS the document; never overwrite a concurrent writer.
        if let Some(new_document) = new_document {
            let Some(expected) = expected_document else {
                tx.rollback().await.map_err(to_storage_err)?;
                return Ok(DirectoryMigrationOutcome::DocumentChanged);
            };
            let changed = sqlx::query(
                "UPDATE configuration_documents SET document_json=$1 WHERE document_key=$2 AND document_json=$3",
            )
            .bind(new_document)
            .bind(key)
            .bind(expected)
            .execute(&mut *tx)
            .await
            .map_err(to_storage_err)?
            .rows_affected();
            if changed == 0 {
                tx.rollback().await.map_err(to_storage_err)?;
                return Ok(DirectoryMigrationOutcome::DocumentChanged);
            }
        }
        tx.commit().await.map_err(to_storage_err)?;
        Ok(DirectoryMigrationOutcome::Applied)
    }

    async fn directory_schema_version(&self) -> Result<Option<u64>, StorageError> {
        sqlite_schema_version(self.pool()).await
    }

    async fn ping(&self) -> Result<(), StorageError> {
        sqlx::query("SELECT 1").execute(self.pool()).await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn upsert_room(&self, room: &Room) -> Result<(), StorageError> {
        let id = *room.id.as_uuid();
        let name = &room.name;
        let mode = room.mode.as_str();
        let created_at = crate::rows::now();

        sqlx::query(
            "INSERT INTO rooms (id, name, mode, created_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET name = $2, mode = $3",
        )
        .bind(id)
        .bind(name)
        .bind(mode)
        .bind(created_at)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;

        Ok(())
    }

    async fn get_room(&self, id: RoomId) -> Result<Option<Room>, StorageError> {
        let uuid = *id.as_uuid();
        match self.get_room(uuid).await {
            Ok(r) => {
                let created_ms = (r.created_at.as_u128() >> 80) as u64;
                let mut room = Room::new(r.name, r.mode);
                room.id = id;
                room.created_ms = created_ms;
                Ok(Some(room))
            }
            Err(crate::Error::NotFound(_)) => Ok(None),
            Err(e) => Err(to_storage_err(e)),
        }
    }

    async fn delete_room(&self, id: RoomId) -> Result<(), StorageError> {
        let uuid = *id.as_uuid();
        let _ = self.delete_room_mode_override(uuid).await;
        match self.delete_room(uuid).await {
            Ok(()) | Err(crate::Error::NotFound(_)) => Ok(()),
            Err(e) => Err(to_storage_err(e)),
        }
    }

    async fn list_rooms(&self) -> Result<Vec<Room>, StorageError> {
        let rooms = self.list_rooms().await.map_err(to_storage_err)?;
        Ok(rooms
            .into_iter()
            .map(|r| {
                let created_ms = (r.created_at.as_u128() >> 80) as u64;
                let mut room = Room::new(r.name, r.mode);
                room.id = RoomId::from_uuid(r.id);
                room.created_ms = created_ms;
                room
            })
            .collect())
    }

    async fn list_room_mode_overrides(&self) -> Result<std::collections::HashMap<RoomId, RoomMode>, StorageError> {
        let list = self.list_room_mode_overrides().await.map_err(to_storage_err)?;
        let mut res = std::collections::HashMap::new();
        for (uuid, m) in list {
            res.insert(RoomId::from_uuid(uuid), m);
        }
        Ok(res)
    }

    async fn set_room_mode_override(&self, room_id: RoomId, mode: RoomMode) -> Result<(), StorageError> {
        self.set_room_mode_override(*room_id.as_uuid(), mode).await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn delete_room_mode_override(&self, room_id: RoomId) -> Result<(), StorageError> {
        self.delete_room_mode_override(*room_id.as_uuid()).await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn upsert_participant(&self, p: &Participant) -> Result<(), StorageError> {
        self.participants_map().write().unwrap().insert(p.id, p.clone());
        Ok(())
    }

    async fn get_participant(&self, id: ParticipantId) -> Result<Option<Participant>, StorageError> {
        Ok(self.participants_map().read().unwrap().get(&id).cloned())
    }

    async fn delete_participant(&self, id: ParticipantId) -> Result<(), StorageError> {
        self.participants_map().write().unwrap().remove(&id);
        Ok(())
    }

    async fn list_participants(&self) -> Result<Vec<Participant>, StorageError> {
        Ok(self.participants_map().read().unwrap().values().cloned().collect())
    }

    async fn upsert_layout(&self, layout: &Layout) -> Result<(), StorageError> {
        let assigned_rooms_json = serde_json::to_string(&layout.assigned_rooms).map_err(to_storage_err)?;
        sqlx::query(
            "INSERT INTO core_layouts (id, name, version, assigned_rooms, is_default)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (id) DO UPDATE SET name = $2, version = $3, assigned_rooms = $4, is_default = $5",
        )
        .bind(&layout.id)
        .bind(&layout.name)
        .bind(layout.version as i32)
        .bind(&assigned_rooms_json)
        .bind(layout.is_default)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_layout(&self, id: &str) -> Result<Option<Layout>, StorageError> {
        let row: Option<(String, String, i32, String, bool)> =
            sqlx::query_as("SELECT id, name, version, assigned_rooms, is_default FROM core_layouts WHERE id = $1")
                .bind(id)
                .fetch_optional(self.pool())
                .await
                .map_err(to_storage_err)?;

        match row {
            Some((id, name, version, assigned_rooms, is_default)) => {
                let assigned_rooms = serde_json::from_str(&assigned_rooms).unwrap_or_default();
                Ok(Some(Layout { id, name, version: version as u32, assigned_rooms, is_default }))
            }
            None => Ok(None),
        }
    }

    async fn delete_layout(&self, id: &str) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM core_layouts WHERE id = $1")
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(to_storage_err)?;
        Ok(())
    }

    async fn list_layouts(&self) -> Result<Vec<Layout>, StorageError> {
        let rows: Vec<(String, String, i32, String, bool)> =
            sqlx::query_as("SELECT id, name, version, assigned_rooms, is_default FROM core_layouts")
                .fetch_all(self.pool())
                .await
                .map_err(to_storage_err)?;

        let mut res = Vec::with_capacity(rows.len());
        for (id, name, version, assigned_rooms, is_default) in rows {
            let assigned_rooms = serde_json::from_str(&assigned_rooms).unwrap_or_default();
            res.push(Layout { id, name, version: version as u32, assigned_rooms, is_default });
        }
        Ok(res)
    }

    async fn upsert_api_key(&self, key: &ApiKey) -> Result<(), StorageError> {
        let scopes_json = serde_json::to_string(&key.scopes).map_err(to_storage_err)?;
        let owner_str = key.owner.to_string();
        sqlx::query(
            "INSERT INTO core_api_keys (id, scopes, owner, revoked)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET scopes = $2, owner = $3, revoked = $4",
        )
        .bind(&key.id)
        .bind(&scopes_json)
        .bind(&owner_str)
        .bind(key.revoked)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_api_key(&self, id: &str) -> Result<Option<ApiKey>, StorageError> {
        let row: Option<(String, String, String, bool)> =
            sqlx::query_as("SELECT id, scopes, owner, revoked FROM core_api_keys WHERE id = $1")
                .bind(id)
                .fetch_optional(self.pool())
                .await
                .map_err(to_storage_err)?;

        match row {
            Some((id, scopes, owner, revoked)) => {
                let scopes = serde_json::from_str(&scopes).unwrap_or_default();
                let owner = match owner.parse::<vks_core::participant::UserId>() {
                    Ok(u) => u,
                    Err(_) => return Ok(None),
                };
                Ok(Some(ApiKey { id, scopes, owner, revoked }))
            }
            None => Ok(None),
        }
    }

    async fn delete_api_key(&self, id: &str) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM core_api_keys WHERE id = $1")
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(to_storage_err)?;
        Ok(())
    }

    async fn list_api_keys(&self) -> Result<Vec<ApiKey>, StorageError> {
        let rows: Vec<(String, String, String, bool)> =
            sqlx::query_as("SELECT id, scopes, owner, revoked FROM core_api_keys")
                .fetch_all(self.pool())
                .await
                .map_err(to_storage_err)?;

        let mut res = Vec::with_capacity(rows.len());
        for (id, scopes, owner, revoked) in rows {
            let scopes = serde_json::from_str(&scopes).unwrap_or_default();
            if let Ok(owner) = owner.parse::<vks_core::participant::UserId>() {
                res.push(ApiKey { id, scopes, owner, revoked });
            }
        }
        Ok(res)
    }

    async fn upsert_user(&self, user: &StoredUser) -> Result<(), StorageError> {
        let id_str = user.id.to_string();
        let role_str = user.role.as_str();
        sqlx::query(
            "INSERT INTO core_users (id, username, role, muted, deleted)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (id) DO UPDATE SET username = $2, role = $3, muted = $4, deleted = $5",
        )
        .bind(&id_str)
        .bind(&user.username)
        .bind(role_str)
        .bind(user.muted)
        .bind(user.deleted)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_user(&self, id: UserId) -> Result<Option<StoredUser>, StorageError> {
        let id_str = id.to_string();
        let row: Option<(String, String, String, bool, bool)> =
            sqlx::query_as("SELECT id, username, role, muted, deleted FROM core_users WHERE id = $1")
                .bind(&id_str)
                .fetch_optional(self.pool())
                .await
                .map_err(to_storage_err)?;

        match row {
            Some((id_str, username, role_str, muted, deleted)) => {
                let id = UserId::from_str(&id_str).map_err(to_storage_err)?;
                let role = match role_str.as_str() {
                    "operator" => vks_core::Role::Operator,
                    "moderator" => vks_core::Role::Moderator,
                    "viewer" => vks_core::Role::Viewer,
                    _ => vks_core::Role::Participant,
                };
                Ok(Some(StoredUser { id, username, role, muted, deleted }))
            }
            None => Ok(None),
        }
    }

    async fn delete_user(&self, id: UserId) -> Result<(), StorageError> {
        let id_str = id.to_string();
        sqlx::query("DELETE FROM core_users WHERE id = $1")
            .bind(&id_str)
            .execute(self.pool())
            .await
            .map_err(to_storage_err)?;
        Ok(())
    }

    async fn list_users(&self) -> Result<Vec<StoredUser>, StorageError> {
        let rows: Vec<(String, String, String, bool, bool)> =
            sqlx::query_as("SELECT id, username, role, muted, deleted FROM core_users ORDER BY username")
                .fetch_all(self.pool())
                .await
                .map_err(to_storage_err)?;

        let mut res = Vec::with_capacity(rows.len());
        for (id_str, username, role_str, muted, deleted) in rows {
            if let Ok(id) = UserId::from_str(&id_str) {
                let role = match role_str.as_str() {
                    "operator" => vks_core::Role::Operator,
                    "moderator" => vks_core::Role::Moderator,
                    "viewer" => vks_core::Role::Viewer,
                    _ => vks_core::Role::Participant,
                };
                res.push(StoredUser { id, username, role, muted, deleted });
            }
        }
        Ok(res)
    }

    async fn append_audit(&self, entry: &AuditEntry) -> Result<(), StorageError> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO core_audit_log (id, ts_ms, user_name, action, room, ip)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&id)
        .bind(entry.ts_ms as i64)
        .bind(&entry.user)
        .bind(&entry.action)
        .bind(entry.room.as_deref())
        .bind(&entry.ip)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn list_audit(&self) -> Result<Vec<AuditEntry>, StorageError> {
        let rows: Vec<(i64, String, String, Option<String>, String)> =
            sqlx::query_as("SELECT ts_ms, user_name, action, room, ip FROM core_audit_log ORDER BY ts_ms ASC")
                .fetch_all(self.pool())
                .await
                .map_err(to_storage_err)?;

        Ok(rows
            .into_iter()
            .map(|(ts_ms, user, action, room, ip)| AuditEntry { ts_ms: ts_ms as u64, user, action, room, ip })
            .collect())
    }

    async fn upsert_user_credentials(&self, creds: &UserCredentials) -> Result<(), StorageError> {
        let id_str = creds.user_id.to_string();
        sqlx::query(
            "INSERT INTO user_credentials (user_id, password_hash_hex, salt_hex, password_changed_at, generation)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (user_id) DO UPDATE SET password_hash_hex = $2, salt_hex = $3, password_changed_at = $4, generation = $5",
        )
        .bind(&id_str)
        .bind(&creds.password_hash_hex)
        .bind(&creds.salt_hex)
        .bind(creds.password_changed_at)
        .bind(creds.generation as i64)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_user_credentials(&self, user_id: UserId) -> Result<Option<UserCredentials>, StorageError> {
        let id_str = user_id.to_string();
        let row: Option<(String, String, String, Option<i64>, i64)> = sqlx::query_as(
            "SELECT user_id, password_hash_hex, salt_hex, password_changed_at, generation FROM user_credentials WHERE user_id = $1",
        )
        .bind(&id_str)
        .fetch_optional(self.pool())
        .await
        .map_err(to_storage_err)?;

        match row {
            Some((_, password_hash_hex, salt_hex, password_changed_at, generation)) => Ok(Some(UserCredentials {
                user_id,
                password_hash_hex,
                salt_hex,
                password_changed_at,
                generation: generation as u64,
            })),
            None => Ok(None),
        }
    }

    async fn list_user_credentials(&self) -> Result<Vec<UserCredentials>, StorageError> {
        let rows: Vec<(String, String, String, Option<i64>, i64)> = sqlx::query_as(
            "SELECT user_id, password_hash_hex, salt_hex, password_changed_at, generation FROM user_credentials",
        )
        .fetch_all(self.pool())
        .await
        .map_err(to_storage_err)?;

        let mut res = Vec::with_capacity(rows.len());
        for (id_str, password_hash_hex, salt_hex, password_changed_at, generation) in rows {
            if let Ok(uid) = UserId::from_str(&id_str) {
                res.push(UserCredentials {
                    user_id: uid,
                    password_hash_hex,
                    salt_hex,
                    password_changed_at,
                    generation: generation as u64,
                });
            }
        }
        Ok(res)
    }

    async fn delete_user_credentials(&self, user_id: UserId) -> Result<(), StorageError> {
        let id_str = user_id.to_string();
        sqlx::query("DELETE FROM user_credentials WHERE user_id = $1")
            .bind(&id_str)
            .execute(self.pool())
            .await
            .map_err(to_storage_err)?;
        Ok(())
    }

    async fn upsert_settings(&self, settings_json: &str) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO server_settings (id, data_json, updated_at)
             VALUES ('current', $1, datetime('now'))
             ON CONFLICT (id) DO UPDATE SET data_json = $1, updated_at = datetime('now')",
        )
        .bind(settings_json)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_settings(&self) -> Result<Option<String>, StorageError> {
        let row: Option<(String,)> = sqlx::query_as("SELECT data_json FROM server_settings WHERE id = 'current'")
            .fetch_optional(self.pool())
            .await
            .map_err(to_storage_err)?;

        Ok(row.map(|(data,)| data))
    }

    // --- Records (T09a) ---

    async fn upsert_record(&self, record: &RecordEntry) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO video_records(id, room_id, created_ms, duration_sec, size_bytes, bitrate_kbps, width, height)
             VALUES($1,$2,$3,$4,$5,$6,$7,$8)
             ON CONFLICT(id) DO UPDATE SET room_id=excluded.room_id, created_ms=excluded.created_ms,
                 duration_sec=excluded.duration_sec, size_bytes=excluded.size_bytes,
                 bitrate_kbps=excluded.bitrate_kbps, width=excluded.width, height=excluded.height",
        )
        .bind(&record.id)
        .bind(record.room_id.to_string())
        .bind(record.created_ms as i64)
        .bind(i64::from(record.duration_sec))
        .bind(record.size_bytes as i64)
        .bind(i64::from(record.bitrate_kbps))
        .bind(i64::from(record.width))
        .bind(i64::from(record.height))
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_record(&self, id: &str) -> Result<Option<RecordEntry>, StorageError> {
        let row: Option<(String, String, i64, i64, i64, i64, i64, i64)> = sqlx::query_as(
            "SELECT id, room_id, created_ms, duration_sec, size_bytes, bitrate_kbps, width, height
             FROM video_records WHERE id=$1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(row.map(record_from_row).transpose()?)
    }

    async fn list_records(&self, room_id: Option<RoomId>) -> Result<Vec<RecordEntry>, StorageError> {
        let rows: Vec<(String, String, i64, i64, i64, i64, i64, i64)> = match room_id {
            Some(room) => {
                sqlx::query_as(
                    "SELECT id, room_id, created_ms, duration_sec, size_bytes, bitrate_kbps, width, height
                     FROM video_records WHERE room_id=$1 ORDER BY created_ms",
                )
                .bind(room.to_string())
                .fetch_all(self.pool())
                .await
            }
            None => {
                sqlx::query_as(
                    "SELECT id, room_id, created_ms, duration_sec, size_bytes, bitrate_kbps, width, height
                     FROM video_records ORDER BY created_ms",
                )
                .fetch_all(self.pool())
                .await
            }
        }
        .map_err(to_storage_err)?;
        rows.into_iter().map(record_from_row).collect()
    }

    async fn delete_record(&self, id: &str) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM video_records WHERE id=$1")
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(to_storage_err)?;
        Ok(())
    }

    async fn reset(&self) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM room_mode_override").execute(self.pool()).await.map_err(to_storage_err)?;
        // T10: test reset must also clear the directory rows AND the marker,
        // otherwise the next migration would think it already ran (P1-4).
        sqlx::query("DELETE FROM directory_group_members").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM directory_user_roles").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM directory_addresses").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM directory_profiles").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM directory_groups").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM directory_roles").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM schema_state").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM configuration_documents").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM core_layouts").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM core_api_keys").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM core_users").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM user_credentials").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM video_records").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM core_audit_log").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM server_settings").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM layouts").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM api_keys").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM rooms").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM users").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM audit_log").execute(self.pool()).await.map_err(to_storage_err)?;
        self.participants_map().write().unwrap().clear();
        Ok(())
    }
}

#[async_trait]
impl Storage for PostgresStorage {
    async fn put_configuration_document(&self, key: &str, document: &str) -> Result<(), StorageError> {
        sqlx::query("INSERT INTO configuration_documents(document_key,document_json) VALUES($1,$2) ON CONFLICT(document_key) DO UPDATE SET document_json=excluded.document_json")
            .bind(key).bind(document).execute(self.pool()).await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_configuration_document(&self, key: &str) -> Result<Option<String>, StorageError> {
        sqlx::query_scalar::<_, String>("SELECT document_json FROM configuration_documents WHERE document_key=$1")
            .bind(key)
            .fetch_optional(self.pool())
            .await
            .map_err(to_storage_err)
    }

    // --- Directory / address book (T10) ---

    async fn load_directory_snapshot(&self) -> Result<DirectorySnapshot, StorageError> {
        postgres_load_snapshot(self.pool()).await
    }

    async fn replace_directory_snapshot(&self, snapshot: &DirectorySnapshot) -> Result<(), StorageError> {
        let mut tx = self.pool().begin().await.map_err(to_storage_err)?;
        postgres_replace_snapshot_tx(&mut tx, snapshot).await?;
        tx.commit().await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn put_configuration_document_with_directory(
        &self,
        key: &str,
        document: &str,
        snapshot: &DirectorySnapshot,
    ) -> Result<(), StorageError> {
        validate_snapshot(snapshot)?;
        let mut tx = self.pool().begin().await.map_err(to_storage_err)?;
        sqlx::query("INSERT INTO configuration_documents(document_key,document_json) VALUES($1,$2) ON CONFLICT(document_key) DO UPDATE SET document_json=excluded.document_json")
            .bind(key).bind(document).execute(&mut *tx).await.map_err(to_storage_err)?;
        postgres_replace_snapshot_tx(&mut tx, snapshot).await?;
        tx.commit().await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn migrate_directory_document(
        &self,
        key: &str,
        expected_document: Option<&str>,
        new_document: Option<&str>,
        snapshot: &DirectorySnapshot,
    ) -> Result<DirectoryMigrationOutcome, StorageError> {
        validate_snapshot(snapshot)?;
        let mut tx = self.pool().begin().await.map_err(to_storage_err)?;
        let owned = sqlx::query(
            "INSERT INTO schema_state(state_key, version, updated_at) VALUES($1,$2,$3)
             ON CONFLICT(state_key) DO UPDATE SET version=excluded.version, updated_at=excluded.updated_at
             WHERE schema_state.version < excluded.version",
        )
        .bind(DIRECTORY_MARKER_KEY)
        .bind(DIRECTORY_MARKER_VERSION)
        .bind(unix_now_secs())
        .execute(&mut *tx)
        .await
        .map_err(to_storage_err)?
        .rows_affected()
            == 1;
        if !owned {
            tx.rollback().await.map_err(to_storage_err)?;
            return Ok(DirectoryMigrationOutcome::AlreadyMigrated);
        }
        let existing = postgres_directory_row_count(&mut tx).await?;
        if existing == 0 {
            postgres_replace_snapshot_tx(&mut tx, snapshot).await?;
        }
        if let Some(new_document) = new_document {
            let Some(expected) = expected_document else {
                tx.rollback().await.map_err(to_storage_err)?;
                return Ok(DirectoryMigrationOutcome::DocumentChanged);
            };
            let changed = sqlx::query(
                "UPDATE configuration_documents SET document_json=$1 WHERE document_key=$2 AND document_json=$3",
            )
            .bind(new_document)
            .bind(key)
            .bind(expected)
            .execute(&mut *tx)
            .await
            .map_err(to_storage_err)?
            .rows_affected();
            if changed == 0 {
                tx.rollback().await.map_err(to_storage_err)?;
                return Ok(DirectoryMigrationOutcome::DocumentChanged);
            }
        }
        tx.commit().await.map_err(to_storage_err)?;
        Ok(DirectoryMigrationOutcome::Applied)
    }

    async fn directory_schema_version(&self) -> Result<Option<u64>, StorageError> {
        postgres_schema_version(self.pool()).await
    }

    async fn ping(&self) -> Result<(), StorageError> {
        sqlx::query("SELECT 1").execute(self.pool()).await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn upsert_room(&self, room: &Room) -> Result<(), StorageError> {
        let id = *room.id.as_uuid();
        let name = &room.name;
        let mode = room.mode.as_str();
        let created_at = crate::rows::now();

        sqlx::query(
            "INSERT INTO rooms (id, name, mode, created_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET name = $2, mode = $3",
        )
        .bind(id)
        .bind(name)
        .bind(mode)
        .bind(created_at)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;

        Ok(())
    }

    async fn get_room(&self, id: RoomId) -> Result<Option<Room>, StorageError> {
        let uuid = *id.as_uuid();
        match self.get_room(uuid).await {
            Ok(r) => {
                let created_ms = (r.created_at.as_u128() >> 80) as u64;
                let mut room = Room::new(r.name, r.mode);
                room.id = id;
                room.created_ms = created_ms;
                Ok(Some(room))
            }
            Err(crate::Error::NotFound(_)) => Ok(None),
            Err(e) => Err(to_storage_err(e)),
        }
    }

    async fn delete_room(&self, id: RoomId) -> Result<(), StorageError> {
        let uuid = *id.as_uuid();
        let _ = self.delete_room_mode_override(uuid).await;
        match self.delete_room(uuid).await {
            Ok(()) | Err(crate::Error::NotFound(_)) => Ok(()),
            Err(e) => Err(to_storage_err(e)),
        }
    }

    async fn list_rooms(&self) -> Result<Vec<Room>, StorageError> {
        let rooms = self.list_rooms().await.map_err(to_storage_err)?;
        Ok(rooms
            .into_iter()
            .map(|r| {
                let created_ms = (r.created_at.as_u128() >> 80) as u64;
                let mut room = Room::new(r.name, r.mode);
                room.id = RoomId::from_uuid(r.id);
                room.created_ms = created_ms;
                room
            })
            .collect())
    }

    async fn list_room_mode_overrides(&self) -> Result<std::collections::HashMap<RoomId, RoomMode>, StorageError> {
        let list = self.list_room_mode_overrides().await.map_err(to_storage_err)?;
        let mut res = std::collections::HashMap::new();
        for (uuid, m) in list {
            res.insert(RoomId::from_uuid(uuid), m);
        }
        Ok(res)
    }

    async fn set_room_mode_override(&self, room_id: RoomId, mode: RoomMode) -> Result<(), StorageError> {
        self.set_room_mode_override(*room_id.as_uuid(), mode).await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn delete_room_mode_override(&self, room_id: RoomId) -> Result<(), StorageError> {
        self.delete_room_mode_override(*room_id.as_uuid()).await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn upsert_participant(&self, p: &Participant) -> Result<(), StorageError> {
        self.participants_map().write().unwrap().insert(p.id, p.clone());
        Ok(())
    }

    async fn get_participant(&self, id: ParticipantId) -> Result<Option<Participant>, StorageError> {
        Ok(self.participants_map().read().unwrap().get(&id).cloned())
    }

    async fn delete_participant(&self, id: ParticipantId) -> Result<(), StorageError> {
        self.participants_map().write().unwrap().remove(&id);
        Ok(())
    }

    async fn list_participants(&self) -> Result<Vec<Participant>, StorageError> {
        Ok(self.participants_map().read().unwrap().values().cloned().collect())
    }

    async fn upsert_layout(&self, layout: &Layout) -> Result<(), StorageError> {
        let assigned_rooms_json = serde_json::to_string(&layout.assigned_rooms).map_err(to_storage_err)?;
        sqlx::query(
            "INSERT INTO core_layouts (id, name, version, assigned_rooms, is_default)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (id) DO UPDATE SET name = $2, version = $3, assigned_rooms = $4, is_default = $5",
        )
        .bind(&layout.id)
        .bind(&layout.name)
        .bind(layout.version as i32)
        .bind(&assigned_rooms_json)
        .bind(layout.is_default)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_layout(&self, id: &str) -> Result<Option<Layout>, StorageError> {
        let row: Option<(String, String, i32, String, bool)> =
            sqlx::query_as("SELECT id, name, version, assigned_rooms, is_default FROM core_layouts WHERE id = $1")
                .bind(id)
                .fetch_optional(self.pool())
                .await
                .map_err(to_storage_err)?;

        match row {
            Some((id, name, version, assigned_rooms, is_default)) => {
                let assigned_rooms = serde_json::from_str(&assigned_rooms).unwrap_or_default();
                Ok(Some(Layout { id, name, version: version as u32, assigned_rooms, is_default }))
            }
            None => Ok(None),
        }
    }

    async fn delete_layout(&self, id: &str) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM core_layouts WHERE id = $1")
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(to_storage_err)?;
        Ok(())
    }

    async fn list_layouts(&self) -> Result<Vec<Layout>, StorageError> {
        let rows: Vec<(String, String, i32, String, bool)> =
            sqlx::query_as("SELECT id, name, version, assigned_rooms, is_default FROM core_layouts")
                .fetch_all(self.pool())
                .await
                .map_err(to_storage_err)?;

        let mut res = Vec::with_capacity(rows.len());
        for (id, name, version, assigned_rooms, is_default) in rows {
            let assigned_rooms = serde_json::from_str(&assigned_rooms).unwrap_or_default();
            res.push(Layout { id, name, version: version as u32, assigned_rooms, is_default });
        }
        Ok(res)
    }

    async fn upsert_api_key(&self, key: &ApiKey) -> Result<(), StorageError> {
        let scopes_json = serde_json::to_string(&key.scopes).map_err(to_storage_err)?;
        let owner_str = key.owner.to_string();
        sqlx::query(
            "INSERT INTO core_api_keys (id, scopes, owner, revoked)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET scopes = $2, owner = $3, revoked = $4",
        )
        .bind(&key.id)
        .bind(&scopes_json)
        .bind(&owner_str)
        .bind(key.revoked)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_api_key(&self, id: &str) -> Result<Option<ApiKey>, StorageError> {
        let row: Option<(String, String, String, bool)> =
            sqlx::query_as("SELECT id, scopes, owner, revoked FROM core_api_keys WHERE id = $1")
                .bind(id)
                .fetch_optional(self.pool())
                .await
                .map_err(to_storage_err)?;

        match row {
            Some((id, scopes, owner, revoked)) => {
                let scopes = serde_json::from_str(&scopes).unwrap_or_default();
                let owner = match owner.parse::<vks_core::participant::UserId>() {
                    Ok(u) => u,
                    Err(_) => return Ok(None),
                };
                Ok(Some(ApiKey { id, scopes, owner, revoked }))
            }
            None => Ok(None),
        }
    }

    async fn delete_api_key(&self, id: &str) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM core_api_keys WHERE id = $1")
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(to_storage_err)?;
        Ok(())
    }

    async fn list_api_keys(&self) -> Result<Vec<ApiKey>, StorageError> {
        let rows: Vec<(String, String, String, bool)> =
            sqlx::query_as("SELECT id, scopes, owner, revoked FROM core_api_keys")
                .fetch_all(self.pool())
                .await
                .map_err(to_storage_err)?;

        let mut res = Vec::with_capacity(rows.len());
        for (id, scopes, owner, revoked) in rows {
            let scopes = serde_json::from_str(&scopes).unwrap_or_default();
            if let Ok(owner) = owner.parse::<vks_core::participant::UserId>() {
                res.push(ApiKey { id, scopes, owner, revoked });
            }
        }
        Ok(res)
    }

    async fn upsert_user(&self, user: &StoredUser) -> Result<(), StorageError> {
        let id_str = user.id.to_string();
        let role_str = user.role.as_str();
        sqlx::query(
            "INSERT INTO core_users (id, username, role, muted, deleted)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (id) DO UPDATE SET username = $2, role = $3, muted = $4, deleted = $5",
        )
        .bind(&id_str)
        .bind(&user.username)
        .bind(role_str)
        .bind(user.muted)
        .bind(user.deleted)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_user(&self, id: UserId) -> Result<Option<StoredUser>, StorageError> {
        let id_str = id.to_string();
        let row: Option<(String, String, String, bool, bool)> =
            sqlx::query_as("SELECT id, username, role, muted, deleted FROM core_users WHERE id = $1")
                .bind(&id_str)
                .fetch_optional(self.pool())
                .await
                .map_err(to_storage_err)?;

        match row {
            Some((id_str, username, role_str, muted, deleted)) => {
                let id = UserId::from_str(&id_str).map_err(to_storage_err)?;
                let role = match role_str.as_str() {
                    "operator" => vks_core::Role::Operator,
                    "moderator" => vks_core::Role::Moderator,
                    "viewer" => vks_core::Role::Viewer,
                    _ => vks_core::Role::Participant,
                };
                Ok(Some(StoredUser { id, username, role, muted, deleted }))
            }
            None => Ok(None),
        }
    }

    async fn delete_user(&self, id: UserId) -> Result<(), StorageError> {
        let id_str = id.to_string();
        sqlx::query("DELETE FROM core_users WHERE id = $1")
            .bind(&id_str)
            .execute(self.pool())
            .await
            .map_err(to_storage_err)?;
        Ok(())
    }

    async fn list_users(&self) -> Result<Vec<StoredUser>, StorageError> {
        let rows: Vec<(String, String, String, bool, bool)> =
            sqlx::query_as("SELECT id, username, role, muted, deleted FROM core_users ORDER BY username")
                .fetch_all(self.pool())
                .await
                .map_err(to_storage_err)?;

        let mut res = Vec::with_capacity(rows.len());
        for (id_str, username, role_str, muted, deleted) in rows {
            if let Ok(id) = UserId::from_str(&id_str) {
                let role = match role_str.as_str() {
                    "operator" => vks_core::Role::Operator,
                    "moderator" => vks_core::Role::Moderator,
                    "viewer" => vks_core::Role::Viewer,
                    _ => vks_core::Role::Participant,
                };
                res.push(StoredUser { id, username, role, muted, deleted });
            }
        }
        Ok(res)
    }

    async fn append_audit(&self, entry: &AuditEntry) -> Result<(), StorageError> {
        let id = uuid::Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO core_audit_log (id, ts_ms, user_name, action, room, ip)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&id)
        .bind(entry.ts_ms as i64)
        .bind(&entry.user)
        .bind(&entry.action)
        .bind(entry.room.as_deref())
        .bind(&entry.ip)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn list_audit(&self) -> Result<Vec<AuditEntry>, StorageError> {
        let rows: Vec<(i64, String, String, Option<String>, String)> =
            sqlx::query_as("SELECT ts_ms, user_name, action, room, ip FROM core_audit_log ORDER BY ts_ms ASC")
                .fetch_all(self.pool())
                .await
                .map_err(to_storage_err)?;

        Ok(rows
            .into_iter()
            .map(|(ts_ms, user, action, room, ip)| AuditEntry { ts_ms: ts_ms as u64, user, action, room, ip })
            .collect())
    }

    async fn upsert_user_credentials(&self, creds: &UserCredentials) -> Result<(), StorageError> {
        let id_str = creds.user_id.to_string();
        sqlx::query(
            "INSERT INTO user_credentials (user_id, password_hash_hex, salt_hex, password_changed_at, generation)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (user_id) DO UPDATE SET password_hash_hex = $2, salt_hex = $3, password_changed_at = $4, generation = $5",
        )
        .bind(&id_str)
        .bind(&creds.password_hash_hex)
        .bind(&creds.salt_hex)
        .bind(creds.password_changed_at)
        .bind(creds.generation as i64)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_user_credentials(&self, user_id: UserId) -> Result<Option<UserCredentials>, StorageError> {
        let id_str = user_id.to_string();
        let row: Option<(String, String, String, Option<i64>, i64)> = sqlx::query_as(
            "SELECT user_id, password_hash_hex, salt_hex, password_changed_at, generation FROM user_credentials WHERE user_id = $1",
        )
        .bind(&id_str)
        .fetch_optional(self.pool())
        .await
        .map_err(to_storage_err)?;

        match row {
            Some((_, password_hash_hex, salt_hex, password_changed_at, generation)) => Ok(Some(UserCredentials {
                user_id,
                password_hash_hex,
                salt_hex,
                password_changed_at,
                generation: generation as u64,
            })),
            None => Ok(None),
        }
    }

    async fn list_user_credentials(&self) -> Result<Vec<UserCredentials>, StorageError> {
        let rows: Vec<(String, String, String, Option<i64>, i64)> = sqlx::query_as(
            "SELECT user_id, password_hash_hex, salt_hex, password_changed_at, generation FROM user_credentials",
        )
        .fetch_all(self.pool())
        .await
        .map_err(to_storage_err)?;

        let mut res = Vec::with_capacity(rows.len());
        for (id_str, password_hash_hex, salt_hex, password_changed_at, generation) in rows {
            if let Ok(uid) = UserId::from_str(&id_str) {
                res.push(UserCredentials {
                    user_id: uid,
                    password_hash_hex,
                    salt_hex,
                    password_changed_at,
                    generation: generation as u64,
                });
            }
        }
        Ok(res)
    }

    async fn delete_user_credentials(&self, user_id: UserId) -> Result<(), StorageError> {
        let id_str = user_id.to_string();
        sqlx::query("DELETE FROM user_credentials WHERE user_id = $1")
            .bind(&id_str)
            .execute(self.pool())
            .await
            .map_err(to_storage_err)?;
        Ok(())
    }

    async fn upsert_settings(&self, settings_json: &str) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO server_settings (id, data_json, updated_at)
             VALUES ('current', $1, NOW()::text)
             ON CONFLICT (id) DO UPDATE SET data_json = $1, updated_at = NOW()::text",
        )
        .bind(settings_json)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_settings(&self) -> Result<Option<String>, StorageError> {
        let row: Option<(String,)> = sqlx::query_as("SELECT data_json FROM server_settings WHERE id = 'current'")
            .fetch_optional(self.pool())
            .await
            .map_err(to_storage_err)?;

        Ok(row.map(|(data,)| data))
    }

    // --- Records (T09a) ---

    async fn upsert_record(&self, record: &RecordEntry) -> Result<(), StorageError> {
        sqlx::query(
            "INSERT INTO video_records(id, room_id, created_ms, duration_sec, size_bytes, bitrate_kbps, width, height)
             VALUES($1,$2,$3,$4,$5,$6,$7,$8)
             ON CONFLICT(id) DO UPDATE SET room_id=excluded.room_id, created_ms=excluded.created_ms,
                 duration_sec=excluded.duration_sec, size_bytes=excluded.size_bytes,
                 bitrate_kbps=excluded.bitrate_kbps, width=excluded.width, height=excluded.height",
        )
        .bind(&record.id)
        .bind(record.room_id.to_string())
        .bind(record.created_ms as i64)
        .bind(i64::from(record.duration_sec))
        .bind(record.size_bytes as i64)
        .bind(i64::from(record.bitrate_kbps))
        .bind(i64::from(record.width))
        .bind(i64::from(record.height))
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_record(&self, id: &str) -> Result<Option<RecordEntry>, StorageError> {
        let row: Option<(String, String, i64, i64, i64, i64, i64, i64)> = sqlx::query_as(
            "SELECT id, room_id, created_ms, duration_sec, size_bytes, bitrate_kbps, width, height
             FROM video_records WHERE id=$1",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(row.map(record_from_row).transpose()?)
    }

    async fn list_records(&self, room_id: Option<RoomId>) -> Result<Vec<RecordEntry>, StorageError> {
        let rows: Vec<(String, String, i64, i64, i64, i64, i64, i64)> = match room_id {
            Some(room) => {
                sqlx::query_as(
                    "SELECT id, room_id, created_ms, duration_sec, size_bytes, bitrate_kbps, width, height
                     FROM video_records WHERE room_id=$1 ORDER BY created_ms",
                )
                .bind(room.to_string())
                .fetch_all(self.pool())
                .await
            }
            None => {
                sqlx::query_as(
                    "SELECT id, room_id, created_ms, duration_sec, size_bytes, bitrate_kbps, width, height
                     FROM video_records ORDER BY created_ms",
                )
                .fetch_all(self.pool())
                .await
            }
        }
        .map_err(to_storage_err)?;
        rows.into_iter().map(record_from_row).collect()
    }

    async fn delete_record(&self, id: &str) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM video_records WHERE id=$1")
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(to_storage_err)?;
        Ok(())
    }

    async fn reset(&self) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM room_mode_override").execute(self.pool()).await.map_err(to_storage_err)?;
        // T10: test reset must also clear the directory rows AND the marker,
        // otherwise the next migration would think it already ran (P1-4).
        sqlx::query("DELETE FROM directory_group_members").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM directory_user_roles").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM directory_addresses").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM directory_profiles").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM directory_groups").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM directory_roles").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM schema_state").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM configuration_documents").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM core_layouts").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM core_api_keys").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM core_users").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM user_credentials").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM video_records").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM core_audit_log").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM server_settings").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM layouts").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM api_keys").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM rooms").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM users").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM audit_log").execute(self.pool()).await.map_err(to_storage_err)?;
        self.participants_map().write().unwrap().clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    async fn test_storage_contract(storage: Arc<dyn Storage>) {
        // Rooms
        let room = Room::new("core-room".to_string(), RoomMode::Mcu);
        let room_id = room.id;
        storage.upsert_room(&room).await.expect("upsert_room");

        let loaded = storage.get_room(room_id).await.expect("get_room").expect("exists");
        assert_eq!(loaded.name, "core-room");
        assert_eq!(loaded.mode, RoomMode::Mcu);
        assert!(loaded.created_ms > 0, "created_ms must be extracted from UUIDv7 timestamp");

        let rooms = storage.list_rooms().await.expect("list_rooms");
        assert!(rooms.iter().any(|r| r.id == room_id));

        // Room mode override
        storage.set_room_mode_override(room_id, RoomMode::Sfu).await.expect("set override");
        let overrides = storage.list_room_mode_overrides().await.expect("list overrides");
        assert_eq!(overrides.get(&room_id), Some(&RoomMode::Sfu));
        storage.delete_room_mode_override(room_id).await.expect("delete override");
        let overrides_after = storage.list_room_mode_overrides().await.expect("list overrides");
        assert_eq!(overrides_after.get(&room_id), None);

        // Participants
        let participant =
            Participant::new(vks_core::participant::UserId::new(), "Alice", vks_core::participant::Role::Moderator);
        let p_id = participant.id;
        storage.upsert_participant(&participant).await.expect("upsert_participant");
        let loaded_p = storage.get_participant(p_id).await.expect("get_participant").expect("exists");
        assert_eq!(loaded_p.display_name, "Alice");

        // Layouts
        let layout = Layout {
            id: "layout-1".to_string(),
            name: "Grid 2x2".to_string(),
            version: 1,
            assigned_rooms: std::collections::BTreeMap::new(),
            is_default: false,
        };
        storage.upsert_layout(&layout).await.expect("upsert_layout");
        let loaded_l = storage.get_layout("layout-1").await.expect("get_layout").expect("exists");
        assert_eq!(loaded_l.name, "Grid 2x2");
        let all_layouts = storage.list_layouts().await.expect("list_layouts");
        assert!(all_layouts.iter().any(|l| l.id == "layout-1"));
        storage.delete_layout("layout-1").await.expect("delete_layout");
        assert!(storage.get_layout("layout-1").await.expect("get_layout").is_none());

        // ApiKeys
        let key = ApiKey {
            id: "key-123".to_string(),
            scopes: vec!["read".to_string(), "write".to_string()],
            owner: vks_core::participant::UserId::new(),
            revoked: false,
        };
        storage.upsert_api_key(&key).await.expect("upsert_api_key");
        let loaded_k = storage.get_api_key("key-123").await.expect("get_api_key").expect("exists");
        assert_eq!(loaded_k.scopes, vec!["read", "write"]);
        assert!(!loaded_k.revoked);
        let all_keys = storage.list_api_keys().await.expect("list_api_keys");
        assert!(all_keys.iter().any(|k| k.id == "key-123"));
        storage.delete_api_key("key-123").await.expect("delete_api_key");
        assert!(storage.get_api_key("key-123").await.expect("get_api_key").is_none());

        // Users (M0-M1)
        let user_id = vks_core::UserId::new();
        let user = StoredUser {
            id: user_id,
            username: "contract-user".to_string(),
            role: vks_core::Role::Moderator,
            muted: false,
            deleted: false,
        };
        storage.upsert_user(&user).await.expect("upsert_user");
        let loaded_u = storage.get_user(user_id).await.expect("get_user").expect("user exists");
        assert_eq!(loaded_u.username, "contract-user");
        assert_eq!(loaded_u.role, vks_core::Role::Moderator);
        assert!(!loaded_u.muted);
        assert!(!loaded_u.deleted);
        let all_users = storage.list_users().await.expect("list_users");
        assert!(all_users.iter().any(|u| u.id == user_id));
        storage.delete_user(user_id).await.expect("delete_user");
        assert!(storage.get_user(user_id).await.expect("get_user").is_none());

        // Audit Log (M0-M1)
        let audit_entry = AuditEntry {
            ts_ms: 1700000000000,
            user: "contract-user".to_string(),
            action: "room.create".to_string(),
            room: Some("contract-room".to_string()),
            ip: "127.0.0.1".to_string(),
        };
        storage.append_audit(&audit_entry).await.expect("append_audit");
        let audit_records = storage.list_audit().await.expect("list_audit");
        assert!(audit_records.iter().any(|a| a.user == "contract-user" && a.action == "room.create"));

        // Credentials (M1.3)
        let creds = UserCredentials {
            user_id,
            password_hash_hex: "deadbeef01".to_string(),
            salt_hex: "01020304".to_string(),
            password_changed_at: Some(1700000000),
            generation: 3,
        };
        storage.upsert_user_credentials(&creds).await.expect("upsert_user_credentials");
        let loaded_creds = storage.get_user_credentials(user_id).await.expect("get_user_credentials").expect("exists");
        assert_eq!(loaded_creds.password_hash_hex, "deadbeef01");
        assert_eq!(loaded_creds.salt_hex, "01020304");
        assert_eq!(loaded_creds.password_changed_at, Some(1700000000));
        assert_eq!(loaded_creds.generation, 3);
        let all_creds = storage.list_user_credentials().await.expect("list_user_credentials");
        assert!(all_creds.iter().any(|c| c.user_id == user_id));

        // Records (T09a): durable metadata with profile fields.
        let record = RecordEntry {
            id: "rec-contract-1".to_string(),
            room_id,
            created_ms: 1_700_000_000_000,
            duration_sec: 42,
            size_bytes: 8_192,
            bitrate_kbps: 2350,
            width: 1920,
            height: 1080,
        };
        storage.upsert_record(&record).await.expect("upsert_record");
        let loaded_record = storage.get_record("rec-contract-1").await.expect("get_record").expect("exists");
        assert_eq!(loaded_record.room_id, room_id);
        assert_eq!(loaded_record.duration_sec, 42);
        assert_eq!(loaded_record.size_bytes, 8_192);
        assert_eq!(loaded_record.bitrate_kbps, 2350);
        assert_eq!((loaded_record.width, loaded_record.height), (1920, 1080));
        let listed = storage.list_records(Some(room_id)).await.expect("list_records by room");
        assert!(listed.iter().any(|entry| entry.id == "rec-contract-1"));
        // Upsert must update, not duplicate.
        let mut updated = record.clone();
        updated.size_bytes = 16_384;
        storage.upsert_record(&updated).await.expect("upsert_record update");
        assert_eq!(
            storage.list_records(None).await.expect("list_records").iter().filter(|e| e.id == "rec-contract-1").count(),
            1
        );
        storage.delete_record("rec-contract-1").await.expect("delete_record");
        assert!(storage.get_record("rec-contract-1").await.expect("get_record").is_none());

        // Settings (M1.5)
        let test_settings = "{\"name\":\"test_vks\",\"default_mode\":\"sfu\"}";
        storage.upsert_settings(test_settings).await.expect("upsert_settings");
        let loaded_settings = storage.get_settings().await.expect("get_settings").expect("exists");
        assert_eq!(loaded_settings, test_settings);

        // Health probe (`/readyz`): реальный ping (SQL `SELECT 1`).
        storage.ping().await.expect("ping");

        // Cleanup
        storage.delete_room(room_id).await.expect("delete_room");
        assert!(storage.get_room(room_id).await.expect("get_room").is_none());
    }

    #[tokio::test]
    async fn memory_storage_satisfies_core_port() {
        let mem = Arc::new(MemoryStorage::new());
        test_storage_contract(mem).await;
    }

    #[tokio::test]
    async fn sqlite_storage_satisfies_core_port() {
        let sqlite = Arc::new(SqliteStorage::init("sqlite::memory:").await.expect("init sqlite"));
        test_storage_contract(sqlite).await;
    }
}

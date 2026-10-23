//! Недеструктивная дедупликация `pnm-definitions-v1` (T6, единый справочник).
//!
//! Живой SQLite хранит весь справочник одним JSON-документом. Исторически
//! сервер материализует отдельный `default`-layout на каждую комнату/сессию и
//! копирует контакт адреса в `resources.contacts[room]` с новым `cs-*` id.
//! Поэтому один и тот же адрес имеет несколько id, а идентичные «Общие»
//! раскладки отличаются только id.
//!
//! Правила (консервативные, без потери данных):
//! - адрес = нормализованный URI (`protocol:rest`, lower); canonical id —
//!   глобальный адрес (`resources.addresses`), иначе старейший; все ссылки
//!   (`layout.slots`, `history`, `conference_settings`, `contacts`) переписываются
//!   на canonical id, записи-копии удаляются только после переписывания;
//! - layout схлопывается, только если `payload.type == "default"`, `slots`
//!   пуст и payload байт-идентичен (канонический JSON). `individual` и
//!   раскладки со слотами не трогаем никогда;
//! - неиспользуемые (orphan) раскладки не удаляются — только отчёт.
//!
//! Функция идемпотентна: повторный прогон на очищенном документе не меняет
//! ни одного байта (см. тесты).

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use vks_common::layout_defaults::is_canonical_default;

/// Ключ configuration-document с определениями PNM.
pub const CONFIGURATION_KEY: &str = "pnm-definitions-v1";

/// Отчёт о дедупликации (сериализуется в JSON для CLI/логов).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DedupReport {
    pub addresses_before: usize,
    pub addresses_after: usize,
    pub room_contacts_before: usize,
    pub room_contacts_after: usize,
    pub layouts_before: usize,
    pub layouts_after: usize,
    pub layouts_removed: usize,
    pub links_rewritten: usize,
    pub dangling_links_removed: usize,
    pub promoted_addresses: Vec<String>,
    pub address_aliases: Vec<Alias>,
    pub layout_aliases: Vec<Alias>,
    pub orphan_default_layouts: usize,
    pub skipped_layouts: Vec<String>,
    pub document_bytes_before: usize,
    pub document_bytes_after: usize,
}

/// Переименование id: `from` → `to` (для аудита и отката).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Alias {
    pub from: String,
    pub to: String,
}

/// Нормализация адреса для дедупликации. T10: единая политика с API —
/// нижний регистр только схемы/host, userinfo и параметры сохраняются,
/// default-порты `sip:5060`/`h323:1720` убираются.
#[must_use]
pub fn normalize_uri(uri: &str) -> String {
    vks_common::uri::canonical_uri(uri)
}

/// Канонический JSON (ключи отсортированы) — fingerprint раскладки.
fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let sorted: BTreeMap<&String, &Value> = map.iter().collect();
            let body = sorted.iter().map(|(k, v)| format!("{k}:{}", canonical_json(v))).collect::<Vec<_>>().join(",");
            format!("{{{body}}}")
        }
        Value::Array(items) => {
            format!("[{}]", items.iter().map(canonical_json).collect::<Vec<_>>().join(","))
        }
        other => other.to_string(),
    }
}

fn default_layout_fingerprint(layout: &Value) -> Option<String> {
    let payload = layout.get("payload")?.as_object()?;
    if payload.get("type").and_then(Value::as_str)? != "default" {
        return None;
    }
    let slots_empty = payload.get("slots").and_then(Value::as_array).is_none_or(Vec::is_empty);
    slots_empty.then(|| canonical_json(&Value::Object(payload.clone())))
}

/// Заменяет все строковые значения, равные ключу `aliases`, на canonical id.
fn rewrite_ids(value: &mut Value, aliases: &BTreeMap<String, String>, counter: &mut usize) {
    match value {
        Value::String(s) => {
            if let Some(to) = aliases.get(s) {
                if to != s {
                    *s = to.clone();
                    *counter += 1;
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(|item| rewrite_ids(item, aliases, counter)),
        Value::Object(map) => map.values_mut().for_each(|item| rewrite_ids(item, aliases, counter)),
        _ => {}
    }
}

fn ids_iter(ids: &[Value]) -> Vec<String> {
    ids.iter().filter_map(Value::as_str).map(str::to_string).collect()
}

fn collect_string_refs(value: &Value, refs: &mut BTreeMap<String, usize>) {
    match value {
        Value::String(s) => *refs.entry(s.clone()).or_default() += 1,
        Value::Array(items) => items.iter().for_each(|item| collect_string_refs(item, refs)),
        Value::Object(map) => map.values().for_each(|item| collect_string_refs(item, refs)),
        _ => {}
    }
}

#[derive(Debug, Clone)]
struct AddressEntry {
    id: String,
    uri: String,
    created_at: u64,
    is_global: bool,
}

/// Дедупликация документа in-place. Возвращает отчёт; при повторном вызове
/// на уже очищенном документе отчёт не содержит изменений.
pub fn dedup_document(document: &mut Value) -> DedupReport {
    let mut report = DedupReport { document_bytes_before: document.to_string().len(), ..Default::default() };

    // --- 1. Собираем адреса: глобальные + комнатные копии --------------------
    let mut entries: Vec<AddressEntry> = Vec::new();
    {
        let Some(resources) = document.get("resources").and_then(Value::as_object) else {
            report.document_bytes_after = document.to_string().len();
            return report;
        };
        if let Some(addresses) = resources.get("addresses").and_then(Value::as_object) {
            for (id, record) in addresses {
                let Some(uri) = record.get("uri").and_then(Value::as_str) else { continue };
                entries.push(AddressEntry {
                    id: id.clone(),
                    uri: uri.to_string(),
                    created_at: record.get("created_at").and_then(Value::as_u64).unwrap_or(0),
                    is_global: true,
                });
            }
        }
        report.addresses_before = resources.get("addresses").and_then(Value::as_object).map_or(0, Map::len);
        if let Some(contacts) = resources.get("contacts").and_then(Value::as_object) {
            for list in contacts.values() {
                let Some(items) = list.as_array() else { continue };
                report.room_contacts_before += items.len();
                for contact in items {
                    let Some(id) = contact.get("id").and_then(Value::as_str) else { continue };
                    let uri = contact.get("uri").and_then(Value::as_str).unwrap_or_default();
                    if uri.trim().is_empty() {
                        continue;
                    }
                    entries.push(AddressEntry {
                        id: id.to_string(),
                        uri: uri.to_string(),
                        created_at: contact.get("created_at").and_then(Value::as_u64).unwrap_or(0),
                        is_global: false,
                    });
                }
            }
        }
    }

    let mut groups: BTreeMap<String, Vec<AddressEntry>> = BTreeMap::new();
    for entry in entries {
        groups.entry(normalize_uri(&entry.uri)).or_default().push(entry);
    }

    let mut address_aliases: BTreeMap<String, String> = BTreeMap::new();
    let mut promoted: BTreeSet<String> = BTreeSet::new();
    for mut group in groups.into_values() {
        if group.len() < 2 {
            continue;
        }
        // canonical: глобальный, затем старейший, затем лексикографически меньший id.
        group.sort_by(|a, b| b.is_global.cmp(&a.is_global).then(a.created_at.cmp(&b.created_at)).then(a.id.cmp(&b.id)));
        let canonical = group.remove(0);
        if !canonical.is_global {
            promoted.insert(canonical.id.clone());
        }
        for other in group {
            if other.id == canonical.id {
                continue;
            }
            address_aliases.insert(other.id.clone(), canonical.id.clone());
            report.address_aliases.push(Alias { from: other.id, to: canonical.id.clone() });
        }
    }

    // --- 2. Чистим комнатные копии по исходным id, promoted переносим в книгу -
    let mut promoted_records: Vec<(String, Value)> = Vec::new();
    {
        let Some(resources) = document.get_mut("resources").and_then(Value::as_object_mut) else {
            report.document_bytes_after = document.to_string().len();
            return report;
        };
        if let Some(contacts) = resources.get_mut("contacts").and_then(Value::as_object_mut) {
            let room_ids: Vec<String> = contacts.keys().cloned().collect();
            for room_id in room_ids {
                let Some(list) = contacts.get_mut(&room_id).and_then(Value::as_array_mut) else { continue };
                for contact in list.iter() {
                    let id = contact.get("id").and_then(Value::as_str).unwrap_or_default();
                    if promoted.contains(id) {
                        let record = json!({
                            "id": id,
                            "uri": contact.get("uri").cloned().unwrap_or_else(|| json!("")),
                            "display_name": contact.get("display_name").cloned().unwrap_or_else(|| json!("")),
                            "created_at": contact.get("created_at").cloned().unwrap_or_else(|| json!(0)),
                            "payload": contact.get("payload").cloned().unwrap_or_else(|| json!({})),
                        });
                        promoted_records.push((id.to_string(), record));
                    }
                }
                list.retain(|contact| {
                    let id = contact.get("id").and_then(Value::as_str).unwrap_or_default();
                    !address_aliases.contains_key(id) && !promoted.contains(id)
                });
            }
            contacts.retain(|_, list| list.as_array().is_some_and(|items| !items.is_empty()));
        }
        if let Some(addresses) = resources.get_mut("addresses").and_then(Value::as_object_mut) {
            for (id, record) in promoted_records {
                addresses.insert(id.clone(), record);
                report.promoted_addresses.push(id);
            }
            let alias_ids: Vec<String> =
                address_aliases.keys().filter(|id| addresses.contains_key(*id)).cloned().collect();
            for id in alias_ids {
                addresses.remove(&id);
            }
        }
        report.addresses_after = resources.get("addresses").and_then(Value::as_object).map_or(0, Map::len);
        report.room_contacts_after = resources
            .get("contacts")
            .and_then(Value::as_object)
            .map_or(0, |contacts| contacts.values().filter_map(Value::as_array).map(Vec::len).sum());
    }

    // --- 3. Переписываем ссылки на canonical id по всему документу ----------
    let mut rewritten = 0usize;
    if !address_aliases.is_empty() {
        rewrite_ids(document, &address_aliases, &mut rewritten);
        report.links_rewritten += rewritten;
    }

    // --- 4. Группируем байт-идентичные default-раскладки --------------------
    let mut layout_aliases: BTreeMap<String, String> = BTreeMap::new();
    let mut original_refs: BTreeMap<String, usize> = BTreeMap::new();
    {
        let Some(resources) = document.get("resources").and_then(Value::as_object) else {
            report.document_bytes_after = document.to_string().len();
            return report;
        };
        let Some(layouts) = resources.get("layouts").and_then(Value::as_object) else {
            report.document_bytes_after = document.to_string().len();
            return report;
        };
        report.layouts_before = layouts.len();
        let mut refcount: BTreeMap<&str, usize> = BTreeMap::new();
        if let Some(links) = resources.get("links").and_then(Value::as_object) {
            for ids in links.values().filter_map(Value::as_array) {
                for id in ids.iter().filter_map(Value::as_str) {
                    *refcount.entry(id).or_default() += 1;
                    *original_refs.entry(id.to_string()).or_default() += 1;
                }
            }
        }
        let mut variants: BTreeMap<String, Vec<(&String, &Value)>> = BTreeMap::new();
        for (id, layout) in layouts {
            if let Some(fp) = default_layout_fingerprint(layout) {
                variants.entry(fp).or_default().push((id, layout));
            }
        }
        for mut group in variants.into_values() {
            if group.len() < 2 {
                continue;
            }
            group.sort_by(|a, b| {
                let ra = refcount.get(a.0.as_str()).copied().unwrap_or(0);
                let rb = refcount.get(b.0.as_str()).copied().unwrap_or(0);
                rb.cmp(&ra)
                    .then(
                        a.1.get("created_at")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                            .cmp(&b.1.get("created_at").and_then(Value::as_u64).unwrap_or(0)),
                    )
                    .then(a.0.cmp(b.0))
            });
            let canonical = group[0].0.clone();
            for (dup, _) in group.iter().skip(1) {
                layout_aliases.insert((*dup).clone(), canonical.clone());
                report.layout_aliases.push(Alias { from: (*dup).clone(), to: canonical.clone() });
            }
        }
    }

    // --- 5. Переписываем links, удаляем неиспользуемые клоны ----------------
    if !layout_aliases.is_empty() {
        let mut counter = 0usize;
        {
            let Some(links) = document
                .get_mut("resources")
                .and_then(Value::as_object_mut)
                .and_then(|r| r.get_mut("links"))
                .and_then(Value::as_object_mut)
            else {
                report.document_bytes_after = document.to_string().len();
                return report;
            };
            for ids in links.values_mut().filter_map(Value::as_array_mut) {
                let mut seen = BTreeSet::new();
                let mut next: Vec<Value> = Vec::with_capacity(ids.len());
                for value in ids_iter(ids) {
                    let mapped = layout_aliases.get(&value).cloned().unwrap_or_else(|| value.clone());
                    if mapped != value {
                        counter += 1;
                    }
                    if seen.insert(mapped.clone()) {
                        next.push(json!(mapped));
                    }
                }
                *ids = next;
            }
        }
        report.links_rewritten += counter;

        // active_layouts и COW-алиасы обязаны пережить дедуп: иначе после
        // перезапуска active укажет на удалённый дубль.
        if let Some(resources) = document.get_mut("resources").and_then(Value::as_object_mut) {
            if let Some(active) = resources.get_mut("active_layouts").and_then(Value::as_object_mut) {
                for value in active.values_mut() {
                    if let Some(id) = value.as_str() {
                        if let Some(mapped) = layout_aliases.get(id) {
                            *value = json!(mapped);
                        }
                    }
                }
            }
            if let Some(aliases) = resources.get_mut("layout_aliases").and_then(Value::as_object_mut) {
                for value in aliases.values_mut() {
                    if let Some(id) = value.as_str() {
                        if let Some(mapped) = layout_aliases.get(id) {
                            *value = json!(mapped);
                        }
                    }
                }
            }
        }

        // Ссылки вне links (links/история/настройки) — проверяем до удаления.
        let mut outside_refs: BTreeMap<String, usize> = BTreeMap::new();
        if let Some(resources) = document.get("resources").and_then(Value::as_object) {
            for (key, value) in resources {
                if key == "layouts" {
                    continue;
                }
                if key == "links" {
                    if let Some(links) = value.as_object() {
                        for ids in links.values().filter_map(Value::as_array) {
                            for id in ids.iter().filter_map(Value::as_str) {
                                *outside_refs.entry(id.to_string()).or_default() += 1;
                            }
                        }
                    }
                    continue;
                }
                collect_string_refs(value, &mut outside_refs);
            }
        }
        let mut removed = 0usize;
        if let Some(layouts) = document
            .get_mut("resources")
            .and_then(Value::as_object_mut)
            .and_then(|r| r.get_mut("layouts"))
            .and_then(Value::as_object_mut)
        {
            for dup in layout_aliases.keys() {
                if !layouts.contains_key(dup) {
                    continue;
                }
                // Непривязанный (orphan) layout не удаляем даже при совпадении payload:
                // UI создаёт раскладки до attach, и они могут быть ещё не сохранены.
                if original_refs.get(dup).copied().unwrap_or(0) == 0 {
                    report.skipped_layouts.push(dup.clone());
                    continue;
                }
                if outside_refs.get(dup).copied().unwrap_or(0) > 0 {
                    report.skipped_layouts.push(dup.clone());
                    continue;
                }
                layouts.remove(dup);
                removed += 1;
            }
        }
        report.layouts_removed = removed;
    }

    // --- 6. Чистим висячие ссылки links (id без ресурса) --------------------
    let (layout_ids, audio_ids) = {
        let Some(resources) = document.get("resources").and_then(Value::as_object) else {
            report.document_bytes_after = document.to_string().len();
            return report;
        };
        (
            resources
                .get("layouts")
                .and_then(Value::as_object)
                .map(|m| m.keys().cloned().collect::<BTreeSet<String>>())
                .unwrap_or_default(),
            resources
                .get("audiochannels")
                .and_then(Value::as_object)
                .map(|m| m.keys().cloned().collect::<BTreeSet<String>>())
                .unwrap_or_default(),
        )
    };
    if let Some(links) = document
        .get_mut("resources")
        .and_then(Value::as_object_mut)
        .and_then(|r| r.get_mut("links"))
        .and_then(Value::as_object_mut)
    {
        let keys: Vec<String> = links.keys().cloned().collect();
        for key in keys {
            let Some(ids) = links.get_mut(&key).and_then(Value::as_array_mut) else { continue };
            let before = ids.len();
            ids.retain(|value| value.as_str().is_some_and(|id| layout_ids.contains(id) || audio_ids.contains(id)));
            report.dangling_links_removed += before - ids.len();
        }
        links.retain(|_, ids| ids.as_array().is_some_and(|items| !items.is_empty()));
    }

    // --- 7. Отчёт: orphan-default и итоговые размеры ------------------------
    if let Some(resources) = document.get("resources").and_then(Value::as_object) {
        if let Some(layouts) = resources.get("layouts").and_then(Value::as_object) {
            report.layouts_after = layouts.len();
            let referenced: BTreeSet<&str> = resources
                .get("links")
                .and_then(Value::as_object)
                .map(|links| links.values().filter_map(Value::as_array).flatten().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            report.orphan_default_layouts = layouts
                .iter()
                .filter(|(id, layout)| {
                    !referenced.contains(id.as_str())
                        && layout.get("payload").and_then(|p| p.get("type")).and_then(Value::as_str) == Some("default")
                })
                .count();
        }
    }
    ensure_active_layouts(document);

    report.document_bytes_after = document.to_string().len();
    report
}

/// Идемпотентно выводит active_layouts из links, если поле отсутствует или
/// указывает на удалённый/непривязанный layout. Порядок links не меняется.
fn ensure_active_layouts(document: &mut Value) {
    let Some(resources) = document.get_mut("resources").and_then(Value::as_object_mut) else { return };
    let layouts: std::collections::BTreeSet<String> =
        resources.get("layouts").and_then(Value::as_object).map(|m| m.keys().cloned().collect()).unwrap_or_default();
    let links = resources.get("links").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut active: BTreeMap<String, String> = resources
        .get("active_layouts")
        .and_then(Value::as_object)
        .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|v| (k.clone(), v.to_string()))).collect())
        .unwrap_or_default();
    for (key, ids) in &links {
        if !key.ends_with(":video_layouts") {
            continue;
        }
        let list: Vec<&str> = ids.as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        if list.is_empty() {
            active.remove(key);
            continue;
        }
        if active.get(key).is_some_and(|id| list.contains(&id.as_str()) && layouts.contains(id)) {
            continue;
        }
        // T01-effective: последняя non-default в links, иначе canonical/default.
        // Иначе миграция потеряет выбранную individual у 21 scope.
        let chosen = list
            .iter()
            .rev()
            .find(|id| {
                resources
                    .get("layouts")
                    .and_then(|l| l.get(**id))
                    .is_some_and(|layout| layout["payload"]["type"].as_str() != Some("default"))
            })
            .or_else(|| {
                list.iter()
                    .find(|id| resources.get("layouts").and_then(|l| l.get(**id)).is_some_and(is_canonical_default))
            })
            .or_else(|| list.iter().rev().find(|id| layouts.contains(**id)))
            .or_else(|| list.first());
        if let Some(id) = chosen {
            active.insert(key.clone(), (*id).to_string());
        }
    }
    active.retain(|key, id| key.ends_with(":video_layouts") && links.contains_key(key) && layouts.contains(id));
    if active.is_empty() {
        resources.remove("active_layouts");
    } else {
        resources.insert("active_layouts".to_string(), serde_json::to_value(active).unwrap_or_else(|_| json!({})));
    }
}

/// Возвращает id канонической default-раскладки (для починки
/// `server_settings.default_layout`): canonical auto/empty приоритетнее
/// legacy manual-default, затем самый популярный среди ссылок.
#[must_use]
pub fn canonical_default_layout(document: &Value) -> Option<String> {
    let resources = document.get("resources")?.as_object()?;
    let layouts = resources.get("layouts")?.as_object()?;
    let links = resources.get("links").and_then(Value::as_object);
    let mut refcount: BTreeMap<String, usize> = BTreeMap::new();
    if let Some(links) = links {
        for ids in links.values().filter_map(Value::as_array) {
            for id in ids.iter().filter_map(Value::as_str) {
                *refcount.entry(id.to_string()).or_default() += 1;
            }
        }
    }
    let mut candidates: Vec<(&String, &Value)> = layouts
        .iter()
        .filter(|(_, layout)| {
            layout.get("payload").and_then(|p| p.get("type")).and_then(Value::as_str) == Some("default")
        })
        .collect();
    candidates.sort_by(|a, b| {
        is_canonical_default(b.1)
            .cmp(&is_canonical_default(a.1))
            .then(refcount.get(b.0).copied().unwrap_or(0).cmp(&refcount.get(a.0).copied().unwrap_or(0)))
            .then(
                a.1.get("created_at")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .cmp(&b.1.get("created_at").and_then(Value::as_u64).unwrap_or(0)),
            )
            .then(a.0.cmp(b.0))
    });
    candidates.first().map(|(id, _)| (*id).clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        json!({
            "version": 1,
            "conference_settings": {
                "room-a": {"creator_id": "admin", "video_layout_id": "L-def-1"}
            },
            "resources": {
                "addresses": {
                    "cs-sip": {"id":"cs-sip","uri":"sip:192.168.1.132","display_name":"Зал 132 (sip)","created_at":100,"payload":{"sip":{"bitrate":2048}}},
                    "cs-h323": {"id":"cs-h323","uri":"H323:192.168.1.132","display_name":"Зал 132 (h323)","created_at":101,"payload":{"h323":{"bitrate":4096}}}
                },
                "contacts": {
                    "room-a": [{"id":"cs-copy-a","uri":"sip:192.168.1.132","display_name":"Зал 132 (sip)","created_at":200,"payload":{},"contact_id":"cs-copy-a"}],
                    "room-b": [{"id":"cs-copy-b","uri":"SIP:192.168.1.132","display_name":"Зал 132 (sip)","created_at":201,"payload":{},"contact_id":"cs-copy-b"}]
                },
                "layouts": {
                    "L-def-1": {"id":"L-def-1","created_at":10,"payload":{"type":"default","mode":"auto","slots":[],"preset":"preset_default_13"}},
                    "L-def-2": {"id":"L-def-2","created_at":11,"payload":{"type":"default","mode":"auto","slots":[],"preset":"preset_default_13"}},
                    "L-def-3": {"id":"L-def-3","created_at":12,"payload":{"type":"default","mode":"manual","slots":[],"preset":"preset_default_13"}},
                    "L-ind-1": {"id":"L-ind-1","created_at":13,"payload":{"type":"individual","mode":"manual","slots":[{"abonent_id":"p1","contact_id":"cs-copy-a"}]}},
                    "L-orphan": {"id":"L-orphan","created_at":14,"payload":{"type":"default","mode":"manual","slots":[],"preset":"preset_default_13"}}
                },
                "links": {
                    "conferences:room-a:video_layouts": ["L-def-1","L-ind-1"],
                    "sessions:room-a:video_layouts": ["L-def-2","L-ind-1"],
                    "conferences:room-b:video_layouts": ["L-def-1"],
                    "sessions:room-b:video_layouts": ["L-def-2"],
                    "conferences:room-a:audiochannels": []
                },
                "directory": {"user_addresses": {"u1": {"cs-pa": {"id":"cs-pa","uri":"sip:t21@example.com","display_name":"addr"}}}},
                "history": {"sessions": [{"id":"h1","calls":[{"contact_id":"cs-copy-b","connection_url":"sip:192.168.1.132"}]}]},
                "audiochannels": {},
                "admin_settings": {},
                "schedules": {}
            }
        })
    }

    #[test]
    fn merges_duplicate_addresses_and_rewrites_every_reference() {
        let mut doc = fixture();
        let report = dedup_document(&mut doc);
        assert_eq!(report.addresses_before, 2);
        assert_eq!(report.addresses_after, 2, "sip-копии и h323 — разные протоколы");
        assert_eq!(report.room_contacts_after, 0);
        assert_eq!(report.address_aliases.len(), 2);
        let text = doc.to_string();
        assert!(!text.contains("cs-copy-a") && !text.contains("cs-copy-b"), "id-копии должны исчезнуть");
        let slot = &doc["resources"]["layouts"]["L-ind-1"]["payload"]["slots"][0];
        assert_eq!(slot["contact_id"], "cs-sip", "слот переписан на canonical");
        let call = &doc["resources"]["history"]["sessions"][0]["calls"][0];
        assert_eq!(call["contact_id"], "cs-sip", "история переписана на canonical");
        assert_eq!(call["connection_url"], "sip:192.168.1.132");
    }

    #[test]
    fn collapses_identical_default_layouts_only() {
        let mut doc = fixture();
        let report = dedup_document(&mut doc);
        let layouts = &doc["resources"]["layouts"];
        assert_eq!(report.layouts_removed, 1, "L-def-2 — байт-идентичный клон L-def-1");
        assert!(layouts.get("L-def-1").is_some());
        assert!(layouts.get("L-def-2").is_none());
        assert!(layouts.get("L-def-3").is_some(), "другой payload (manual) — отдельный вариант, сохраняем");
        assert!(layouts.get("L-ind-1").is_some(), "individual со слотами не трогаем");
        assert!(layouts.get("L-orphan").is_some(), "orphan не удаляем");
        let links = &doc["resources"]["links"];
        assert_eq!(links["sessions:room-a:video_layouts"], json!(["L-def-1", "L-ind-1"]));
    }

    #[test]
    fn active_layouts_are_remapped_and_filled_idempotently() {
        let mut doc = json!({
            "version": 1,
            "resources": {
                "layouts": {
                    "L-default": {"id":"L-default","payload":{"type":"default","mode":"auto","slots":[]}},
                    "L-dup": {"id":"L-dup","payload":{"type":"default","mode":"auto","slots":[]}},
                    "L-manual": {"id":"L-manual","payload":{"type":"individual","mode":"manual","slots":[]}}
                },
                "links": {
                    "sessions:room-a:video_layouts": ["L-default","L-dup","L-manual"],
                    "sessions:room-b:video_layouts": ["L-manual"]
                },
                "active_layouts": {
                    "sessions:room-a:video_layouts": "L-dup"
                }
            }
        });
        let report = dedup_document(&mut doc);
        assert_eq!(report.layouts_before, 3);
        let active = &doc["resources"]["active_layouts"];
        assert_eq!(active["sessions:room-a:video_layouts"], "L-default", "active перемаплен на canonical");
        assert_eq!(active["sessions:room-b:video_layouts"], "L-manual", "active выведен из links");
        let once = doc.to_string();
        let second = dedup_document(&mut doc);
        assert!(second.layout_aliases.is_empty());
        assert_eq!(doc.to_string(), once, "повторный прогон байт-идентичен");
    }

    #[test]
    fn second_run_is_byte_identical() {
        let mut doc = fixture();
        let first = dedup_document(&mut doc);
        assert!(first.layouts_removed > 0);
        let snapshot = doc.to_string();
        let second = dedup_document(&mut doc);
        assert_eq!(doc.to_string(), snapshot, "повторный прогон не меняет документ");
        assert_eq!(second.layouts_removed, 0);
        assert_eq!(second.address_aliases.len(), 0);
        assert_eq!(second.links_rewritten, 0);
        assert_eq!(second.dangling_links_removed, 0);
    }

    #[test]
    fn promotes_contact_to_global_when_no_global_address_exists() {
        let mut doc = json!({"resources": {
            "addresses": {},
            "contacts": {
                "r1": [{"id":"cs-one","uri":"sip:10.0.0.1","display_name":"a","created_at":1,"payload":{}}],
                "r2": [{"id":"cs-two","uri":"sip:10.0.0.1","display_name":"a","created_at":2,"payload":{}}]
            },
            "layouts": {}, "links": {}, "audiochannels": {},
            "directory": {}, "history": {}, "admin_settings": {}, "schedules": {}
        }});
        let report = dedup_document(&mut doc);
        assert_eq!(report.promoted_addresses, vec!["cs-one".to_string()]);
        assert!(doc["resources"]["addresses"].get("cs-one").is_some());
        assert_eq!(report.room_contacts_after, 0);
        assert_eq!(report.addresses_after, 1);
    }

    #[test]
    fn normalize_uri_matches_api_canonical_policy() {
        assert_eq!(normalize_uri("SIP:Host:5060"), "sip:host");
        assert_eq!(normalize_uri("sip:Host"), normalize_uri("SIP:Host"));
        assert_eq!(normalize_uri("h323:Host:1720"), "h323:host");
        assert_eq!(normalize_uri("Example.COM"), "sip:example.com");
        // userinfo регистр сохраняется: SIP user-part case-sensitive.
        assert_ne!(normalize_uri("sip:Alice@Host"), normalize_uri("sip:alice@Host"));
    }

    #[test]
    fn canonical_default_layout_ignores_dangling_setting() {
        let mut doc = fixture();
        let _ = dedup_document(&mut doc);
        assert_eq!(canonical_default_layout(&doc).as_deref(), Some("L-def-1"));
    }
}

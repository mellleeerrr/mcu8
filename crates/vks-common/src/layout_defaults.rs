//! Каноническая «общая» раскладка — единственный источник её payload'а.
//!
//! Payload проверяется в двух местах: при дедупликации ресурсов
//! (`vks-storage`) и при разборе раскладки (`vks-admin`). Раньше он был
//! продублирован в двух крейтах, и правка одного места тихо расходилась с
//! другим: дедупликация выбирала один canonical, а runtime — другой.
//! Общее место — `vks-common`, от которого зависят оба крейта.

use serde_json::{json, Value};

/// Payload канонической «Общей» раскладки (схема `preset_default_13`).
#[must_use]
pub fn canonical_default_payload() -> Value {
    json!({"mode":"auto", "preset":"preset_default_13", "type":"default", "slots":[], "size":13, "autoscroll":0})
}

/// True, если payload раскладки совпадает с канонической «Общей».
#[must_use]
pub fn is_canonical_default(layout: &Value) -> bool {
    layout["payload"] == canonical_default_payload()
}

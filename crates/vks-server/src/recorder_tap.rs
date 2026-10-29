//! Tap записи на уже кодируемый поток группы фермы (T09b).
//!
//! Рекордер не создаёт энкодер и не становится «скрытым зрителем»: он
//! подписывается на `broadcast::Receiver<Arc<AccessUnit>>` **существующей**
//! группы (см. [`EncoderFarm::subscribe`](vks_media_mcu::EncoderFarm::subscribe))
//! и пишет её Annex-B access units в MP4-контейнер текущей
//! [`RecordingSession`].
//!
//! Инварианты:
//! - до первого IDR с SPS/PPS ничего не пишется (иначе `stss` объявил бы
//!   delta-кадр sync-кадром);
//! - `Lagged(n)` считается потерей и не рвёт запись (в `stts` будет большая
//!   дельта — это честный gap);
//! - `Closed` (группа ретировалась вместе с последним зрителем) завершает tap:
//!   запись без зрителя не продолжается, hidden group не поднимается;
//! - ошибка writer'а логируется и останавливает tap; `stop` затем честно
//!   вернёт ошибку финализации (не «200 с пустым файлом»).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast::error::RecvError;
use tracing::{debug, warn};
use vks_core::RoomId;
use vks_media_mcu::farm::FarmTap;
use vks_recorder::RecordingSession;

/// Крутит tap до `Closed`, исчезновения сессии или ошибки writer'а.
pub(crate) async fn run(room: RoomId, mut tap: FarmTap, sessions: Arc<Mutex<HashMap<RoomId, RecordingSession>>>) {
    // Snapshot группы плюс SPS/PPS из встреченных AU; дедуп по байтам.
    let mut parameter_sets = tap.parameter_sets.clone();
    let mut started = false;
    let mut lagged_frames = 0u64;

    loop {
        match tap.rx.recv().await {
            Ok(access_unit) => {
                for set in access_unit.parameter_sets() {
                    if !parameter_sets.iter().any(|known| known.as_ref() == set.as_ref()) {
                        parameter_sets.push(Arc::clone(set));
                    }
                }
                if !started {
                    // Первый записанный сэмпл обязан быть IDR, а avcC — уже
                    // иметь SPS/PPS (P1-4: stss не подставляет кадр 1).
                    if !access_unit.is_idr() || parameter_sets.is_empty() {
                        continue;
                    }
                    started = true;
                }
                let mut guard = sessions.lock().unwrap();
                let Some(session) = guard.get_mut(&room) else {
                    debug!(room = %room, "recording session is gone; tap stops");
                    break;
                };
                let result = session.set_video_parameter_sets(&parameter_sets).and_then(|()| {
                    session.write_video_sample(access_unit.data(), access_unit.pts_us(), access_unit.is_idr())
                });
                if let Err(error) = result {
                    warn!(room = %room, %error, "recorder tap stopped: writer rejected a video sample");
                    break;
                }
            }
            Err(RecvError::Lagged(dropped)) => {
                lagged_frames = lagged_frames.saturating_add(dropped);
                debug!(room = %room, dropped, total = lagged_frames, "recorder tap lagged; continuing");
            }
            Err(RecvError::Closed) => {
                debug!(room = %room, lagged = lagged_frames, "encoder group closed; tap stops");
                break;
            }
        }
    }
}

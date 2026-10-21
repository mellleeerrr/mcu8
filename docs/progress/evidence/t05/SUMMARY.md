# T05a — реальные max frame size / bitrate: evidence

Дата: 2026-10-22. HEAD до задачи: `7c2c2e4`.
Живой стенд: `vks-mcu.service`, `http://127.0.0.1:8080`, SIP/H.323 `192.168.1.132`.

## Что изменено

Единый профиль видео для gateway-сессии, SDP offer и FFmpeg:

`env base → global /api/v3/settings/video.max_frame_rate → conference settings (bitrate/resolution/fps_limit) → call_string.sip|h323` (последний — высший приоритет).

- `crates/vks-server/src/gateway_video.rs`
  - `VideoProfile`: width/height/fps/bitrate/profile/level; default
    `1280x720@30/2048 kbps/baseline/3.1`.
  - `apply_video_settings`: global fps, затем payload комнаты
    (`bitrate`, `resolution.width/height/fps_limit`); валидация/чётность
    (160..1920 / 120..1080, fps 1..60, bitrate 64..8192).
  - `apply_call_string_settings`: те же поля из `call_string.sip/h323`,
    применяются после комнаты.
  - `max_fs` (макроблоки 16×16), `max_mbps`, `profile_level_id`
    (`42c0<level_idc>` — совпадает с SPS x264 baseline), `level_idc`,
    `clamp_to_answer(max_fs,max_br,max_mbps)`.
  - Answer-clamp размера идёт по стандартной лестнице 16:9
    `1920×1080 → 1280×720 → 960×540 → 640×360 → 480×270 → 320×180`,
    а не делением пополам.
  - `frame_interval(profile)` = `1_000_000/fps` мкс; render loop больше не
    прибит к 66 667 мкс (15 fps).
- `crates/vks-server/src/gateways.rs`
  - `video_profile(room_id, call_string)` — приоритеты выше;
    `call_string_video_profile(uri)` + чистый `find_call_string_override`.
  - При дозвоне профиль считается до SDP; `offered_video_profile`
    сохраняется, из answer SDP читаются `max-fs/max-br/max-mbps/b=AS`,
    эффективный профиль клампится (`min(max-br, b=AS)`).
  - `media_stats`/`media_details`: `offered_video_size`,
    `offered_video_bitrate_kbps`, `offered_video_max_fs/mbps/b_as`,
    `answer_video_max_fs/br/mbps/b_as`,
    `runtime_video_size/max_fps/bitrate_kbps`.
- `crates/vks-gateway-sip/src/rtp_bridge.rs`
  - `VideoSdpProfile { width, height, fps, bitrate_kbps, profile_level_id }`,
    `build_offer_with_video`, `build_plain_sdp_with_video`.
  - SDP: `b=AS:<kbps>` в video m-line, `max-fs` (МБ 16×16),
    `max-mbps = max_fs*fps`, `max-br = bitrate_kbps`,
    `profile-level-id=42c0<level_idc>`.
  - `parse_sdp` читает `b=AS`; `video_answer_limits` — детерминированный
    выбор H.264 PT (явный PT, иначе минимальный H.264 PT), `b_as`,
    `max-fs/max-br/max-mbps`.
- `crates/vks-admin/src/api_v3/sessions.rs`
  - Удалён жёсткий `640×360` для tx/rx: геометрия берётся только из
    `runtime_video_size`, поэтому UI/тесты видят реальный профиль.
  - `attach_gateway_statistics` публикует offered/effective/answer-лимиты.
- `tests/e2e/live.js`
  - профиль комнаты применяется после старта/автосохранения модалки, перед
    дозвоном; проверки offer size/bitrate, max-fs, answer-лимитов, effective
    resolution, измеренного RTP tx; `VKS_VIDEO_LOW=1` — второй живой профиль.

Native-профиль не менялся: PCMU/PCMA/H264, без H.264-high/H.265.
Env `VKS_GATEWAY_VIDEO_PROFILE` вне baseline теперь игнорируется с warning
(H.264-high включается только в T05b после parser/native flags/H.323).

## Живые прогоны (финальный бинарь, после удаления хардкода 640×360)

### Профиль 1920×1080 / 4096 kbps

`artifacts/e2e-t05-final3`: **108 checks, 0 failed, 110.3 s, page_errors 0**.
Журнал: `outbound gateway video profile width=1920 height=1080 fps=30 bitrate_kbps=4096`.

SDP offer (единый unit-тест + runtime-поля):
```
m=video ... 97
b=AS:4096
a=fmtp:97 packetization-mode=1;profile-level-id=42c028;max-mbps=244800;max-fs=8160;max-br=4096
```
Answer терминала: `max-br=20010`, `max-fs` отсутствует, `b=AS` отсутствует →
clamp не уменьшает 1080p; эффективный профиль **1920×1080**.
FFmpeg: `-s 1920x1080 -r 30 -b:v 4096k`.
Фактический RTP tx по `engine/abonents`: **4 021 644 bps ≈ 4.02 Mbps**
(H264, >1000 пакетов), т.е. ≈4 Mbps, как требует приёмка.

### Профиль 640×360 / 512 kbps

`artifacts/e2e-t05-low-final2`: **108 checks, 0 failed, 110.4 s, page_errors 0**.
SDP offer:
```
b=AS:512
a=fmtp:97 packetization-mode=1;profile-level-id=42c01f;max-mbps=27600;max-fs=920;max-br=512
```
Answer: `max-br=20010`, `max-mbps=49000`, `max-fs` нет → эффективный
**640×360**. Фактический RTP tx: **605 027 bps ≈ 0.61 Mbps** (<1.5 Mbps),
т.е. настройка реально влияет на эфир: 4.02 Mbps vs 0.61 Mbps.

## Исправленные по ревью дефекты

- **B1:** `sessions.rs` безусловно перезаписывал `tx/rx` на `640×360` при
  наличии пакетов; из-за этого оба профиля выглядели одинаково, а
  «answer-clamp» был ложнозелёным. Блок удалён, геометрия — только из
  `runtime_video_size`.
- **B2:** halving 1920×1080 при `max-fs=920` давал 480×270; заменено на
  лестницу 16:9, unit-тест ожидает 640×360.
- **R3:** SDP `profile-level-id` теперь `42c0…` (SPS x264 baseline), а не
  `42e0…`.
- **R4:** приоритет call_string покрыт unit-тестом чистого поиска
  `find_call_string_override` (URI case-insensitive, выбор sip/h323) и
  тестом применения; live-контакт UI создаёт `bitrate=null`.
- **R5:** offered/answer лимиты и effective size публикуются в
  `media_details` и проверяются E2E (`offered_max_fs`, `answer_max_br`,
  effective resolution).
- **R6:** крайние случаи clamp (`только max-fs`, `только max-br`,
  `только max-mbps`, невозможный max-fs) покрыты тестами.
- **R8:** env/call_string/конференция клампят битрейт в 64..8192.
- **R9:** `video_answer_limits` читает `b=AS`, детерминированно выбирает PT.

## Тесты/гейты

- `cargo clippy --workspace --all-targets -- -D warnings` — чисто.
- `cargo test --workspace --all-targets --no-fail-fast` —
  **1014 passed / 0 failed / 6 ignored**.
- Новые/обновлённые тесты: `profile_derives_h264_limits_and_level`,
  `conference_settings_override_global_and_env`,
  `call_string_settings_override_conference`,
  `answer_limits_handle_each_limit_independently`,
  `answer_limits_clamp_bitrate_size_and_fps`,
  `call_string_lookup_matches_uri_case_insensitively_and_picks_protocol`,
  `answer_limits_read_h264_fmtp_and_b_as_deterministically` (6/6 прогонов
  после фикса HashMap-порядка),
  `offer_video_limits_follow_effective_profile`,
  `offer_carries_full_video_matrix_with_h264_and_h265_profiles`.
- E2E: оба профиля 108/108, page_errors 0, cleanup без сирот; 8
  console-errors — pre-existing null-deref vendor UI при навигации
  (T04a), page_errors 0.

## Ограничения / отложено в T05b

- H.323: H.241 `maxBitRate`/level вместо SDP `b=AS` (sidecar пока `5120`).
- SFU/WebRTC: per-media `b=AS`/H264 fmtp в `vks-signaling`.
- H.264-high: включать только после обновления parser/native flags/H.323;
  сейчас env non-baseline принудительно откатывается в baseline.
- Измеренный fps 22–27 при профиле 30 — среднее с момента соединения,
  включая стартовый ramp; каденция рендера 33.3 мс подтверждена unit-тестом.
- Answer-clamp размера — лестница только 16:9; для 4:3-профиля в T05b
  нужен подбор той же пропорции.
- Опубликованные `offered_b_as/answer_max_br` — кбит/с, а
  `offered_bitrate/requested_bitrate` — бит/с; имена стоит привести к
  единому суффиксу при следующей правке admin-DTO.
- `VKS_GATEWAY_VIDEO_LEVEL` пересчитывается из фактического размера/профиля
  и в production фактически не используется; env-уровень — не контракт.

## Следующая задача

T05b (H.323 H.241 + SFU b=AS), затем T06a.

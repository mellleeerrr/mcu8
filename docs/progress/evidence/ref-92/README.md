# Эталонные ответы TrueConf MCU 2.2.1.66 (192.168.1.92)

Снято 2026-10-20 с живого сервера `https://192.168.1.92` (`admin`), GET-only, для
паритета DTO/форм. Используется в T1.2–T2.4 как golden-фикстуры (не для копирования
бизнес-логики, а для сверки полей и форм ответов нашего `/api/v3`).

| Файл | Запрос |
|---|---|
| `calibration.json` | `GET /api/v3/engine/calibration` → `{"calibration":null}` (T1.1: раздел удалён) |
| `features.json` | `GET /api/v3/features` (массив 50 флагов, не объект) |
| `conference_ui_detail.json` | `GET /api/v3/ui/conferences/{id}` — `addresses:{sip,h323,webrtc}`, `primary_dtmf`, `contacts` |
| `conferences_ui_list.json` | `GET /api/v3/ui/conferences?...` — список (без `addresses`) |
| `system_status.json` | `GET /api/v3/ui/system/status` — `engine/disk/sip/h323/time` |
| `settings_video.json`, `settings_calls_sip.json`, `settings_calls_h323.json` | `GET /api/v3/settings/...` → `{payload:{...}}` |
| `groups.json`, `call_history.json`, `schedules.json`, `audiochannels.json`, `video_layouts.json`, `contacts.json` | соответствующие GET-и |

Секреты в фикстурах отсутствуют (пароли/токены не сохранялись).

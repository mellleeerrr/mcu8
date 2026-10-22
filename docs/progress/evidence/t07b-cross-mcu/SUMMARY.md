# T07b — кросс-MCU SIP: 20 вызовов на TrueConf MCU 192.168.1.92

Дата: 2026-10-22, HEAD до коммита.

## Что проверено

- Живая сессия `4d313d01-8300-450c-ab08-28c8a00f9f25`, 20 SIP call string
  на конференцию `sip:35a87982-1eed-4138-a8cd-589c88aac0cb@192.168.1.92`
  (уникальные `;transport=udp;tag=N`, т.к. повтор canonical URI отклоняется 409).
- Через 20–25 с: **20 / 20** call string в комнате, **17** с входящим audio+video,
  суммарно `audio_rx=27402`, `video_rx=23001`, `rejected=50`;
  у одного вызова `video_rx` и `decoded_video_frames` > 0, H.264 1280×720@30,
  `encoded_frames` растут; аудио PCMU 8 кГц, RTP tx/rx идут.
- В веб-UI TrueConf активная конференция видна (9 участников на момент снимка),
  затем сервер сам прислал BYE всем 20 вызовам (конец конференции на стороне
  TrueConf); наши вызовы корректно завершились `Remote SIP BYE received`.

## Найденный и исправленный interop-баг

- TrueConf отвечает `PCMU`, но шлёт RTP с PT **8 (PCMA)**; `RtpSource` требовал
  точного PT и отбрасывал аудио (`rejected_packets` рос, звука не было).
- Исправлено: G.711 PCMU/PCMA взаимозаменяемы (0↔8), декодирование по PT пакета;
  для видео/динамических PT по-прежнему требуется точное совпадение.
- Второй баг: после первой правки видео-PT перестали приниматься; исправлено
  условием `pt != expected && !g711_interchange`, видео снова идёт.
- Тест `accepts_alternate_g711_payload_and_reports_actual_pt`.

## Ограничение

- Наша MCU пока **UAC-only**: в `vks-gateway-sip` `register_uas()` — только флаг,
  UDP/TCP listener на 5060 и H.323 listener на 1720 не подняты. Входящий вызов
  с другой MCU/терминала на `sip:...@192.168.1.93` сейчас не принимается.
  Для полного закрытия нужен SIP UAS (INVITE→100/180/200+SDP→ACK/BYE, RTP bridge)
  и H.323 Q.931/H.245 listener — отдельная задача T07c.

## Гейты

```text
cargo test -p vks-server --lib reports_actual_pt → 1 passed
```

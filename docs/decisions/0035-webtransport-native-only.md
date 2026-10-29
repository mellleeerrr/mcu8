# ADR-0035. WebTransport — нативные клиенты; браузер отложен

Статус: accepted (2026-10-29, T13)
Связано: ADR-0001 (Rust), ADR-0005 (protobuf), ADR-0018 (proto_version),
`docs/architecture/SIGNALING_PROTOCOL.md` §1–2, `docs/audit/2026-10-29/00_MASTER_PLAN.md` T13,
`docs/audit/2026-10-29/04_infra_l5l7.md` §4.1

## Контекст

R5 требует второй сигнальный транспорт рядом с WS/WSS. Заявка предлагала
WebTransport «для браузеров» на `wtransport`. Аудит 04 показал:

- `wtransport 0.7.2` реализует QUIC/HTTP3-сервер и клиент, но браузер
  (Chromium) принимает самоподписанный сертификат только через
  `serverCertificateHashes`: ECDSA P-256, срок ≤14 дней, хэш SHA-256 DER
  известен клиенту заранее. Это отдельный UX/деплой-контур (provisioning,
  ротация каждые две недели), а не «ещё один URL».
- Нативные клиенты (SDK на Rust) могут либо доверять операторскому CA, либо
  пинить тот же SHA-256. Им WT даёт UDP/QUIC вместо TCP/WS — это и есть
  practically проверяемая ценность T13.
- SDP/SDP-answer ~1–3 КБ не влезает в QUIC-датаграмму (~1200 B). Значит,
  сигнальные конверты должны идти по reliable bi-stream; датаграммы — только
  для будущих ICE/пингов.
- `Endpoint::server` в 0.7 синхронный, `accept_bi` — на `Connection`; старые
  примеры (`Endpoint::server(...).await`, `accept_bi` на endpoint) не
  компилируются. Аутентификацию нужно проверять **до** `request.accept()`,
  иначе любой UDP-пир создаёт сессию и держит память.

## Решение

1. **T13 реализует native-only WebTransport.** Сервер: `wtransport 0.7.2`
   (ring, без cmake), TLS 1.3, PEM identity оператора, feature
   `vks-server/webtransport` + env `VKS_WEBTRANSPORT=1`. Без флага — `/wt`
   404, Alt-Svc нет, WS-only приложение не меняется.
2. **Браузерный WT в этом релизе не заявляется.** Для него нужен
   `serverCertificateHashes`-контур (≤14-дневный ECDSA-сертификат и его
   доставка в JS) и UDP/QUIC на пути; это отдельная задача, а не «включить
   флаг». `GET /wt/cert-hash` уже отдаёт нужное значение SHA-256 DER, чтобы
   контур можно было добавить без изменения серверной части.
3. **Только bi-stream для сигналинга.** Кадр: `u32` big-endian length +
   protobuf `SignalingMessage`, лимит 64 КиБ (как WS). Датаграммы не
   используются для SDP; заявлять их как сигнальный путь нельзя.
4. **Fail-closed.** Cert/key обязательны (никакого self-signed fallback в
   production), malformed env/битый PEM — exit 78. Пред-accept guards: путь
   `/wt`, JWT-проверка (тот же `AppState::verify_token`, что у `/ws`),
   потолок соединений, таймауты handshake/idle. После accept — лимит кадра.
5. **Alt-Svc — только при включённом транспорте.** `h3=":<actual port>";
   ma=86400` добавляется middleware ко всем HTTP-ответам (включая
   `/healthz`); при off заголовка нет. `/wt/cert-hash` отдаёт SHA-256
   листового DER-сертификата и фактический UDP-порт (`:0` → реальный).

## Последствия

- T13 проверяется реальным wtransport-клиентом in-process: QUIC handshake,
   pinning по SHA-256, `ClientHello → ServerHello` (+`room_mode`), отказ при
   чужом хэше/токене, `404` и отсутствие Alt-Svc при off, WS-регрессия.
- Браузерный WT остаётся P2-долгом; до его закрытия `Alt-Svc` не делает WT
   доступным из браузера сам по себе (нужен ещё JS `serverCertificateHashes`).
- Открытый UDP-порт 8443 требует firewall/allowlist; reverse proxy его не
  проксирует. Нативные клиенты должны знать хэш сертификата (TOFU/пиннинг).
- `sha2 0.11`/`thiserror 2` из `wtransport` — новые дубли в lock; это цена
  за pinned 0.7.2, отражено в отчёте задачи.

## Документация, изученная перед решением

- <https://docs.rs/wtransport/0.7.2/wtransport/>
- <https://docs.rs/wtransport/0.7.2/wtransport/struct.Identity.html>
- <https://docs.rs/wtransport/0.7.2/wtransport/struct.ServerConfig.html>
- <https://docs.rs/wtransport/0.7.2/wtransport/config/struct.ClientConfigBuilder.html#method.with_server_certificate_hashes>
- <https://www.w3.org/TR/webtransport/#dom-webtransportoptions-servercertificatehashes>
- `docs/audit/2026-10-29/04_infra_l5l7.md` §4.1 (API 0.7.2, ошибки примеров,
  лимиты, DoS-поверхность)

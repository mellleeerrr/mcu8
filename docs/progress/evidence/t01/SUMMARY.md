# T01 — production fail-closed evidence

Дата: 2026-10-21, HEAD 108df90 + рабочее дерево T01.

## Gates
```
$ cargo fmt --all -- --check
exit 0
$ cargo clippy --workspace --all-targets -- -D warnings
exit 0
$ cargo test --workspace --all-targets
TOTAL 970 passed; 0 failed; 6 ignored
```

## Prod fail-closed
```
$ env -i PATH=$PATH target/debug/vks-server --config configs/prod.example.toml
exit 78
ERROR configuration rejected: insecure defaults are disabled but jwt_secret is shorter than 32 bytes or is the published dev secret
```

## Weak DB rotation
```
$ cp vks_data.db /tmp/t01-live.db  # копия, живая БД не тронута
$ VKS_ADMIN_PASSWORD=StrongPass123456 VKS_JWT_SECRET=<48> VKS_DATABASE_URL=sqlite:///tmp/t01-live.db ...
WARN rotated weak operator password from VKS_ADMIN_PASSWORD and persisted it login=admin
INFO vks-server ready and listening 127.0.0.1:18083
$ env -u VKS_ADMIN_PASSWORD ...  # рестарт без env
INFO vks-server ready and listening 127.0.0.1:18083
```

## Placeholder DB (CHANGE_ME) in prod
```
$ # dev DB created with VKS_ADMIN_PASSWORD=CHANGE_ME_STRONG_PASSWORD
$ VKS_JWT_SECRET=<48> VKS_DATABASE_URL=sqlite:///tmp/t01-placeholder.db ... prod.example.toml
ERROR operator policy rejected startup: stored operator password is a known dev default
exit 78
```
(до фикса: `operator credentials restored` + старт — P1 закрыт)

## Weak DB without env
```
ERROR stored operator password is a known dev default; set a strong VKS_ADMIN_PASSWORD once ...
exit 78
```

## Unit
```
$ systemd-analyze verify deploy/systemd/vks-mcu-prod.service
vks-mcu-prod.service: Command /opt/vks/bin/vks-server is not executable (ожидаемо: root нет, не установлено)
$ systemd-analyze security --offline deploy/systemd/vks-mcu-prod.service
4.3 OK
user-unit smoke: LoadCredential + %d/server.toml -> active, /healthz 200, Result=success
```

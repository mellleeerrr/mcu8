# T9 — гигиена, конфиги, мёртвый код

Дата: 2026-10-22. База: `726e3f2`. Аудит read-only (subagent) + ручная проверка.

## Конфиги (прочитаны полностью через `cat`)
- `configs/default.toml` — используется живым user-unit `vks-mcu.service`
  (`ExecStart ... --config /home/pro/mcu/configs/default.toml`), `scripts/run-local.sh`,
  README и тестом `config.rs`. Оставлен.
- `configs/prod.example.toml` — production-шаблон, используется `deploy/README.md`,
  `deploy/systemd/vks-mcu-prod.service` (`LoadCredential`), `config_failclosed.rs`.
  Оставлен.
- Других/дублирующих конфигов нет; удалять нечего.

## Мёртвый код/модули
- `cargo machete`, `cargo udeps`, `cargo deny` не установлены; `cargo deny` в CI
  остаётся внешним action.
- Граф модулей: 171 `.rs`, все `mod`-цели существуют; не-module `.rs` — легитимные
  bin/integration-test/архивный demo. TODO/FIXME/`todo!`/`unimplemented!` в
  production-коде нет.
- 10 `#[allow(dead_code)]` — осознанные storage/engine/BWE/тестовые поля, не
  доказывают removable-код.
- Кандидаты `qoe`/`slo`/`vks_core::Session`, `now_*`, `csv_escape` отложены в T13:
  публичные API и расходящееся поведение, удаление в T9 рискованно.

## Мусор и фронтенд
- `git status --porcelain` — 0 untracked; все ignored-записи покрыты `.gitignore` (14 до удаления, 13 после: `scripts/__pycache__` исчез, `python/__pycache__` пересоздан запуском Python).
- Удалены только регенерируемые `scripts/__pycache__/` и
  `crates/vks-gateway-h323/python/__pycache__/` (`rm -rf` с проверкой realpath).
- `client/pnm-frontend/deployed/pnm`: 212 файла, `MANIFEST.sha256` 212/212 OK;
  32 legacy-bundle обязательны (`index.html` nomodule), дубли иконок — vendor 1:1,
  оставлены.
- Не удалялись: `vks_data.db` и бэкапы/lock/log, `target/`, `artifacts/`,
  `client/pnm-frontend/archive/`, оба venv, deploy, migrations, schemas,
  `h323_asn.py`, evidence/archive, playwright cache.

## Проверка
- `git status --porcelain` пуст после удаления (оба `__pycache__` ignored).
- `git check-ignore -v` подтверждает покрытие всех ignored-путей.
- `(cd client/pnm-frontend/deployed/pnm && sha256sum -c ../../MANIFEST.sha256)` — OK.
- Live `/healthz`, `/readyz`, `/pnm/monitoring/sessions` — 200 (после T10 restart).

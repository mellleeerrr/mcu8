#!/usr/bin/env python3
"""Общее ядро патч-скриптов PNM: проверяемые правила + атомарная запись.

Зачем модуль: оба скрипта (`patch-pnm-chartjs.py`, `patch-pnm-audio-in-layouts.py`)
правят минифицированные вендорские чанки, и оба обязаны вести себя одинаково:

* **fail-closed**: если якорь «до» не найден в ожидаемом количестве, вендор
  изменился ⇒ `ok=False` и `exit != 0`, а не «молча ничего не сделали»;
* **идемпотентность**: повторный запуск на уже пропатченном бандле ничего не
  пишет и завершается успехом (якорь «после» уже на месте);
* **атомарность**: запись через временный файл + `os.replace`; если `node`
  доступен и синтаксис сломан — файл откатывается к исходному содержимому;
* **`--check`**: проверить состояние, ничего не записывая (для CI/импорта).

Правило — последовательность из 5 или 6 элементов:

    (name, old, new, expected_old, expected_new[, limit])

* `expected_old` — сколько раз якорь «до» обязан встретиться в текущем тексте
  (текст на этом шаге уже содержит правки предыдущих правил);
* `expected_new` — сколько раз якорь «после» обязан встретиться в тексте ПОСЛЕ
  применения правила; нужен, чтобы отличить «уже применено» от «вендор
  испортил якорь»;
* `limit` — сколько вхождений заменять (0/отсутствует = все). Нужен для
  бандлов, где одинаковый минифицированный метод встречается в нескольких
  компонентах, а патчить надо только первый.
"""
from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
from pathlib import Path


def atomic_write(path: Path, text: str) -> None:
    """Пишет файл атомарно: временный файл рядом + `os.replace`."""
    # mkstemp создаёт 0600; для вендорского рантайма это 403 под nginx/другим
    # пользователем — сохраняем исходный режим (или 0644 для нового файла).
    try:
        mode = path.stat().st_mode & 0o7777
    except FileNotFoundError:
        mode = 0o644
    fd, name = tempfile.mkstemp(dir=str(path.parent), prefix=path.name + ".", suffix=".tmp")
    tmp = Path(name)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="") as fh:
            fh.write(text)
        os.chmod(tmp, mode)
        os.replace(tmp, path)
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise


def node_syntax_ok(path: Path) -> tuple[bool, str]:
    """Проверка синтаксиса бандла (`node --check`). `node` может отсутствовать."""
    node = shutil.which("node")
    if not node:
        return True, "node не найден — проверка синтаксиса пропущена"
    proc = subprocess.run([node, "--check", str(path)], capture_output=True, text=True, check=False)
    if proc.returncode == 0:
        return True, "node --check: OK"
    return False, "node --check: ОШИБКА\n" + (proc.stderr or proc.stdout).strip()[:800]


def _unpack(rule) -> tuple[str, str, str, int, int, int]:
    """Нормализует правило к `(name, old, new, expected_old, expected_new, limit)`."""
    name, old, new, expected_old, expected_new, *rest = rule
    limit = (rest[0] if rest else 0) or expected_old
    return name, old, new, expected_old, expected_new, limit


def apply_rules(path: Path, rules, check_only: bool = False) -> tuple[bool, list[str]]:
    """Применяет правила к файлу. Возвращает `(ok, сообщения)`.

    «Уже применено» ⇔ якоря «до» осталось ровно `expected_old - limit`
    вхождений И якорь «после» присутствует не меньше `expected_new` раз.
    Любое другое расхождение — «вендор изменился» ⇒ `ok=False`.
    """
    messages: list[str] = []
    if not path.is_file():
        return False, ["    ОШИБКА: файл не найден: %s" % path]
    original = path.read_text(encoding="utf-8")
    text = original
    for rule in rules:
        name, old, new, expected_old, expected_new, limit = _unpack(rule)
        count_old = text.count(old)
        count_new = text.count(new)
        if count_old == expected_old:
            text = text.replace(old, new, limit)
            verb = "проверено (в памяти)" if check_only else "применено"
            messages.append("    %s: %s (old x%s)" % (verb, name, count_old))
            continue
        if count_old == expected_old - limit and count_new >= expected_new:
            messages.append("    уже применено: %s" % name)
            continue
        messages.append(
            "    ОШИБКА: %s — якорь «до» встречается %s раз(а), ожидалось %s,"
            " якорь «после» — %s раз(а), ожидалось %s; вендор изменился,"
            " патч требует ревизии" % (name, count_old, expected_old, count_new, expected_new)
        )
        return False, messages
    if text == original:
        messages.append("    БЕЗ ИЗМЕНЕНИЙ: %s уже в нужном состоянии" % path.name)
        return True, messages
    if check_only:
        messages.append("    НЕ ПРИМЕНЕНО (--check): %s требует патча" % path.name)
        return False, messages
    atomic_write(path, text)
    syntax_ok, syntax_msg = node_syntax_ok(path)
    if not syntax_ok:
        # Сломанный бандл хуже непропатченного: возвращаем исходное содержимое.
        atomic_write(path, original)
        messages.append("    ОТКАТ: синтаксис не прошёл проверку, файл восстановлен")
        messages.append("    %s" % syntax_msg)
        return False, messages
    messages.append(
        "    ЗАПИСАНО: %s (%s → %s байт); %s"
        % (path.name, len(original.encode("utf-8")), len(text.encode("utf-8")), syntax_msg)
    )
    return True, messages

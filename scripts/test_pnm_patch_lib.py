#!/usr/bin/env python3
"""Тесты общего ядра патч-скриптов PNM (`scripts/pnm_patch_lib.py`).

Запуск: `cd scripts && python -m unittest -v test_pnm_patch_lib`
(та же конвенция, что у python-тестов h323-sidecar).
"""
from __future__ import annotations

import tempfile
import unittest
from pathlib import Path
from unittest import mock

from pnm_patch_lib import apply_rules, atomic_write

# Мини-аналоги вендорских якорей. Синтаксис после патча обязан остаться
# валидным JS: ядро прогоняет `node --check` и откатывает битую запись.
RULES = (
    ("axes-y", "AAA:1", "AA1:1", 2, 2),
    ("grid-lines", "CCC:", "DDD:", 1, 1),
)
CHUNK = "var a={AAA:1,b:{AAA:1},CCC:!0};"


class PatchLibTest(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def write(self, text: str, name: str = "chunk.js") -> Path:
        path = self.root / name
        path.write_text(text, encoding="utf-8")
        return path

    def test_applies_anchors_and_writes(self) -> None:
        path = self.write(CHUNK)
        ok, messages = apply_rules(path, RULES)
        self.assertTrue(ok, messages)
        patched = path.read_text(encoding="utf-8")
        self.assertNotIn("AAA:1", patched)
        self.assertIn("AA1:1", patched)
        self.assertIn("DDD:!0", patched)
        self.assertTrue(any("применено" in m for m in messages))

    def test_is_idempotent(self) -> None:
        path = self.write(CHUNK)
        self.assertTrue(apply_rules(path, RULES)[0])
        first = path.read_text(encoding="utf-8")
        ok, messages = apply_rules(path, RULES)
        self.assertTrue(ok, messages)
        self.assertEqual(path.read_text(encoding="utf-8"), first, "второй прогон не пишет")
        self.assertTrue(any("уже применено" in m or "БЕЗ ИЗМЕНЕНИЙ" in m for m in messages))

    def test_check_mode_reports_patch_needed_without_writing(self) -> None:
        path = self.write(CHUNK)
        ok, messages = apply_rules(path, RULES, check_only=True)
        self.assertFalse(ok)
        self.assertEqual(path.read_text(encoding="utf-8"), CHUNK)
        self.assertTrue(any("--check" in m for m in messages))

    def test_check_mode_accepts_already_patched(self) -> None:
        path = self.write(CHUNK)
        self.assertTrue(apply_rules(path, RULES)[0])
        ok, messages = apply_rules(path, RULES, check_only=True)
        self.assertTrue(ok, messages)

    def test_vendor_drift_fails_closed_and_keeps_file(self) -> None:
        drifted = "var a={AAA:1,CCC:!0};"  # одно вхождение вместо двух
        path = self.write(drifted)
        ok, messages = apply_rules(path, RULES)
        self.assertFalse(ok, "вендор изменился — это ошибка, а не «ничего не сделали»")
        self.assertTrue(any("ОШИБКА" in m for m in messages))
        self.assertEqual(path.read_text(encoding="utf-8"), drifted)

    def test_missing_file_fails(self) -> None:
        ok, messages = apply_rules(self.root / "нет-такого.js", RULES)
        self.assertFalse(ok)
        self.assertTrue(any("не найден" in m for m in messages))

    def test_limited_rule_patches_only_first_occurrence(self) -> None:
        # Два одинаковых минифицированных метода, патчить надо только первый.
        text = "function f(){return 1}function f(){return 1}"
        path = self.write(text)
        rules = (("data-watcher", "function f(){return 1}", "function f(){return 2}", 2, 1, 1),)
        ok, messages = apply_rules(path, rules)
        self.assertTrue(ok, messages)
        patched = path.read_text(encoding="utf-8")
        self.assertEqual(patched.count("return 2"), 1)
        self.assertEqual(patched.count("return 1"), 1, "второе вхождение не тронуто")
        ok, messages = apply_rules(path, rules)
        self.assertTrue(ok, messages)
        self.assertTrue(any("уже применено" in m for m in messages))

    def test_syntax_failure_rolls_back(self) -> None:
        path = self.write(CHUNK)
        with mock.patch("pnm_patch_lib.node_syntax_ok", return_value=(False, "node --check: ОШИБКА")):
            ok, messages = apply_rules(path, RULES)
        self.assertFalse(ok)
        self.assertTrue(any("ОТКАТ" in m for m in messages))
        self.assertEqual(path.read_text(encoding="utf-8"), CHUNK, "битый бандл не остаётся на диске")

    def test_atomic_write_leaves_no_temp_files(self) -> None:
        path = self.write("старое")
        atomic_write(path, "новое")
        self.assertEqual(path.read_text(encoding="utf-8"), "новое")
        self.assertEqual([p.name for p in self.root.iterdir()], ["chunk.js"])


if __name__ == "__main__":
    unittest.main()

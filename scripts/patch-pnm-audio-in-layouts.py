#!/usr/bin/env python3
"""Идемпотентно переносит вкладку «Аудиоканал(ы)» внутрь вкладки «Раскладки».

Заказчик (T15): «перемести вкладку аудиоканалы во вкладку раскладки».
Перестановка верхнего ряда бессмысленна — «Аудиоканал» и так стоит сразу
после «Раскладок» и на странице конференции, и на странице сессии. Поэтому
вкладка исчезает из верхнего ряда, а её содержимое переезжает ВНУТРЬ панели
вкладки «Раскладки».

Что делает патч (правки вендорских чанков index6 / index-legacy6):

  1. Пункт «Аудиоканал(ы)» в tabList получает флаг `hidden:!0`.
     TcTabs (index.bundle) фильтрует вкладки как `this.tabs.filter(e => !e.hidden)`
     ⇒ кнопка не рендерится вообще: MDC-таб-бар её не видит, индексы/aria/
     клавиатура не разъезжаются, «залипшего» mdc-tab--active не бывает.
  2. Панель вкладки «Раскладки» (компонент LayoutTab) получает вторым ребёнком
     `main-card-body__container` панель аудиоканалов ⇒ аудиоканалы видны
     ВНУТРИ активной вкладки «Раскладки».
  3. `currentComponent`: значение "Audio" открывает тот же объединённый
     LayoutTab — страховка от «залипшего» состояния стора (иначе пустая карточка).
  4. Таб-бару передаётся modelValue "Layouts", если состояние "Audio"
     ⇒ активная вкладка всегда совпадает с видимой панелью.
  5. Возврат из отдельной страницы редактора аудиоканала («Назад»/«Отмена»/
     «Создать») ведёт на «Раскладки» — по одному site-уникальному якорю на
     каждое место (3 в конференции + 3 в сессии).

Запуск:
    python3 scripts/patch-pnm-audio-in-layouts.py            # применить
    python3 scripts/patch-pnm-audio-in-layouts.py --check    # только проверить
    python3 scripts/patch-pnm-audio-in-layouts.py --root DIR  # другой рантайм
    python3 scripts/patch-pnm-audio-in-layouts.py --modern-only

Свойства:
  * идемпотентен: повторный запуск ничего не меняет (файл не переписывается);
  * проверка ПО ПРАВИЛУ: «уже применено» означает «якоря "до" нет, а якорь
    "после" есть в нужном количестве» — глобального маркера недостаточно
    (он доказывал бы применение только одного правила и маскировал бы
    частично пропатченный/подменённый вендор);
  * «шумный» при смене вендора: число вхождений каждого якоря проверяется,
    при несовпадении — exit != 0 и импорт прерывается;
  * запись атомарная (временный файл + os.replace) + проверка синтаксиса
    `node --check`; при ошибке синтаксиса файл возвращается в исходное
    состояние;
  * проверяет наличие CSS-override геометрии мержа в index.html (без него
    панель делится 50/50, а блок аудиоканалов занимает пол-экрана).
"""
from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CHUNKS_REL = "client/pnm-frontend/deployed/pnm/chunks"
INDEX_REL = "client/pnm-frontend/deployed/pnm/index.html"
MODERN = "index6.bundle-2.2.1.66.js"
LEGACY = "index-legacy6.bundle-2.2.1.66.js"

# Обязательные куски CSS-override геометрии мержа (в index.html).
# Проверяется не просто наличие блока, а ЯКОРЯ ПОЛИТИКИ: устаревший override
# (например, без max-height или с возвращённой иллюстрацией 240×240) дал бы
# «зелёный» --check, хотя панель делилась бы 50/50 или кнопка «Добавить»
# уходила бы за нижний край.
# T16: обе иллюстрации пустых состояний (раскладки + аудиоканалы) должны быть
# РОВНО ВДВОЕ меньше вендорских 240×240, то есть 120×120; прежний якорь
# "display: none" (иллюстрация аудиоканалов была скрыта) больше не существует.
#
# ВАЖНО (аудит T16): якорь проверяется как ЦЕЛОЕ ПРАВИЛО (селектор + объявления),
# а не как отдельная подстрока. С подстроками ("width: 120px") частично
# устаревший override проходил --check «зелёным»: например, возврат иллюстрации
# РАСКЛАДОК к 240×240 не ловился — подстрока "width: 120px" оставалась в
# соседнем правиле иллюстрации аудиоканалов. Сравнение выполняется по
# нормализованным пробелам (переносы строк и отступы не важны, а значения — важны).
INDEX_CSS_REQUIRED = (
    # блок аудиоканалов в объединённой панели (конференция и сессия)
    ".main-card-body__container > .main-card-body.conference-audio-channels",
    ".main-card-body__container > .main-card-body.session-audio-channels",
    # геометрия блока аудиоканалов: не растёт, потолок 320px
    (
        ".main-card-body__container > .main-card-body.conference-audio-channels,"
        " .main-card-body__container > .main-card-body.session-audio-channels"
        " { flex: 0 0 auto; min-height: 0; max-height: 320px;"
    ),
    # T16: компенсация высоты — вертикальный padding блока аудиоканалов 12px
    "padding-top: 12px; padding-bottom: 12px;",
    # правило :has() — список раскладок растягивается на остаток панели
    ":has(> .main-card-body.conference-audio-channels) > .main-card-body:first-child",
    # T16: иллюстрация пустого состояния списка РАСКЛАДОК — ровно 120×120
    (
        ".main-card-body__container > .main-card-body.layouts .main-blank__image"
        " { width: 120px; height: 120px; }"
    ),
    # T16: иллюстрация пустого состояния списка АУДИОКАНАЛОВ — ровно 120×120
    (
        ".main-card-body__container > .main-card-body.conference-audio-channels"
        " .main-blank__image,"
        " .main-card-body__container > .main-card-body.session-audio-channels"
        " .main-blank__image { width: 120px; height: 120px; }"
    ),
)

# ── Якоря возврата из редактора аудиоканала ──────────────────────────────────
# Каждое место — уникальная строка (включая имя обработчика), поэтому её
# можно проверить и по «до», и по «после». Общие для modern и legacy сборок:
# в обоих минификатор оставил одинаковые строки компонента AudioChannelCardHeader.
BACK_CONFERENCE = (
    (
        "conf-back-go-back",
        'goBackHandler(){this.$router.push({name:"monitoring-conferences",params:{conferenceId:this.conferenceId}}).then(()=>{this.conferenceCurrentTab="Audio"})}',
        'goBackHandler(){this.$router.push({name:"monitoring-conferences",params:{conferenceId:this.conferenceId}}).then(()=>{this.conferenceCurrentTab="Layouts"})}',
    ),
    (
        "conf-back-cancel",
        'cancelHandler(){this.$router.push({name:"monitoring-conferences",params:{conferenceId:this.conferenceId}}).then(()=>{this.conferenceCurrentTab="Audio"})}',
        'cancelHandler(){this.$router.push({name:"monitoring-conferences",params:{conferenceId:this.conferenceId}}).then(()=>{this.conferenceCurrentTab="Layouts"})}',
    ),
    (
        "conf-back-after-create",
        'finally{this.saveAudioChannelLoading=!1,this.$router.push({name:"monitoring-conferences",params:{conferenceId:this.conferenceId}}).then(()=>{this.conferenceCurrentTab="Audio"})}',
        'finally{this.saveAudioChannelLoading=!1,this.$router.push({name:"monitoring-conferences",params:{conferenceId:this.conferenceId}}).then(()=>{this.conferenceCurrentTab="Layouts"})}',
    ),
)
BACK_SESSION = (
    (
        "sess-back-go-back",
        'goBackHandler(){this.$router.push({name:"monitoring-sessions",params:{sessionId:this.sessionId},query:this.$route.query}).then(()=>{this.sessionCurrentTab="Audio"})}',
        'goBackHandler(){this.$router.push({name:"monitoring-sessions",params:{sessionId:this.sessionId},query:this.$route.query}).then(()=>{this.sessionCurrentTab="Layouts"})}',
    ),
    (
        "sess-back-cancel",
        'cancelHandler(){this.$router.push({name:"monitoring-sessions",params:{sessionId:this.sessionId},query:this.$route.query}).then(()=>{this.sessionCurrentTab="Audio"})}',
        'cancelHandler(){this.$router.push({name:"monitoring-sessions",params:{sessionId:this.sessionId},query:this.$route.query}).then(()=>{this.sessionCurrentTab="Layouts"})}',
    ),
    (
        "sess-back-after-create",
        'finally{this.saveAudioChannelLoading=!1,this.$router.push({name:"monitoring-sessions",params:{sessionId:this.sessionId},query:this.$route.query}).then(()=>{this.sessionCurrentTab="Audio"})}',
        'finally{this.saveAudioChannelLoading=!1,this.$router.push({name:"monitoring-sessions",params:{sessionId:this.sessionId},query:this.$route.query}).then(()=>{this.sessionCurrentTab="Layouts"})}',
    ),
)

# Правило: (имя, до, после, ожидаемое число «до», ожидаемое число «после»).
# Современная сборка: s()=openBlock, m()=createElementBlock, i()=createElementVNode,
# b()=createBlock, v()=createCommentVNode, Se()=resolveDynamicComponent.
MODERN_RULES = [
    (
        "conf-tab-hidden",
        '{value:"Audio",text:this.$pgettext("Tab","Audio channel"),qaSelector:"conference-audio-tab"}',
        '{value:"Audio",text:this.$pgettext("Tab","Audio channel"),qaSelector:"conference-audio-tab",hidden:!0}',
        1,
        1,
    ),
    (
        "sess-tab-hidden",
        '{value:"Audio",qaSelector:"session-main-tabs-audio",text:this.$pgettext("The title of a tab where a user will configure audio channel settings. For example, the user will be able to select the participants who will send or receive audio during a video meeting.","Audio channels")}',
        '{value:"Audio",qaSelector:"session-main-tabs-audio",hidden:!0,text:this.$pgettext("The title of a tab where a user will configure audio channel settings. For example, the user will be able to select the participants who will send or receive audio during a video meeting.","Audio channels")}',
        1,
        1,
    ),
    (
        "conf-model-maps-audio-to-layouts",
        'modelValue:e.conferenceCurrentTab,"onUpdate:modelValue":',
        'modelValue:e.conferenceCurrentTab==="Audio"?"Layouts":e.conferenceCurrentTab,"onUpdate:modelValue":',
        1,
        1,
    ),
    (
        "sess-model-maps-audio-to-layouts",
        'modelValue:e.sessionCurrentTab,"onUpdate:modelValue":',
        'modelValue:e.sessionCurrentTab==="Audio"?"Layouts":e.sessionCurrentTab,"onUpdate:modelValue":',
        1,
        1,
    ),
    (
        "conf-layouts-panel-contains-audio",
        'e.showConferenceLayoutPopup.remove?(s(),b(f,{key:2,onClose:t[0]||(t[0]=r=>e.closeConferenceLayoutPopup("remove"))})):v("",!0)])])}',
        'e.showConferenceLayoutPopup.remove?(s(),b(f,{key:2,onClose:t[0]||(t[0]=r=>e.closeConferenceLayoutPopup("remove"))})):v("",!0)]),(s(),b(wp,{key:3}))])}',
        1,
        1,
    ),
    (
        "sess-layouts-panel-contains-audio",
        'e.showSessionLayoutPopup.remove?(s(),b(f,{key:2,onClose:t[0]||(t[0]=r=>e.closeSessionLayoutPopup("remove"))})):v("",!0)])])}',
        'e.showSessionLayoutPopup.remove?(s(),b(f,{key:2,onClose:t[0]||(t[0]=r=>e.closeSessionLayoutPopup("remove"))})):v("",!0)]),(s(),b(PS,{key:3}))])}',
        1,
        1,
    ),
    (
        "conf-currentcomponent-audio-is-layouts",
        "Layouts:iy,Audio:wp,",
        "Layouts:iy,Audio:iy,",
        1,
        1,
    ),
    (
        "sess-currentcomponent-audio-is-layouts",
        "Layouts:PT,Audio:PS,",
        "Layouts:PT,Audio:PT,",
        1,
        1,
    ),
] + [(n, o, w, 1, 1) for n, o, w in BACK_CONFERENCE + BACK_SESSION]

# Legacy-сборка: U()=openBlock, F()=createElementBlock, O()=createElementVNode,
# Y()=createBlock, G()=createCommentVNode; аудио-компоненты — Jr (конф.) и sv (сессия).
LEGACY_RULES = [
    (
        "conf-tab-hidden",
        '{value:"Audio",text:this.$pgettext("Tab","Audio channel"),qaSelector:"conference-audio-tab"}',
        '{value:"Audio",text:this.$pgettext("Tab","Audio channel"),qaSelector:"conference-audio-tab",hidden:!0}',
        1,
        1,
    ),
    (
        "sess-tab-hidden",
        '{value:"Audio",qaSelector:"session-main-tabs-audio",text:this.$pgettext("The title of a tab where a user will configure audio channel settings. For example, the user will be able to select the participants who will send or receive audio during a video meeting.","Audio channels")}',
        '{value:"Audio",qaSelector:"session-main-tabs-audio",hidden:!0,text:this.$pgettext("The title of a tab where a user will configure audio channel settings. For example, the user will be able to select the participants who will send or receive audio during a video meeting.","Audio channels")}',
        1,
        1,
    ),
    (
        "conf-model-maps-audio-to-layouts",
        'modelValue:e.conferenceCurrentTab,"onUpdate:modelValue":',
        'modelValue:e.conferenceCurrentTab==="Audio"?"Layouts":e.conferenceCurrentTab,"onUpdate:modelValue":',
        1,
        1,
    ),
    (
        "sess-model-maps-audio-to-layouts",
        'modelValue:e.sessionCurrentTab,"onUpdate:modelValue":',
        'modelValue:e.sessionCurrentTab==="Audio"?"Layouts":e.sessionCurrentTab,"onUpdate:modelValue":',
        1,
        1,
    ),
    (
        "conf-layouts-panel-contains-audio",
        'e.showConferenceLayoutPopup.remove?(U(),Y(l,{key:2,onClose:t[0]||(t[0]=t=>e.closeConferenceLayoutPopup("remove"))})):G("",!0)])])}',
        'e.showConferenceLayoutPopup.remove?(U(),Y(l,{key:2,onClose:t[0]||(t[0]=t=>e.closeConferenceLayoutPopup("remove"))})):G("",!0)]),(U(),Y(Jr,{key:3}))])}',
        1,
        1,
    ),
    (
        "sess-layouts-panel-contains-audio",
        'e.showSessionLayoutPopup.remove?(U(),Y(l,{key:2,onClose:t[0]||(t[0]=t=>e.closeSessionLayoutPopup("remove"))})):G("",!0)])])}',
        'e.showSessionLayoutPopup.remove?(U(),Y(l,{key:2,onClose:t[0]||(t[0]=t=>e.closeSessionLayoutPopup("remove"))})):G("",!0)]),(U(),Y(sv,{key:3}))])}',
        1,
        1,
    ),
    (
        "conf-currentcomponent-audio-is-layouts",
        "Layouts:fp,Audio:Jr,",
        "Layouts:fp,Audio:fp,",
        1,
        1,
    ),
    (
        "sess-currentcomponent-audio-is-layouts",
        "Layouts:CI,Audio:sv,",
        "Layouts:CI,Audio:CI,",
        1,
        1,
    ),
] + [(n, o, w, 1, 1) for n, o, w in BACK_CONFERENCE + BACK_SESSION]


def atomic_write(path: Path, text: str) -> None:
    """Пишет файл атомарно: временный файл рядом + os.replace."""
    fd, tmp_name = tempfile.mkstemp(dir=str(path.parent), prefix=path.name + ".", suffix=".tmp")
    tmp = Path(tmp_name)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="") as fh:
            fh.write(text)
        os.replace(tmp, path)
    except BaseException:
        tmp.unlink(missing_ok=True)
        raise


def node_syntax_ok(path: Path) -> tuple[bool, str]:
    """Проверка синтаксиса бандла (node --check). node может отсутствовать."""
    node = shutil.which("node")
    if not node:
        return True, "node не найден — проверка синтаксиса пропущена"
    proc = subprocess.run(
        [node, "--check", str(path)], capture_output=True, text=True, check=False
    )
    if proc.returncode == 0:
        return True, "node --check: OK"
    return False, "node --check: ОШИБКА\n" + (proc.stderr or proc.stdout).strip()[:800]


def apply_rules(path: Path, rules, check_only: bool) -> tuple[bool, list[str]]:
    """Применяет правила к файлу. Возвращает (ok, сообщения).

    Проверка по правилу: «уже применено» ⇔ якоря «до» нет (0 вхождений) И
    якорь «после» присутствует в ожидаемом количестве. Иначе — «вендор
    изменился» (якорь пропал/переехал/переписан вендором) ⇒ exit != 0.
    """
    messages: list[str] = []
    if not path.is_file():
        return False, ["    ОШИБКА: файл не найден: %s" % path]
    original = path.read_text(encoding="utf-8")
    text = original
    for name, old, new, expected_old, expected_new in rules:
        count_old = text.count(old)
        count_new = text.count(new)
        if count_old == expected_old:
            text = text.replace(old, new)
            verb = "проверено (в памяти)" if check_only else "применено"
            messages.append("    %s: %s (old x%d)" % (verb, name, count_old))
            continue
        if count_old == 0 and count_new >= expected_new:
            messages.append("    уже применено: %s" % name)
            continue
        messages.append(
            "    ОШИБКА: %s — якорь «до» встречается %d раз(а), ожидалось %d,"
            " якорь «после» — %d раз(а), ожидалось %d; вендор изменился,"
            " патч требует ревизии"
            % (name, count_old, expected_old, count_new, expected_new)
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
        # Откат: возвращаем исходное содержимое — сломанный бандл хуже
        # непропатченного.
        atomic_write(path, original)
        messages.append("    ОТКАТ: синтаксис не прошёл проверку, файл восстановлен")
        messages.append("    %s" % syntax_msg)
        return False, messages
    messages.append(
        "    ЗАПИСАНО: %s (%d → %d байт); %s"
        % (
            path.name,
            len(original.encode("utf-8")),
            len(text.encode("utf-8")),
            syntax_msg,
        )
    )
    return True, messages


def _norm_css(text: str) -> str:
    """Нормализует пробелы/переносы строк — сравнение якорей не должно зависеть
    от форматирования override-блока, но значения объявлений — должны."""
    return " ".join(text.split())


def check_index_css(root: Path) -> tuple[bool, list[str]]:
    """Проверяет, что в index.html есть CSS-override геометрии мержа T15."""
    messages: list[str] = []
    index = root / INDEX_REL
    if not index.is_file():
        return False, ["    ОШИБКА: index.html не найден: %s" % index]
    text = index.read_text(encoding="utf-8")
    haystack = _norm_css(text)
    missing = [
        snippet for snippet in INDEX_CSS_REQUIRED if _norm_css(snippet) not in haystack
    ]
    if missing:
        messages.append(
            "    ОШИБКА: в index.html нет CSS-override геометрии мержа T15"
            " (не найдено: %s) — без него панель «Раскладки» делится 50/50."
            " Восстановите блок <style id=\"prostor-pnm-overrides\"> (NOTES.md, T15)"
            % "; ".join(missing)
        )
        return False, messages
    messages.append("    CSS-override геометрии мержа: найден в index.html")
    return True, messages


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(
        description="Патч PNM: вкладка «Аудиоканал(ы)» → внутрь вкладки «Раскладки»"
    )
    parser.add_argument(
        "--root", type=Path, default=ROOT, help="корень репозитория (по умолчанию — родитель scripts/)"
    )
    parser.add_argument("--check", action="store_true", help="только проверить, ничего не писать")
    parser.add_argument(
        "--modern-only", action="store_true", help="не патчить legacy-сборку (index-legacy6)"
    )
    args = parser.parse_args(argv)

    chunks = args.root / CHUNKS_REL
    print("[patch-audio-in-layouts] root=%s" % args.root)
    print("  chunks: %s" % chunks)
    ok = True

    print("  modern (%s):" % MODERN)
    file_ok, messages = apply_rules(chunks / MODERN, MODERN_RULES, args.check)
    ok = ok and file_ok
    for line in messages:
        print(line)

    if args.modern_only:
        print("  legacy: пропущен (--modern-only)")
    else:
        print("  legacy (%s):" % LEGACY)
        file_ok, messages = apply_rules(chunks / LEGACY, LEGACY_RULES, args.check)
        ok = ok and file_ok
        for line in messages:
            print(line)

    print("  index.html:")
    css_ok, messages = check_index_css(args.root)
    ok = ok and css_ok
    for line in messages:
        print(line)

    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())

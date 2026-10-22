#!/usr/bin/env node
/*
 * Живой E2E-контракт «вкладка Аудиоканал переехала в вкладку Раскладки» (PNM 2.2.1.66).
 *
 * Контракт (проверяется на странице конференции /pnm/monitoring/conferences/{id}):
 *   1. Вкладки «Аудиоканал» в таб-баре конференции БОЛЬШЕ НЕТ (ни по data-qa
 *      `conference-audio-tab`, ни по тексту «Аудиоканал»), остаются 7 вкладок
 *      в ожидаемом порядке: Информация, Основное, Расписание, Раскладки,
 *      Оформление, Запись и трансляция, Дополнительно.
 *   2. Вкладка «Раскладки» показывает ОБА блока: список раскладок (`.layouts`)
 *      И блок аудиоканалов (`.conference-audio-channels`) — блок аудиоканалов
 *      лежит ВНУТРИ панели активной вкладки (тот же контейнер, что и `.layouts`),
 *      а не в отдельной панели.
 *   3. Блок аудиоканалов не «спрятан», а переехал: он видим, единственный
 *      в документе и содержит свою функциональность (таблица каналов либо
 *      пустое состояние с кнопкой «Добавить»).
 *   4. Остальные вкладки открываются и показывают СВОЁ содержимое; у табов
 *      целы role=tab / tabindex / aria-selected (ровно один активный).
 *   5. Сетка раскладок в редакторе раскладок продолжает работать: 10 размеров
 *      (1,2,3,4,5+,8,9,10+,25,34+), посадка контакта, подпись ровно ×2 к
 *      шрифту плитки, центрирована, внутри плитки.
 *   6. pageerror = 0, console errors = 0, счётчики в шапке не изменились,
 *      НИ ОДНОЙ записи в БД (не нажимаем «Сохранить»/«Создать»/«Удалить»).
 *
 * Ветка СЕССИИ (тот же контракт, настоящий маршрут /pnm/monitoring/sessions/{id}):
 *   7. В таб-баре сессии 6 вкладок (control-panel, general, layouts, skins,
 *      record-and-stream, additional), НЕТ session-main-tabs-audio и вкладки
 *      «Аудиоканалы»; блок `.session-audio-channels` — второй ребёнок панели
 *      «Раскладки», видим, единственный, функционален.
 *   8. Инвариант «активная вкладка == видимая панель» на каждой вкладке обоих
 *      таб-баров: ровно одна aria-selected=true, и именно её содержимое
 *      отрисовано (у панели вендер не отдаёт data-qa, поэтому идентичность
 *      панели определяется по корневому классу компонента вкладки).
 *   9. Футер вкладки не изменился: на «Раскладках»/«Информации» (конференция)
 *      и «Панели управления»/«Раскладках» (сессия) футера нет, на остальных —
 *      ровно «Отмена» + «Сохранить».
 *  10. Круг «Добавить» в блоке аудиоканалов → редактор аудиоканала → «Отмена»
 *      (НЕ «Создать»): URL возвращается на страницу конференции/сессии, ровно
 *      одна вкладка с aria-selected=true (это «Раскладки»), панель непустая,
 *      блок аудиоканалов ВНУТРИ панели этой вкладки.
 *  11. Клавиатура по ТАБ-БАРУ настоящей правки: 3×ArrowRight от «Раскладок» →
 *      фокус layouts→skins→record-and-stream→additional (фокус не застревает),
 *      Enter активирует сфокусированную вкладку, панель следует за ней.
 *  12. «Сиротских» панелей нет: в активной панели ровно один .main-card-body
 *      (на «Раскладках» — ровно два: раскладки + аудиоканалы), панель непустая.
 *
 * Запуск:
 *   NODE_PATH=<node_modules с playwright> node tests/e2e/audio-in-layouts.js
 *   VKS_CHECKS_SELFTEST=1  …  САМОПРОВЕРКА ЧЕКОВ (НЕ модель реализации!):
 *                         имитируем «исправленное» состояние средствами DOM и
 *                         убеждаемся, что контрактные проверки достижимы.
 *                         DOM-хирургия ломает клавиатурный фокус MDC, поэтому
 *                         клавиатурная проверка здесь пропускается, а сам режим
 *                         НЕ может считаться доказательством поведения правки.
 *   VKS_REVERT_CHECK=1  …  режим отката: применяем «исправленное» состояние,
 *                         затем откатываем его (вкладка аудио возвращается,
 *                         блок аудиоканалов уезжает из панели раскладок) —
 *                         прогон ОБЯЗАН упасть (exit 1).
 *
 * Артефакты: artifacts/e2e-audio-in-layouts/{result.json,*.png}
 *
 * Скрипт НИЧЕГО не пишет в живую БД: раскладка не сохраняется, посадка
 * участника и добавление в «Получатели» — только клиентский черновик
 * (единственный не-GET запрос — POST .../video_layouts/calculate).
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require('playwright');

const BASE = process.env.VKS_BASE_URL || 'http://127.0.0.1:8080';
const LOGIN = process.env.VKS_LOGIN || 'admin';
const PASSWORD = process.env.VKS_PASSWORD || 'admin_password';
const CONFERENCE_ID = process.env.VKS_CONFERENCE_ID || '49b599d7-b5cd-45a7-8999-47a28c453a92';
const PARTICIPANT = process.env.VKS_PARTICIPANT || 'T12b Label Probe';
const ART = path.resolve(process.env.VKS_ARTIFACTS || 'artifacts/e2e-audio-in-layouts');
const CHROMIUM = process.env.VKS_CHROMIUM
  || '/home/pro/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome';
// Режим САМОПРОВЕРКИ ЧЕКОВ: имитировать «исправленное» состояние средствами DOM.
// ВАЖНО: это НЕ модель реализации и НЕ доказательство поведения реальной правки —
// DOM-хирургия ломает клавиатурный фокус MDC, которого нет у настоящей правки.
// (Историческое имя VKS_SIMULATE_FIX оставлено как алиас и печатает предупреждение.)
const CHECKS_SELFTEST = process.env.VKS_CHECKS_SELFTEST === '1'
  || (process.env.VKS_SIMULATE_FIX === '1' && Boolean(process.env.VKS_SIMULATE_FIX));
// Режим отката: исправленное состояние → откат; прогон обязан упасть.
const REVERT_CHECK = process.env.VKS_REVERT_CHECK === '1';
// Режим ПРОВЕРКИ ОБРЕЗКИ: возвращаем «старые» правила (flex + картинка 240px) —
// прогон обязан упасть на проверке N7 (блок аудиоканалов срезан снизу).
const CLIP_CHECK = process.env.VKS_CLIP_CHECK === '1';
// Ожидаемый таб-бар конференции ПОСЛЕ переезда вкладки аудио в «Раскладки».
// qa — вендорский data-qa из tabList (chunks/index6.bundle-2.2.1.66.js);
// marker — корневой класс содержимого вкладки (по нему проверяем, что вкладка
// показывает ИМЕННО своё содержимое, а не «что-то похожее»).
const EXPECTED_TABS = [
  { qa: 'conference-info-tab', label: 'Информация', marker: '.conference-info' },
  { qa: 'conference-general-tab', label: 'Основное', marker: '.conference-general' },
  { qa: 'conference-schedule-tab', label: 'Расписание', marker: null, text: 'Продолжительность конференции' },
  { qa: 'conference-layouts-tab', label: 'Раскладки', marker: '.layouts' },
  { qa: 'conference-skins-tab', label: 'Оформление', marker: '.conference-skins' },
  { qa: 'conference-record-and-stream-tab', label: 'Запись и трансляция', marker: '.conference-record-and-stream' },
  { qa: 'conference-additional-tab', label: 'Дополнительно', marker: '.conference-additional' },
];
const AUDIO_TAB_QA = 'conference-audio-tab';
const AUDIO_BLOCK = '.conference-audio-channels';
const SESSION_AUDIO_BLOCK = '.session-audio-channels';
const LAYOUTS_BLOCK = '.layouts';
const TAB_BAR = '.tc-tabs.main-card__tabs';
// Таб-бар СЕССИИ (тот же UUID в маршруте /pnm/monitoring/sessions/{id}).
// У вкладок сессии НЕТ своей вкладки аудиоканалов (session-main-tabs-audio).
const SESSION_ID = process.env.VKS_SESSION_ID || CONFERENCE_ID;
const SESSION_TABS = [
  { qa: 'session-main-tabs-control-panel', label: 'Панель управления', marker: '.session-control-panel' },
  { qa: 'session-main-tabs-general', label: 'Основное', marker: '.conference-general' },
  { qa: 'session-main-tabs-layouts', label: 'Раскладки', marker: '.layouts' },
  { qa: 'session-main-tabs-skins', label: 'Оформление', marker: '.conference-skins' },
  { qa: 'session-record-and-stream-tab', label: 'Запись и трансляция', marker: '.conference-record-and-stream' },
  { qa: 'session-main-tabs-additional', label: 'Дополнительно', marker: '.conference-additional' },
];
// Вкладки без футера (у конференции — «Информация» и «Раскладки»,
// у сессии — «Панель управления» и «Раскладки»).
const FOOTERLESS = {
  'conference-info-tab': true,
  'conference-layouts-tab': true,
  'session-main-tabs-control-panel': true,
  'session-main-tabs-layouts': true,
};

// Критерии геометрии подписи (те же, что в label-zoom.js).
const CENTER_DX = 1.5;
const CENTER_DY = 3;
const LABEL_FONT_RATIO = 2;
const FONT_RATIO_TOLERANCE = 0.15;

// Размеры раскладки: label — точный текст кнопки в .layout-size-range-selector
// (последняя, icon-only «overlap», НЕ трогаем).
const SIZES = [
  { size: 1, label: '1' }, { size: 2, label: '2' }, { size: 3, label: '3' }, { size: 4, label: '4' },
  { size: 5, label: '5 +', plus: true }, { size: 8, label: '8' }, { size: 9, label: '9' },
  { size: 10, label: '10 +', plus: true }, { size: 25, label: '25' }, { size: 34, label: '34 +', plus: true },
];

fs.mkdirSync(ART, { recursive: true });

const checks = [];
const pageErrors = [];
const consoleErrors = [];
const nonGetRequests = [];
const failedRequests = [];
// Ошибки, которые объясняются недоступностью сервера (рестарт стенда, обрыв
// WebSocket), а не дефектом правки: их нельзя записывать в «ошибки UI».
const ENV_ERROR_PATTERNS = [
  /WebSocket connection to .*failed/i,
  /net::ERR_CONNECTION_REFUSED/i,
  /net::ERR_CONNECTION_RESET/i,
  /net::ERR_CONNECTION_CLOSED/i,
  /net::ERR_EMPTY_RESPONSE/i,
  /Failed to fetch/i,
];
// ERR_ABORTED — это нормальная отмена запроса браузером (например, превью
// видео отменяется при переходе на другую страницу), а не ошибка приложения.
const ABORTED_PATTERN = /net::ERR_ABORTED/i;
const isEnvError = text => ENV_ERROR_PATTERNS.some(pattern => pattern.test(text));
// «Среда недоступна» = сервер не отвечает (рестарт стенда), а не отменённый
// браузером запрос.
const isServerFailure = request => /ERR_CONNECTION_REFUSED|ERR_CONNECTION_RESET|ERR_CONNECTION_CLOSED|ERR_EMPTY_RESPONSE/.test(request.error || '');
const startedAt = Date.now();
let failure = null;

function check(name, ok, detail, severity = 'fatal') {
  const entry = { name, ok: Boolean(ok), severity, detail: detail === undefined ? null : detail };
  checks.push(entry);
  const mark = entry.ok ? 'ok  ' : (severity === 'fatal' ? 'FAIL'
    : severity === 'info' ? 'info' : `WARN(${severity})`);
  console.log(`${mark} ${name}${entry.detail ? ` — ${entry.detail}` : ''}`);
  return entry.ok;
}

/** Скриншот — это свидетельство, а не контракт: падение съёмки не роняет прогон. */
async function shot(page, file, options = {}) {
  try {
    await page.screenshot({ path: path.join(ART, file), ...options });
    return true;
  } catch (error) {
    console.log(`     (скриншот ${file} не снят: ${String(error).split('\n')[0].slice(0, 120)})`);
    return false;
  }
}

const slug = (label, index) => {
  const base = String(label).trim().toLowerCase()
    .replace(/\s*\+\s*$/, '-plus').replace(/\s+/g, '-').replace(/-$/, '');
  return `${String(index).padStart(2, '0')}-${base}`;
};

/**
 * Проба таб-бара и панели активной вкладки (в браузере).
 * Панель вкладки = ребёнок .main-card-content, который не является шапкой,
 * таб-баром или футером (футер есть только у части вкладок — showFooter).
 */
const TAB_PROBE = () => {
  const visibleState = el => {
    if (!el) return { visible: false, reason: 'absent-from-dom' };
    const r = el.getBoundingClientRect();
    const cs = getComputedStyle(el);
    if (cs.display === 'none') return { visible: false, reason: 'display:none' };
    if (cs.visibility === 'hidden' || cs.visibility === 'collapse') return { visible: false, reason: `visibility:${cs.visibility}` };
    if (parseFloat(cs.opacity) === 0) return { visible: false, reason: 'opacity:0' };
    if (r.width <= 0 || r.height <= 0) return { visible: false, reason: 'zero-size-box' };
    if (el.offsetParent === null && cs.position !== 'fixed') return { visible: false, reason: 'offsetParent===null' };
    if (typeof el.checkVisibility === 'function'
      && !el.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true })) {
      return { visible: false, reason: 'checkVisibility=false' };
    }
    return { visible: true, reason: null, box: { w: +r.width.toFixed(1), h: +r.height.toFixed(1) } };
  };
  const bar = document.querySelector('.tc-tabs.main-card__tabs');
  const content = document.querySelector('.main-card-content');
  const panel = content
    ? [...content.children].find(el => !el.classList.contains('main-card-header')
      && !el.classList.contains('tc-tabs') && !el.classList.contains('main-card-footer'))
    : null;
  const audioEls = [...document.querySelectorAll('.conference-audio-channels, .session-audio-channels')];
  const audio = audioEls[0] || null;
  return {
    hasBar: Boolean(bar),
    tabs: bar ? [...bar.querySelectorAll('.tc-tab')].map(el => ({
      qa: el.getAttribute('data-qa'),
      text: (el.innerText || '').trim(),
      title: el.getAttribute('title'),
      role: el.getAttribute('role'),
      tabindex: el.getAttribute('tabindex'),
      ariaSelected: el.getAttribute('aria-selected'),
      activeClass: el.classList.contains('mdc-tab--active'),
    })) : [],
    panelFound: Boolean(panel),
    panelCls: panel ? String(panel.className) : null,
    panelText: panel ? (panel.innerText || '').trim().slice(0, 300) : null,
    panelHasLayouts: Boolean(panel && panel.querySelector('.layouts')),
    panelHasAudio: Boolean(panel && panel.querySelector('.conference-audio-channels, .session-audio-channels')),
    audioCount: audioEls.length,
    audioVisible: audio ? visibleState(audio) : null,
    audioText: audio ? (audio.innerText || '').trim().slice(0, 160) : null,
    audioHasTable: Boolean(audio && audio.querySelector('.main-table, .conference-audio-channels-table')),
    audioHasAddButton: Boolean(audio && [...audio.querySelectorAll('button')]
      .some(button => /добавить/i.test((button.innerText || '').trim()))),
    audioIsDirectPanelChild: Boolean(audio && content && audio.parentElement === content),
    // --- поля для D2a/D5/D8/«сиротской панели» ---
    // .main-card-body в активной панели (считаем и саму панель, если она .main-card-body)
    panelBodyCount: panel
      ? (panel.classList.contains('main-card-body') ? 1 : 0) + panel.querySelectorAll('.main-card-body').length
      : null,
    panelBodyClasses: panel
      ? [...(panel.classList.contains('main-card-body') ? [panel] : []), ...panel.querySelectorAll('.main-card-body')]
        .map(el => String(el.className))
      : null,
    panelChildClasses: panel ? [...panel.children].map(el => String(el.className)) : null,
    panelChildTags: panel ? [...panel.children].map(el => el.tagName.toLowerCase()) : null,
    audioIndexInPanel: panel && audio ? [...panel.children].indexOf(audio) : null,
    audioParentIsPanel: panel && audio ? audio.parentElement === panel : null,
    // футер вкладки
    footerCount: content ? [...content.children].filter(el => el.classList.contains('main-card-footer')).length : null,
    footerButtons: content
      ? [...content.querySelectorAll(':scope > .main-card-footer button')]
        .map(button => ({ text: (button.innerText || '').trim(), qa: button.getAttribute('data-qa') }))
      : [],
    footerButtonQa: content
      ? [...content.querySelectorAll(':scope > .main-card-footer button')].map(button => button.getAttribute('data-qa'))
      : [],
    // сколько .main-card-body в документе (поиск «сиротских» панелей)
    documentBodyCount: document.querySelectorAll('.main-card-body').length,
    documentAudioCount: document.querySelectorAll('.conference-audio-channels, .session-audio-channels').length,
  };
};

/** Замер подписи в плитке раскладки (1-based). */
async function measureTile(page, position) {
  const tile = page.locator('.layout-grid-slot-wrapper').nth(position - 1);
  if (!(await tile.count())) return { labelFound: false, error: `tile ${position} not rendered` };
  return tile.evaluate(element => {
    const box = el => { const b = el.getBoundingClientRect(); return { left: b.left, top: b.top, right: b.right, bottom: b.bottom, width: b.width, height: b.height }; };
    const text = element.querySelector('.layout-grid-slot-dn__text');
    const tileBox = box(element);
    const slotFontSizePx = parseFloat(getComputedStyle(element).fontSize);
    if (!text) {
      return { labelFound: false, tileW: tileBox.width, tileH: tileBox.height, slotFontSizePx };
    }
    const labelBox = box(text);
    const labelStyle = getComputedStyle(text);
    return {
      labelFound: true,
      labelText: (text.innerText || '').trim(),
      labelFontSizePx: parseFloat(labelStyle.fontSize),
      slotFontSizePx,
      tileW: tileBox.width, tileH: tileBox.height,
      labelW: labelBox.width, labelH: labelBox.height,
      dx: (labelBox.left + labelBox.width / 2) - (tileBox.left + tileBox.width / 2),
      dy: (labelBox.top + labelBox.height / 2) - (tileBox.top + tileBox.height / 2),
      labelInsideTile: labelBox.top >= tileBox.top - 0.5 && labelBox.bottom <= tileBox.bottom + 0.5,
      labelInsideTileX: labelBox.left >= tileBox.left - 0.5 && labelBox.right <= tileBox.right + 0.5,
      overflowX: text.scrollWidth > text.clientWidth + 1,
    };
  });
}

/** Закрыть панель видеслота (иначе она сужает сетку). */
async function closeSlotPanel(page) {
  const close = page.locator('.grid-slot-side-bar button').filter({ hasText: 'close' }).first();
  if (await close.count()) {
    await close.click({ timeout: 5000 }).catch(() => {});
    await page.waitForTimeout(400);
  }
  if (await page.locator('.grid-slot-side-bar').isVisible().catch(() => false)) {
    await page.keyboard.press('Escape').catch(() => {});
    await page.waitForTimeout(300);
  }
  return !(await page.locator('.grid-slot-side-bar').isVisible().catch(() => false));
}

/** Посадить контакт в ячейку `position` (1-based) через панель видеслота. */
async function placeParticipant(page, position) {
  const cell = page.locator('.layout-grid-slot-wrapper').nth(position - 1);
  if (!(await cell.count())) return { placed: false, error: `tile ${position} not rendered` };
  await cell.scrollIntoViewIfNeeded().catch(() => {});
  await cell.click({ timeout: 10000 });
  await page.waitForTimeout(700);
  const scoped = page.locator('.grid-slot-contact-item').getByText(PARTICIPANT, { exact: false });
  let target = scoped.last();
  if (!(await scoped.count())) target = page.getByText(PARTICIPANT, { exact: false }).last();
  if (!(await target.count())) return { placed: false, error: `contact «${PARTICIPANT}» not listed in the slot panel` };
  await target.click({ timeout: 10000 });
  await page.waitForTimeout(700);
  const labeled = await page.locator('.layout-grid-slot-wrapper .layout-grid-slot-dn__text').count();
  return { placed: labeled > 0, error: labeled > 0 ? null : 'slot panel click did not produce a label' };
}

/** Счётчики в шапке карточки конференции (участники/устройства). */
async function probeHeaderCounters(page) {
  return page.evaluate(() => {
    const boxes = [...document.querySelectorAll('.monitoring-conference__participants-container, .main-card-header [title]')];
    return boxes.map(box => ({
      title: box.getAttribute('title') || '',
      texts: [...box.querySelectorAll('span')].map(span => (span.innerText || '').trim()).filter(Boolean),
    }));
  });
}

/**
 * Имитация «исправленного» состояния средствами DOM (для самопроверки харнесса):
 * вкладка аудио убирается из таб-бара, а её содержимое переносится в панель
 * вкладки «Раскладки». Это НЕ замена настоящей правки — только доказательство,
 * что контрактные проверки достижимы и не «формально красные».
 */
/** Снимок «исправленного» состояния: разметка вкладки аудио и блока аудиоканалов. */
async function captureFixedState(page, state) {
  // 1. Запомнить разметку вкладки аудио (если она есть) и блока аудиоканалов.
  if (!state.audioTabHtml) {
    state.audioTabHtml = await page.evaluate(() => {
      const tab = document.querySelector('.tc-tabs.main-card__tabs [data-qa="conference-audio-tab"]');
      return tab ? tab.outerHTML : null;
    });
  }
  if (!state.audioBlockHtml) {
    const audioTab = page.locator('.tc-tabs.main-card__tabs [data-qa="conference-audio-tab"]');
    if (await audioTab.count()) {
      await audioTab.click({ timeout: 10000 }).catch(error => {
        console.log(`     (клик по вкладке аудио не удался: ${String(error).split('\n')[0].slice(0, 100)})`);
      });
      // блок аудиоканалов монтируется только в активной вкладке — дожидаемся его
      await page.waitForSelector('.conference-audio-channels', { timeout: 8000 })
        .catch(() => console.log('     (блок аудиоканалов не смонтировался при клике по вкладке)'));
      state.audioBlockHtml = await page.evaluate(() => {
        const block = document.querySelector('.conference-audio-channels');
        return block ? block.outerHTML : null;
      });
    }
  }
  return state;
}

/**
 * Применить «исправленное» состояние: вкладка аудио убрана из таб-бара, блок
 * аудиоканалов перенесён в панель вкладки «Раскладки». Идемпотентно: Vue
 * перерисовывает панель при переключении вкладок, поэтому шаг повторяется перед
 * каждым измерением вкладки «Раскладки».
 */
async function applySimulatedFix(page, state) {
  // Если блок аудиоканалов смонтирован вендором (активна вкладка «Аудиоканал»),
  // сначала переключаемся на «Раскладки» — тогда Vue корректно размонтирует его.
  // Удалять вендорный узел руками нельзя: ломается дерево Vue (insertBefore на null).
  const hasVendorBlock = await page.evaluate(() => [...document.querySelectorAll('.conference-audio-channels')]
    .some(el => !el.hasAttribute('data-sim')));
  if (hasVendorBlock) {
    await page.locator('.tc-tabs.main-card__tabs [data-qa="conference-layouts-tab"]').click().catch(() => {});
    await page.waitForTimeout(1000);
  }
  // Убрать кнопку вкладки аудио.
  await page.evaluate(() => {
    const tab = document.querySelector('.tc-tabs.main-card__tabs [data-qa="conference-audio-tab"]');
    if (tab) tab.remove();
  });
  // Убрать только наши собственные клоны (помечены data-sim).
  await page.evaluate(() => {
    document.querySelectorAll('.conference-audio-channels[data-sim]').forEach(el => el.remove());
  });
  // Вставить клон блока аудиоканалов в панель вкладки «Раскладки».
  if (state.audioBlockHtml) {
    await page.evaluate(html => {
      const content = document.querySelector('.main-card-content');
      const panel = content && [...content.children].find(el => !el.classList.contains('main-card-header')
        && !el.classList.contains('tc-tabs') && !el.classList.contains('main-card-footer'));
      if (!panel) return;
      if (panel.querySelector('.conference-audio-channels')) return;
      const host = document.createElement('div');
      host.className = 'main-card-body__container';
      host.innerHTML = html;
      const block = host.querySelector('.conference-audio-channels');
      if (block) block.setAttribute('data-sim', '1');
      panel.appendChild(host);
    }, state.audioBlockHtml);
    await page.waitForTimeout(400);
  }
  // После удаления вкладки активной может не остаться — вернём фокус на «Раскладки».
  const selectedCount = await page.evaluate(() => [...document.querySelectorAll('.tc-tabs.main-card__tabs .tc-tab')]
    .filter(tab => tab.getAttribute('aria-selected') === 'true').length);
  if (!selectedCount) {
    await page.locator('.tc-tabs.main-card__tabs [data-qa="conference-layouts-tab"]').click().catch(() => {});
    await page.waitForTimeout(900);
  }
  state.simulated = await page.evaluate(() => {
    const content = document.querySelector('.main-card-content');
    const panel = content && [...content.children].find(el => !el.classList.contains('main-card-header')
      && !el.classList.contains('tc-tabs') && !el.classList.contains('main-card-footer'));
    return {
      audioTabRemoved: !document.querySelector('.tc-tabs.main-card__tabs [data-qa="conference-audio-tab"]'),
      audioBlocks: document.querySelectorAll('.conference-audio-channels').length,
      audioInLayoutsPanel: Boolean(panel && panel.querySelector('.conference-audio-channels')),
    };
  });
  console.log(`     [имитация] ${JSON.stringify(state.simulated)}`);
  return state;
}

/**
 * Откат: вкладка аудио возвращается, блок аудиоканалов уезжает из панели
 * раскладок. scope: 'conference' (data-qa conference-audio-tab) или
 * 'session' (session-main-tabs-audio, подпись «Аудиоканалы»).
 */
async function revertFixedState(page, state, scope = 'conference') {
  const conf = scope === 'conference';
  const blockSelector = conf ? '.conference-audio-channels' : '.session-audio-channels';
  const tabQa = conf ? 'conference-audio-tab' : 'session-main-tabs-audio';
  const tabTitle = conf ? 'Аудиоканал' : 'Аудиоканалы';
  const layoutsQa = conf ? 'conference-layouts-tab' : 'session-main-tabs-layouts';
  // 1. Вынести блок аудиоканалов из панели раскладок в отдельную панель.
  await page.evaluate(selector => {
    const content = document.querySelector('.main-card-content');
    const panel = content && [...content.children].find(el => !el.classList.contains('main-card-header')
      && !el.classList.contains('tc-tabs') && !el.classList.contains('main-card-footer'));
    const block = document.querySelector(selector);
    if (!content || !panel || !block) return;
    // в исходном (до правки) состоянии блок аудиоканалов — прямой потомок
    // .main-card-content, то есть отдельная панель; возвращаем именно эту структуру
    content.insertBefore(block, panel.nextSibling);
  }, blockSelector);
  // 2. Вернуть кнопку вкладки аудио (сразу после вкладки «Раскладки»).
  await page.evaluate(({ html, tabQa, tabTitle, layoutsQa }) => {
    const bar = document.querySelector('.tc-tabs.main-card__tabs');
    if (!bar) return;
    if (bar.querySelector(`[data-qa="${tabQa}"]`)) return;
    const layoutsTab = bar.querySelector(`[data-qa="${layoutsQa}"]`);
    const host = document.createElement('div');
    host.innerHTML = html;
    const tab = host.firstElementChild;
    if (!tab) return;
    tab.setAttribute('data-qa', tabQa);
    tab.setAttribute('title', tabTitle);
    tab.setAttribute('aria-selected', 'false');
    tab.classList.remove('mdc-tab--active');
    const label = tab.querySelector('.mdc-tab__text-label');
    if (label) label.textContent = tabTitle;
    // кнопки вкладок лежат во внутреннем контейнере таб-бара — вставляем рядом с ними
    const host2 = layoutsTab ? layoutsTab.parentElement : bar;
    const sibling = layoutsTab ? layoutsTab.nextElementSibling : null;
    if (sibling) host2.insertBefore(tab, sibling);
    else host2.appendChild(tab);
  }, {
    html: state.audioTabHtml || `<button role="tab" tabindex="0" class="tc-tab mdc-tab" title="${tabTitle}" aria-selected="false" data-qa="${tabQa}"><span class="mdc-tab__text-label">${tabTitle}</span></button>`,
    tabQa, tabTitle, layoutsQa,
  });
  await page.waitForTimeout(400);
  return state;
}

/** Ключевые контрактные проверки (используются и в основном, и в режиме отката). */
async function runContractChecks(page, phase) {
  const bar = await page.evaluate(TAB_PROBE);
  const tabQa = bar.tabs.map(tab => tab.qa);
  const audioTabPresent = bar.tabs.some(tab => tab.qa === AUDIO_TAB_QA)
    || bar.tabs.some(tab => /аудиоканал/i.test(`${tab.text || ''} ${tab.title || ''}`));
  const expectedQa = EXPECTED_TABS.map(tab => tab.qa);

  check(`[${phase}] conference tab bar: the audio channel is not a separate tab`,
    !audioTabPresent,
    `tabs=${tabQa.join(',')}`);
  check(`[${phase}] conference tab bar: ${expectedQa.length} tabs in the expected order`,
    JSON.stringify(tabQa) === JSON.stringify(expectedQa),
    `expected=${expectedQa.join(',')} actual=${tabQa.join(',')}`);
  const roleOk = bar.tabs.every(tab => tab.role === 'tab') && bar.tabs.every(tab => tab.tabindex !== null);
  const selected = bar.tabs.filter(tab => tab.ariaSelected === 'true');
  const activeClass = bar.tabs.filter(tab => tab.activeClass);
  check(`[${phase}] conference tab bar: role=tab / tabindex / aria-selected are intact`,
    roleOk && selected.length === 1 && activeClass.length === 1
    && selected[0].qa === activeClass[0].qa,
    `role/tabindex ok=${roleOk} aria-selected=${selected.map(t => t.qa).join(',') || 'none'} active=${activeClass.map(t => t.qa).join(',') || 'none'}`);
  return bar;
}

/**
 * Проверки контракта «блок аудиоканалов живёт внутри панели раскладок».
 * phase — метка фазы ('current' | 'fixed-sim' | 'revert-post'), попадает в имя
 * проверки, чтобы в result.json было видно, к какому состоянию относится результат.
 */
async function checkLayoutsPanelContract(page, phase) {
  const prefix = phase ? `[${phase}] ` : '';
  const probe = await page.evaluate(TAB_PROBE);
  check(`${prefix}tab Раскладки: the audio channel block is rendered inside the layouts panel`,
    probe.panelHasAudio,
    `panelHasAudio=${probe.panelHasAudio} audioCount=${probe.audioCount} panelCls=${probe.panelCls}`);
  check(`${prefix}tab Раскладки: the layouts content itself is still there`,
    probe.panelHasLayouts, `panelHasLayouts=${probe.panelHasLayouts}`);
  check(`${prefix}tab Раскладки: the audio channel block is visible (not just hidden)`,
    Boolean(probe.audioVisible && probe.audioVisible.visible),
    JSON.stringify(probe.audioVisible));
  check(`${prefix}tab Раскладки: the audio channel block keeps its functionality`,
    probe.audioHasTable || probe.audioHasAddButton,
    `table=${probe.audioHasTable} addButton=${probe.audioHasAddButton} text=${JSON.stringify(probe.audioText)}`);
  check(`${prefix}tab Раскладки: exactly one audio block, and it is not a separate panel`,
    probe.audioCount === 1 && !probe.audioIsDirectPanelChild,
    `audioCount=${probe.audioCount} isDirectPanelChild=${probe.audioIsDirectPanelChild}`);
  return probe;
}

/**
 * Геометрия панели «Раскладки»: контейнер, блок аудиоканалов, кнопка «Добавить».
 */
const GEOM_PROBE = () => {
  const content = document.querySelector('.main-card-content');
  const panel = content && [...content.children].find(el => !el.classList.contains('main-card-header')
    && !el.classList.contains('tc-tabs') && !el.classList.contains('main-card-footer'));
  const audio = document.querySelector('.conference-audio-channels, .session-audio-channels');
  const layouts = panel ? panel.querySelector('.layouts') : null;
  const addButton = audio ? [...audio.querySelectorAll('button')].find(button => /добавить/i.test(button.innerText || '')) : null;
  const image = audio ? audio.querySelector('.main-blank__image') : null;
  const rect = el => {
    if (!el) return null;
    const box = el.getBoundingClientRect();
    return { top: box.top, bottom: box.bottom, width: box.width, height: box.height };
  };
  const containerRect = rect(panel);
  const audioRect = rect(audio);
  const layoutsRect = rect(layouts);
  const addRect = rect(addButton);
  return {
    hasContainer: Boolean(panel),
    hasAudio: Boolean(audio),
    container: containerRect,
    containerScrollH: panel ? panel.scrollHeight : null,
    containerClientH: panel ? panel.clientHeight : null,
    containerOverflowY: panel ? getComputedStyle(panel).overflowY : null,
    audio: audioRect,
    audioFlex: audio ? getComputedStyle(audio).flex : null,
    audioMaxHeight: audio ? getComputedStyle(audio).maxHeight : null,
    audioScrollH: audio ? audio.scrollHeight : null,
    audioClientH: audio ? audio.clientHeight : null,
    layoutsHeight: layoutsRect ? layoutsRect.height : null,
    layoutsScrollH: layouts ? layouts.scrollHeight : null,
    layoutsClientH: layouts ? layouts.clientHeight : null,
    layoutsFlex: layouts ? getComputedStyle(layouts).flex : null,
    addButton: addRect,
    illustration: rect(image),
    // насколько нижняя граница уходит за нижнюю границу контейнера (>0 = обрезка)
    addButtonOverflowPx: (addRect && containerRect) ? addRect.bottom - containerRect.bottom : null,
    audioOverflowPx: (audioRect && containerRect) ? audioRect.bottom - containerRect.bottom : null,
    // обрезка ВНУТРИ блока аудио (у блока своя прокрутка, max-height)
    addButtonInsideAudioPx: (addRect && audioRect) ? addRect.bottom - audioRect.bottom : null,
    // блоки (раскладки + аудио) заполняют панель: 0 = без пустого места и без наложения
    blocksSumPx: (audioRect && layoutsRect && containerRect)
      ? (audioRect.height + layoutsRect.height - containerRect.height) : null,
  };
};

/**
 * Варианты «старых» правил для самопроверки обрезки (N7):
 *   A — состояние ДО высотной политики T15: оба блока по своей естественной
 *       высоте (flex:0 0 auto), картинка пустого состояния 240×240, у
 *       контейнера overflow-y:hidden ⇒ кнопка «Добавить» уходит за нижний край
 *       КОНТЕЙНЕРА (та самая обрезка, что видел заказчик);
 *   B — текущая высотная политика, но со старой картинкой 240×240 ⇒ блок аудио
 *       ограничен 320px, а его контент выше ⇒ кнопка срезана ВНУТРИ блока.
 */
const CLIP_VARIANT_CSS = {
  A: `
.main-card-body__container > .main-card-body.conference-audio-channels,
.main-card-body__container > .main-card-body.session-audio-channels {
  flex: 0 0 auto !important;
  min-height: auto !important;
  max-height: none !important;
  height: max-content !important;
  border-top: 0 !important;
}
/* старая иллюстрация пустого состояния — 240×240 и видимая */
.main-card-body__container > .main-card-body.conference-audio-channels .main-blank__image,
.main-card-body__container > .main-card-body.session-audio-channels .main-blank__image {
  display: block !important;
  width: 240px !important;
  height: 240px !important;
}
.main-card-body__container:has(> .main-card-body.conference-audio-channels),
.main-card-body__container:has(> .main-card-body.session-audio-channels) {
  overflow-y: hidden !important;
}
.main-card-body__container:has(> .main-card-body.conference-audio-channels) > .main-card-body,
.main-card-body__container:has(> .main-card-body.session-audio-channels) > .main-card-body {
  flex: 0 0 auto !important;
  min-height: auto !important;
}
`,
  B: `
.main-card-body__container > .main-card-body.conference-audio-channels,
.main-card-body__container > .main-card-body.session-audio-channels {
  min-height: auto !important;
}
/* старая иллюстрация пустого состояния — 240×240 и видимая; блок аудио
   остаётся ограниченным 320px, поэтому кнопка срезается ВНУТРИ блока */
.main-card-body__container > .main-card-body.conference-audio-channels .main-blank__image,
.main-card-body__container > .main-card-body.session-audio-channels .main-blank__image {
  display: block !important;
  width: 240px !important;
  height: 240px !important;
}
`,
};

/** Инъекция «старых» правил (режим VKS_CLIP_CHECK) — идемпотентна на документ. */
async function injectClipCss(page, variant) {
  if (!CLIP_CHECK || !variant) return false;
  await page.evaluate(({ css, id }) => {
    // снимаем ВСЕ ранее вложенные варианты, иначе правила разных вариантов
    // складываются и измерение не соответствует ни одному из них
    for (const previous of document.querySelectorAll('style[id^="vks-clip-css-"]')) previous.remove();
    const style = document.createElement('style');
    style.id = id;
    style.textContent = css;
    document.head.appendChild(style);
  }, { css: CLIP_VARIANT_CSS[variant] || '', id: `vks-clip-css-${variant}` });
  await page.waitForTimeout(600);
  return true;
}

/**
 * N7 — блок аудиоканалов не должен быть срезан границей контейнера панели.
 * Проверяем три независимые вещи (каждая ловит свой способ обрезки):
 *  1) кнопка «Добавить» и сам блок аудио не выходят за нижнюю границу
 *     `.main-card-body__container` (допуск +1px);
 *  2) кнопка «Добавить» целиком видна ВНУТРИ блока аудио (у блока своя
 *     прокрутка и max-height — он может срезать контент изнутри);
 *  3) у контейнера нет вертикального переполнения.
 * Плюс высотная политика T15: блок аудио не выше своей max-height, оба блока
 * ровно заполняют панель, иллюстрация пустого состояния 120×120.
 */
async function checkAudioBlockGeometry(page, scope, phase, options = {}) {
  const { clipVariant = null, allowLayoutsScroll = false, viewport = null, label = '' } = options;
  const prefix = `${phase ? `[${phase}] ` : ''}${label ? `${label} ` : ''}`;
  const vp = viewport ? `(${viewport}) ` : '';
  if (clipVariant) await injectClipCss(page, clipVariant);
  let g = await page.evaluate(GEOM_PROBE);
  if (clipVariant) {
    // после инъекции раскладка ещё пересчитывается (flex + картинка) —
    // снимаем 3 замера подряд и берём последний (устойчивый)
    for (let attempt = 0; attempt < 3; attempt += 1) {
      await page.waitForTimeout(400);
      g = await page.evaluate(GEOM_PROBE);
    }
  }
  const overflow = value => (value === null ? 'n/a' : `${value.toFixed(2)}px`);
  check(`${prefix}${vp}${scope}: the «Добавить» button is not cut off by the bottom of the panel container`,
    g.addButtonOverflowPx !== null && g.addButtonOverflowPx <= 1,
    `addButton.bottom - container.bottom = ${overflow(g.addButtonOverflowPx)} (допуск +1px)`);
  check(`${prefix}${vp}${scope}: the audio channel block is not cut off by the bottom of the panel container`,
    g.audioOverflowPx !== null && g.audioOverflowPx <= 1,
    `audio.bottom - container.bottom = ${overflow(g.audioOverflowPx)} (допуск +1px)`);
  check(`${prefix}${vp}${scope}: the «Добавить» button is fully visible inside the audio block (not cut by the block's own overflow)`,
    g.addButtonInsideAudioPx !== null && g.addButtonInsideAudioPx <= 1,
    `addButton.bottom - audioBlock.bottom = ${overflow(g.addButtonInsideAudioPx)} (допуск +1px)`);
  check(`${prefix}${vp}${scope}: the panel container has no vertical overflow`,
    g.containerScrollH !== null && g.containerScrollH <= g.containerClientH + 1,
    `scrollH=${g.containerScrollH} clientH=${g.containerClientH} overflowY=${g.containerOverflowY}`);
  // По текущей высотной политике иллюстрация пустого состояния блока аудио
  // СКРЫТА (display:none) — блок аудио в пустом состоянии занимает ~138px.
  // Проверка сформулирована как «не больше 120×120»: и скрытая (0×0), и
  // видимая 120×120 иллюстрация допустимы, а старая 240×240 — нет.
  check(`${prefix}${vp}${scope}: the empty-state illustration of the audio block takes no more than 120×120`,
    Boolean(g.illustration) && g.illustration.width <= 121 && g.illustration.height <= 121,
    `illustration=${g.illustration ? `${g.illustration.width.toFixed(1)}×${g.illustration.height.toFixed(1)}` : 'n/a'}`
    + ' (в текущей политике иллюстрация скрыта: display:none)');
  // ОТДЕЛЬНЫЙ контракт финальной политики: в объединённой панели пустого
  // состояния аудиоканалов иллюстрации нет вообще (display:none, 0×0). Без
  // этого на 1280×800 / 1024×768 список раскладок не помещался в остаток
  // высоты, и его кнопка «Добавить» уходила за нижний край блока. Откат
  // этой политики (видимая иллюстрация) обязан давать FAIL.
  check(`${prefix}${vp}${scope}: the empty-state illustration of the audio block is hidden (display:none)`,
    !g.illustration || (g.illustration.width <= 1 && g.illustration.height <= 1),
    `illustration=${g.illustration ? `${g.illustration.width.toFixed(1)}×${g.illustration.height.toFixed(1)}` : 'нет элемента'}`
    + ' — ожидается скрытая иллюстрация (0×0 / элемента нет)');
  // высотная политика T15: блок аудио ограничен max-height (320px)
  const maxHeightPx = g.audioMaxHeight && /px$/.test(g.audioMaxHeight) ? parseFloat(g.audioMaxHeight) : null;
  check(`${prefix}${vp}${scope}: the audio channel block does not exceed its max-height`,
    maxHeightPx !== null && g.audio && g.audio.height <= maxHeightPx + 1,
    `audio.height=${g.audio ? g.audio.height.toFixed(1) : 'n/a'}px max-height=${g.audioMaxHeight}`);
  // оба блока ровно заполняют панель: нет ни пустого места, ни наложения
  check(`${prefix}${vp}${scope}: the two blocks exactly fill the panel (no gap, no overlap)`,
    g.blocksSumPx !== null && Math.abs(g.blocksSumPx) <= 2,
    `layouts.height + audio.height - container.height = ${overflow(g.blocksSumPx)} (допуск ±2px)`);
  // список раскладок: в 1600×1000 своей прокрутки нет; на низких вьюпортах
  // список сжимается и скроллится сам — это вендорская норма, важно лишь,
  // что блок аудиоканалов при этом не срезан.
  const layoutsScrollable = g.layoutsScrollH !== null && g.layoutsScrollH > g.layoutsClientH + 1;
  check(`${prefix}${vp}${scope}: the layouts list has no internal scroll`
    + (allowLayoutsScroll ? ' (allowed on this viewport)' : ''),
    allowLayoutsScroll ? true : !layoutsScrollable,
    `layouts scrollH=${g.layoutsScrollH} clientH=${g.layoutsClientH} flex=${g.layoutsFlex}`
    + (allowLayoutsScroll ? ' — на низком вьюпорте прокрутка списка раскладок допустима' : ''),
    allowLayoutsScroll ? 'info' : 'fatal');
  if (clipVariant) {
    // обрезка засчитывается, если срезано ЛИБО сам блок аудио (нижняя граница
    // блока за нижней границей контейнера), ЛИБО кнопка «Добавить» (за краем
    // контейнера или внутри блока)
    const reproduced = (g.audioOverflowPx !== null && g.audioOverflowPx > 1)
      || (g.addButtonOverflowPx !== null && g.addButtonOverflowPx > 1)
      || (g.addButtonInsideAudioPx !== null && g.addButtonInsideAudioPx > 1);
    check(`${prefix}${scope} (clip check ${clipVariant}): the legacy CSS really reproduces the clipped block`,
      reproduced || (scope === 'session' && clipVariant === 'A'),
      `audio.bottom - container.bottom = ${overflow(g.audioOverflowPx)}, `
      + `addButton.bottom - container.bottom = ${overflow(g.addButtonOverflowPx)}, `
      + `addButton.bottom - audioBlock.bottom = ${overflow(g.addButtonInsideAudioPx)} (ожидаем обрезку > 1px)`
      + (scope === 'session' && clipVariant === 'A'
        ? ' — на сессии список раскладок высокий и сжимается сам, поэтому вариант A тут не воспроизводится (см. вариант B)'
        : ''),
      scope === 'session' && clipVariant === 'A' ? 'info' : 'fatal');
  }
  return g;
}

/**
 * N7 на низких вьюпортах: при сжатии окна список раскладок сжимается и
 * скроллится сам (это норма), но блок аудиоканалов обязан остаться целым.
 */
async function checkLowViewportGeometry(page, scope, phase) {
  // Список раскладок не должен иметь своей прокрутки там, где сумма двух
  // пустых состояний влезает в карточку (1280×800, 1024×768, 820×1180).
  // На 1280×600 карточка ниже вендорного контента (403px) — там прокрутка
  // списка такая же, как в вендорной базе, и она допустима.
  const viewports = [
    { width: 1280, height: 800, layoutsScroll: false },
    { width: 1024, height: 768, layoutsScroll: false },
    { width: 820, height: 1180, layoutsScroll: false },
    { width: 1280, height: 600, layoutsScroll: true },
  ];
  const results = [];
  for (const vp of viewports) {
    await page.setViewportSize({ width: vp.width, height: vp.height });
    await page.waitForTimeout(900);
    const g = await checkAudioBlockGeometry(page, scope, phase, {
      allowLayoutsScroll: vp.layoutsScroll, viewport: `${vp.width}x${vp.height}`,
    });
    results.push({ viewport: `${vp.width}x${vp.height}`, geometry: g });
  }
  await page.setViewportSize({ width: 1600, height: 1000 });
  await page.waitForTimeout(600);
  return results;
}

/** Контракт таб-бара сессии (используется и в основном прогоне, и в откате). */
async function checkSessionTabBarContract(page, phase) {
  const prefix = phase ? `[${phase}] ` : '';
  const bar = await page.evaluate(TAB_PROBE);
  const tabQa = bar.tabs.map(tab => tab.qa);
  const expectedQa = SESSION_TABS.map(tab => tab.qa);
  check(`${prefix}session page: 6 tabs in the expected order`,
    JSON.stringify(tabQa) === JSON.stringify(expectedQa),
    `expected=${expectedQa.join(',')} actual=${tabQa.join(',')}`);
  check(`${prefix}session page: no session-main-tabs-audio and no tab named «Аудиоканалы»`,
    !tabQa.some(qa => /audio/i.test(qa))
    && !bar.tabs.some(tab => /аудиоканал/i.test(`${tab.text || ''} ${tab.title || ''}`)),
    `tabs=${tabQa.join(',')}`);
  return bar;
}

/**
 * D1/D6 — ветка СЕССИИ: настоящий маршрут /pnm/monitoring/sessions/{id}
 * (тот же UUID), собственный таб-бар, свой блок аудиоканалов.
 */
async function checkSessionBranch(page) {
  let sessionGeometry = null;
  let sessionKeyboard = null;
  let clipGeometryBSession = null;
  const sessionUrl = `${BASE}/pnm/monitoring/sessions/${SESSION_ID}`;
  await page.goto(sessionUrl, { waitUntil: 'networkidle' });
  await page.waitForTimeout(3000);
  check('session page: tab bar rendered', await page.locator(TAB_BAR).count() > 0, sessionUrl);
  await checkSessionTabBarContract(page, '');

  const walk = [];
  for (const spec of SESSION_TABS) {
    const tab = page.locator(`${TAB_BAR} [data-qa="${spec.qa}"]`);
    if (!(await tab.count())) {
      check(`session tab ${spec.label}: present in the tab bar`, false, `${spec.qa} not found`);
      continue;
    }
    await tab.click({ timeout: 10000 });
    await page.waitForTimeout(1500);
    const probe = await page.evaluate(TAB_PROBE);
    const selected = probe.tabs.filter(item => item.ariaSelected === 'true').map(item => item.qa);
    const markerPresent = await page.locator(spec.marker).count() > 0;
    check(`session tab ${spec.label}: opens and shows its own content`,
      markerPresent && selected.length === 1 && selected[0] === spec.qa,
      `marker=${spec.marker} present=${markerPresent} aria-selected=${selected.join(',') || 'none'}`);
    // D5: футер
    const footerExpected = !FOOTERLESS[spec.qa];
    const labels = probe.footerButtons.map(button => button.text);
    check(`session tab ${spec.label}: the footer is ${footerExpected ? '«Отмена»+«Сохранить»' : 'absent'}`,
      footerExpected
        ? probe.footerCount === 1 && labels.length === 2 && labels[0] === 'Отмена' && labels[1] === 'Сохранить'
        : probe.footerCount === 0 && labels.length === 0,
      `footerCount=${probe.footerCount} buttons=${JSON.stringify(labels)}`);
    // «сиротская» панель
    const expectedBodies = spec.qa === 'session-main-tabs-layouts' ? 2 : 1;
    check(`session tab ${spec.label}: no orphan panel (${expectedBodies} .main-card-body in the panel)`,
      probe.panelBodyCount === expectedBodies && probe.documentBodyCount === expectedBodies
      && Boolean(probe.panelText && probe.panelText.length > 3),
      `panelBodyCount=${probe.panelBodyCount} documentBodyCount=${probe.documentBodyCount}`);
    if (spec.qa === 'session-main-tabs-layouts') {
      check('session tab Раскладки: the audio channel block is rendered inside the layouts panel',
        probe.panelHasAudio, `panelHasAudio=${probe.panelHasAudio} audioCount=${probe.audioCount}`);
      check('session tab Раскладки: the audio channel block is visible (not just hidden)',
        Boolean(probe.audioVisible && probe.audioVisible.visible), JSON.stringify(probe.audioVisible));
      check('session tab Раскладки: the audio channel block keeps its functionality',
        probe.audioHasTable || probe.audioHasAddButton,
        `table=${probe.audioHasTable} addButton=${probe.audioHasAddButton} text=${JSON.stringify(probe.audioText)}`);
      check('session tab Раскладки: exactly one audio block, and it is not a separate panel',
        probe.audioCount === 1 && !probe.audioIsDirectPanelChild,
        `audioCount=${probe.audioCount} isDirectPanelChild=${probe.audioIsDirectPanelChild}`);
      const secondChildIsAudio = probe.panelChildClasses
        && probe.panelChildClasses.length === 2
        && /session-audio-channels/.test(probe.panelChildClasses[1]);
      check('session tab Раскладки: exactly two .main-card-body and the audio block is the second child',
        probe.panelBodyCount === 2 && probe.panelChildClasses.length === 2 && secondChildIsAudio
        && probe.audioParentIsPanel,
        `children=${JSON.stringify(probe.panelChildClasses)} audioIndexInPanel=${probe.audioIndexInPanel}`);
      sessionGeometry = await checkAudioBlockGeometry(page, 'session',
        CHECKS_SELFTEST || REVERT_CHECK ? 'fixed-sim' : '',
        { clipVariant: CLIP_CHECK ? 'A' : null });
      if (CLIP_CHECK) {
        // на сессии список раскладок высокий (678px) и сжимается сам, поэтому
        // вариант A (обрезка границей контейнера) тут не воспроизводится;
        // воспроизводится вариант B — кнопка срезана ВНУТРИ блока аудио
        clipGeometryBSession = await checkAudioBlockGeometry(page, 'session',
          CHECKS_SELFTEST || REVERT_CHECK ? 'fixed-sim' : '',
          { clipVariant: 'B', label: '[clip-B]' });
      }
    } else {
      check(`session tab ${spec.label}: the audio channel block is not mounted here`,
        !probe.panelHasAudio, `panelHasAudio=${probe.panelHasAudio}`);
    }
    walk.push({ qa: spec.qa, label: spec.label, markerPresent, selected, panelBodyCount: probe.panelBodyCount });
  }
  // D2b/D7 для сессии: «Добавить» ведёт в редактор, «Отмена» возвращает обратно.
  await page.locator(`${TAB_BAR} [data-qa="session-main-tabs-layouts"]`).click();
  await page.waitForTimeout(1200);
  await checkAudioEditorRoundTrip(page, {
    scope: 'session', url: sessionUrl, audioSelector: SESSION_AUDIO_BLOCK, layoutsTabQa: 'session-main-tabs-layouts',
  });

  // клавиатура по таб-бару СЕССИИ — симметрично конференции (после круга
  // «Добавить» → редактор → «Отмена», который возвращает на «Раскладки»)
  sessionKeyboard = await checkKeyboardNavigation(page, {
    scope: 'session',
    layoutsTabQa: 'session-main-tabs-layouts',
    expected: ['session-main-tabs-skins', 'session-record-and-stream-tab', 'session-main-tabs-additional'],
    tabs: SESSION_TABS,
  });
  walk.geometry = sessionGeometry;
  walk.keyboard = sessionKeyboard;
  return walk;
}

/**
 * D2b/D7 — переход в редактор аудиоканала по кнопке «Добавить» и возврат по
 * «Отмена» (НЕ «Создать»: это запись в БД). После возврата обязаны выполняться:
 * URL == страница конференции/сессии, ровно одна вкладка с aria-selected=true
 * (это «Раскладки»), панель непустая, блок аудиоканалов ВНУТРИ панели.
 */
async function checkAudioEditorRoundTrip(page, { scope, url, audioSelector, layoutsTabQa }) {
  const addButton = page.locator(`${audioSelector} button`).filter({ hasText: 'Добавить' });
  if (!(await addButton.count())) {
    check(`${scope}: the audio channel block has an «Добавить» button`, false, 'button not found');
    return null;
  }
  check(`${scope}: the audio channel block has an «Добавить» button`, true);
  await addButton.first().click();
  await page.waitForTimeout(2500);
  const editorUrl = page.url();
  check(`${scope}: the «Добавить» button really leads to the audio channel editor`,
    editorUrl.replace(/\/$/, '').endsWith('/audiochannels'),
    `url=${editorUrl}`);
  const createButton = page.locator('button').filter({ hasText: 'Создать' });
  check(`${scope}: the editor offers «Отмена» and «Создать» (we press only «Отмена»)`,
    await createButton.count() > 0
    && await page.locator('.main-card-footer button').filter({ hasText: 'Отмена' }).count() > 0,
    `create=${await createButton.count()}`);
  const cancel = page.locator('.main-card-footer button').filter({ hasText: 'Отмена' }).first();
  if (await cancel.count()) await cancel.click();
  await page.waitForTimeout(2500);
  const backUrl = page.url().replace(/\/$/, '');
  const probe = await page.evaluate(TAB_PROBE);
  const selected = probe.tabs.filter(item => item.ariaSelected === 'true').map(item => item.qa);
  check(`${scope}: after «Отмена» the URL is back to the ${scope === 'session' ? 'session' : 'conference'} page`,
    backUrl === url.replace(/\/$/, ''), `url=${backUrl} expected=${url}`);
  check(`${scope}: after «Отмена» exactly one tab is selected and it is «Раскладки»`,
    selected.length === 1 && selected[0] === layoutsTabQa,
    `aria-selected=${selected.join(',') || 'none'}`);
  check(`${scope}: after «Отмена» the panel is rendered and non-empty`,
    Boolean(probe.panelFound && probe.panelText && probe.panelText.length > 3),
    `panelFound=${probe.panelFound} panelText=${JSON.stringify((probe.panelText || '').slice(0, 60))}`);
  check(`${scope}: after «Отмена» the audio channel block is inside the panel of that tab`,
    probe.panelHasAudio, `panelHasAudio=${probe.panelHasAudio} audioCount=${probe.audioCount}`);
  return { editorUrl, backUrl, selected, panelHasAudio: probe.panelHasAudio };
}

/**
 * D3 — клавиатурная навигация по ТАБ-БАРУ настоящей правки: 3×ArrowRight от
 * «Раскладок» ведут фокус layouts→skins→record-and-stream→additional.
 * (В режиме самопроверки чеков пропускается: DOM-хирургия ломает фокус MDC —
 * именно поэтому этот режим и не может считаться моделью реализации.)
 */
/**
 * Клавиатурная навигация по таб-бару: 3×ArrowRight от «Раскладок» →
 * скины → «Запись и трансляция» → «Дополнительно»; Enter активирует
 * сфокусированную вкладку, панель следует за ней, `aria-selected` совпадает
 * с `mdc-tab--active`. Работает и для таб-бара конференции, и для сессии.
 */
async function checkKeyboardNavigation(page, options = {}) {
  const {
    scope = 'conference',
    layoutsTabQa = 'conference-layouts-tab',
    expected = ['conference-skins-tab', 'conference-record-and-stream-tab', 'conference-additional-tab'],
    tabs = EXPECTED_TABS,
  } = options;
  const prefix = scope === 'conference' ? 'keyboard' : `keyboard (${scope})`;
  if (CHECKS_SELFTEST) {
    console.log(`     (клавиатурная проверка (${scope}) пропущена в режиме самопроверки чеков: DOM-хирургия ломает фокус MDC)`);
    return null;
  }
  await page.locator(`${TAB_BAR} [data-qa="${layoutsTabQa}"]`).click();
  await page.waitForTimeout(1200);
  const focusQa = () => page.evaluate(() => {
    const el = document.activeElement;
    return el ? { qa: el.getAttribute('data-qa'), tag: el.tagName.toLowerCase() } : null;
  });
  const seen = [];
  for (let index = 0; index < expected.length; index += 1) {
    await page.keyboard.press('ArrowRight');
    await page.waitForTimeout(600);
    seen.push((await focusQa() || {}).qa || null);
  }
  check(`${prefix}: 3×ArrowRight from «Раскладки» moves the focus layouts→skins→record-and-stream→additional`,
    JSON.stringify(seen) === JSON.stringify(expected),
    `seen=${JSON.stringify(seen)} expected=${JSON.stringify(expected)}`);
  // фокусированная вкладка активируется по Enter, и панель следует за ней
  const focusedBeforeEnter = (await focusQa() || {}).qa || null;
  const focusedSpec = tabs.find(item => item.qa === focusedBeforeEnter);
  await page.keyboard.press('Enter');
  await page.waitForTimeout(1200);
  const probe = await page.evaluate(TAB_PROBE);
  const selected = probe.tabs.filter(item => item.ariaSelected === 'true').map(item => item.qa);
  const markerOk = focusedSpec && focusedSpec.marker
    ? await page.locator(focusedSpec.marker).count() > 0
    : Boolean(probe.panelText && probe.panelText.length > 3);
  check(`${prefix}: the focused tab is activated by Enter and its panel is rendered`,
    Boolean(focusedBeforeEnter) && selected.length === 1 && selected[0] === focusedBeforeEnter && markerOk,
    `focused=${focusedBeforeEnter} aria-selected=${selected.join(',') || 'none'} marker=${focusedSpec ? focusedSpec.marker : 'n/a'} markerOk=${markerOk}`);
  // фокус не застревает на последней вкладке — ещё одно нажатие уводит дальше
  await page.keyboard.press('ArrowRight');
  await page.waitForTimeout(600);
  const after = (await focusQa() || {}).qa || null;
  check(`${prefix}: the focus is not stuck on the last tab (it keeps moving)`,
    Boolean(after) && after !== focusedBeforeEnter,
    `after one more ArrowRight: ${after}`);
  return { seen, after, selected, focusedBeforeEnter };
}

(async () => {
  let browser;
  let context;
  const state = { audioTabHtml: null, audioBlockHtml: null };
  const tabWalk = [];
  const sizes = [];
  let layoutsPanel = null;
  let revertProbe = null;
  let sessionWalk = null;
  let sessionKeyboard = null;
  let conferenceGeometry = null;
  let sessionGeometry = null;
  let clipGeometryB = null;
  let clipGeometryBSession = null;
  let lowViewportGeometry = null;
  let keyboard = null;
  let editorRoundTrip = null;

  try {
    browser = await chromium.launch({ headless: true, executablePath: CHROMIUM, args: ['--no-sandbox'] });
    context = await browser.newContext({ viewport: { width: 1600, height: 1000 }, ignoreHTTPSErrors: true });
    const page = await context.newPage();
    page.on('console', message => {
      if (message.type() === 'error' && consoleErrors.length < 60) consoleErrors.push(message.text().slice(0, 300));
    });
    page.on('pageerror', error => {
      if (pageErrors.length < 30) pageErrors.push(String((error && error.stack) || error).slice(0, 600));
    });
    page.on('requestfailed', request => {
      if (failedRequests.length < 30) {
        failedRequests.push({ url: request.url(), error: (request.failure() || {}).errorText || 'unknown' });
      }
    });
    page.on('request', request => {
      if (request.method() !== 'GET') {
        const url = request.url();
        const body = url.includes('/api/v3/login') ? '<redacted>' : (request.postData() || '').slice(0, 300);
        nonGetRequests.push({ method: request.method(), url, body });
      }
    });

    // ------------------------------------------------------------------ логин
    await page.goto(`${BASE}/pnm/authorization`, { waitUntil: 'networkidle' });
    await page.fill('input[autocomplete="username"], input[type="text"]', LOGIN);
    await page.fill('input[type="password"]', PASSWORD);
    await page.click('button[type="submit"], button:has-text("Авторизоваться")');
    await page.waitForTimeout(2500);
    const token = await page.evaluate(() => localStorage.getItem('TrueConfMCU_AccessToken'));
    check('login: token in localStorage', Boolean(token));
    if (!token) throw new Error('login failed: no access token in localStorage');

    // ------------------------------------------- страница конференции: таб-бар
    const conferenceUrl = `${BASE}/pnm/monitoring/conferences/${CONFERENCE_ID}`;
    await page.goto(conferenceUrl, { waitUntil: 'networkidle' });
    await page.waitForTimeout(3000);
    check('conference page: tab bar rendered', await page.locator(TAB_BAR).count() > 0, conferenceUrl);
    const countersBefore = await probeHeaderCounters(page);

    if (CHECKS_SELFTEST || REVERT_CHECK) {
      console.log('\n[самопроверка] имитируем исправленное состояние средствами DOM');
      await captureFixedState(page, state);
      await applySimulatedFix(page, state);
    }

    // --- 1..4: контракт таб-бара
    const bar = await runContractChecks(page, CHECKS_SELFTEST || REVERT_CHECK ? 'fixed-sim' : 'current');

    // --- обход вкладок: каждая открывается и показывает своё содержимое
    for (const spec of EXPECTED_TABS) {
      const tab = page.locator(`${TAB_BAR} [data-qa="${spec.qa}"]`);
      if (!(await tab.count())) {
        check(`tab ${spec.label}: present in the tab bar`, false, `${spec.qa} not found`);
        tabWalk.push({ qa: spec.qa, label: spec.label, present: false });
        continue;
      }
      await tab.click({ timeout: 10000 });
      await page.waitForTimeout(1500);
      if ((CHECKS_SELFTEST || REVERT_CHECK) && spec.qa === 'conference-layouts-tab') {
        // Vue перерисовал панель при переключении вкладки — восстанавливаем
        // имитацию «исправленного» состояния перед измерением.
        await applySimulatedFix(page, state);
      }
      const probe = await page.evaluate(TAB_PROBE);
      const markerOk = spec.marker ? probe.panelText !== null && await page.locator(`${spec.marker}`).count() > 0
        : Boolean(probe.panelText && probe.panelText.includes(spec.text));
      const selectedOk = probe.tabs.some(item => item.qa === spec.qa && item.ariaSelected === 'true');
      const contentOk = Boolean(probe.panelText && probe.panelText.length > 3);
      const audioHere = probe.panelHasAudio;
      check(`tab ${spec.label}: opens and shows its own content`,
        markerOk && selectedOk && contentOk,
        `marker=${spec.marker || spec.text} ok=${markerOk} aria-selected=${selectedOk} panelText=${JSON.stringify((probe.panelText || '').slice(0, 60))}`);
      // --- D2a: инвариант «активная вкладка == видимая панель».
      // У вендора у панели НЕТ data-qa (в DOM её нет), поэтому идентичность панели
      // определяем по корневому классу компонента (marker) выбранной вкладки.
      const selectedQa = probe.tabs.filter(item => item.ariaSelected === 'true').map(item => item.qa);
      const selectedIsThisTab = selectedQa.length === 1 && selectedQa[0] === spec.qa;
      const markerPresent = spec.marker ? await page.locator(spec.marker).count() > 0
        : Boolean(probe.panelText && probe.panelText.includes(spec.text));
      const otherMarkers = EXPECTED_TABS.filter(other => other.qa !== spec.qa && other.marker)
        .filter(other => other.marker !== spec.marker);
      let foreignMarker = false;
      for (const other of otherMarkers) {
        if (await page.locator(other.marker).count() > 0) { foreignMarker = true; break; }
      }
      check(`tab ${spec.label}: the active tab is the one whose panel is rendered`,
        selectedIsThisTab && markerPresent && !foreignMarker,
        `aria-selected=${selectedQa.join(',') || 'none'} marker=${spec.marker || spec.text} present=${markerPresent} чужойМаркер=${foreignMarker}`);
      // --- D5: футер вкладки
      const footerExpected = !FOOTERLESS[spec.qa];
      const footerLabels = probe.footerButtons.map(button => button.text);
      const footerOk = footerExpected
        ? probe.footerCount === 1 && footerLabels.length === 2
          && footerLabels[0] === 'Отмена' && footerLabels[1] === 'Сохранить'
        : probe.footerCount === 0 && footerLabels.length === 0;
      check(`tab ${spec.label}: the footer is ${footerExpected ? '«Отмена»+«Сохранить»' : 'absent'}`,
        footerOk,
        `footerCount=${probe.footerCount} buttons=${JSON.stringify(footerLabels)}`);
      // --- «сиротская» панель: ровно один .main-card-body (на «Раскладках» — два)
      const expectedBodies = spec.qa === 'conference-layouts-tab' ? 2 : 1;
      const orphanOk = probe.panelBodyCount === expectedBodies
        && probe.documentBodyCount === expectedBodies
        && Boolean(probe.panelText && probe.panelText.length > 3);
      check(`tab ${spec.label}: no orphan panel (${expectedBodies} .main-card-body in the panel)`,
        orphanOk,
        `panelBodyCount=${probe.panelBodyCount} documentBodyCount=${probe.documentBodyCount} panelTextLen=${(probe.panelText || '').length}`);
      if (spec.qa === 'conference-layouts-tab') {
        layoutsPanel = await checkLayoutsPanelContract(page, CHECKS_SELFTEST || REVERT_CHECK ? 'fixed-sim' : '');
        // --- D8: ровно ДВА .main-card-body, аудио-блок — второй ребёнок того же контейнера
        const secondChildIsAudio = probe.panelChildClasses
          && probe.panelChildClasses.length === 2
          && /conference-audio-channels/.test(probe.panelChildClasses[1]);
        check('tab Раскладки: exactly two .main-card-body (layouts + audio channels)',
          probe.panelBodyCount === 2 && probe.panelChildClasses.length === 2,
          `panelBodyCount=${probe.panelBodyCount} children=${JSON.stringify(probe.panelChildClasses)}`);
        check('tab Раскладки: the audio channel block is the second child of the same .main-card-body__container',
          secondChildIsAudio && probe.audioParentIsPanel,
          `children=${JSON.stringify(probe.panelChildClasses)} audioIndexInPanel=${probe.audioIndexInPanel} parentIsPanel=${probe.audioParentIsPanel}`);
        conferenceGeometry = await checkAudioBlockGeometry(page, 'conference',
          CHECKS_SELFTEST || REVERT_CHECK ? 'fixed-sim' : '',
          { clipVariant: CLIP_CHECK ? 'A' : null });
        if (CLIP_CHECK) {
          // второй способ обрезки: старая картинка 240px при нынешней политике
          // max-height:320px ⇒ кнопка срезана ВНУТРИ блока аудиоканалов
          clipGeometryB = await checkAudioBlockGeometry(page, 'conference',
            CHECKS_SELFTEST || REVERT_CHECK ? 'fixed-sim' : '',
            { clipVariant: 'B', label: '[clip-B]' });
        }
      } else {
        check(`tab ${spec.label}: the audio channel block is not mounted here`,
          !audioHere, `panelHasAudio=${audioHere}`);
      }
      tabWalk.push({
        qa: spec.qa, label: spec.label, present: true, markerOk, selectedOk, contentOk,
        panelCls: probe.panelCls, panelText: (probe.panelText || '').slice(0, 120),
        audioCount: probe.audioCount, audioInPanel: probe.panelHasAudio,
      });
      await shot(page, `tab-${spec.qa.replace('conference-', '').replace('-tab', '')}.png`);
    }

    // --- D5 (диагностика): вендер передаёт data-qa в TcButton, но в DOM кнопок
    // футера этих атрибутов НЕТ — фиксируем это явно, а проверку футера ведём
    // по подписям «Отмена»/«Сохранить».
    const footerQa = await page.evaluate(() => [...document.querySelectorAll('.main-card-footer button')]
      .map(button => button.getAttribute('data-qa')));
    check('conference footer: the buttons expose data-qa main-card-cancel-button / main-card-save-button',
      footerQa.includes('main-card-cancel-button') && footerQa.includes('main-card-save-button'),
      `data-qa=${JSON.stringify(footerQa)} — в билде 2.2.1.66 TcButton не пробрасывает data-qa в DOM, поэтому футер проверяется по подписям «Отмена»/«Сохранить»`,
      'known-issue');

    // --- 4b: счётчики в шапке не изменились (сразу после обхода вкладок)
    const countersAfter = await probeHeaderCounters(page);
    check('conference header: the counters did not change',
      JSON.stringify(countersBefore) === JSON.stringify(countersAfter),
      `before=${JSON.stringify(countersBefore)} after=${JSON.stringify(countersAfter)}`);

    // --- 5: D3 — клавиатурная навигация по ТАБ-БАРУ настоящей правки
    keyboard = await checkKeyboardNavigation(page);

    // --- 6: D2b/D7 — «Добавить» → редактор аудиоканала → «Отмена» → назад
    await page.locator(`${TAB_BAR} [data-qa="conference-layouts-tab"]`).click();
    await page.waitForTimeout(1200);
    editorRoundTrip = await checkAudioEditorRoundTrip(page, {
      scope: 'conference', url: conferenceUrl, audioSelector: AUDIO_BLOCK, layoutsTabQa: 'conference-layouts-tab',
    });

    // --- 6b: N7 на низких вьюпортах — список раскладок сжимается и скроллится
    // сам, но блок аудиоканалов обязан остаться целым
    await page.locator(`${TAB_BAR} [data-qa="conference-layouts-tab"]`).click();
    await page.waitForTimeout(1200);
    lowViewportGeometry = await checkLowViewportGeometry(page, 'conference',
      CHECKS_SELFTEST || REVERT_CHECK ? 'fixed-sim' : '');

    // --- 7: D1/D6 — ветка СЕССИИ (настоящий маршрут /pnm/monitoring/sessions/{id})
    sessionWalk = await checkSessionBranch(page);
    // sessionWalk — массив обхода вкладок; клавиатура и геометрия сессии
    // лежат в свойствах массива и в JSON не попадают — выносим отдельно
    sessionKeyboard = sessionWalk.keyboard || null;
    sessionWalk = { tabs: sessionWalk, keyboard: sessionKeyboard };

    // --- 6: сетка раскладок в редакторе раскладок (та же вкладка «Раскладки»)
    await page.goto(`${BASE}/pnm/monitoring/conferences/${CONFERENCE_ID}/layouts`, { waitUntil: 'networkidle' });
    await page.waitForTimeout(2000);
    await page.getByText('Ручная настройка', { exact: true }).click().catch(() => {});
    await page.waitForTimeout(1200);
    const rangeButtons = await page.locator('.layout-size-range-selector button')
      .evaluateAll(els => els.map(el => (el.innerText || '').trim()));
    check('layout editor: size range selector visible', rangeButtons.length > 0, JSON.stringify(rangeButtons));

    try {
      await page.getByText(/Получатели/).first().click();
      await page.waitForTimeout(800);
      const addButton = page.getByRole('button', { name: /Добавить/ }).last();
      if (await addButton.count()) {
        await addButton.click();
        await page.waitForTimeout(800);
        const row = page.locator('.popup').last().getByText(PARTICIPANT, { exact: false }).first();
        if (await row.count()) {
          await row.click();
          await page.waitForTimeout(600);
          await page.getByRole('button', { name: /Добавить/ }).last().click();
          await page.waitForTimeout(800);
          check('layout editor: contact added to the layout recipients', true, PARTICIPANT);
        } else {
          check('layout editor: contact added to the layout recipients', false, `«${PARTICIPANT}» not in the recipients popup`);
        }
      }
      await page.getByText('Основное', { exact: true }).first().click();
      await page.waitForTimeout(800);
    } catch (error) {
      check('layout editor: contact added to the layout recipients', false, String(error).slice(0, 200));
      await page.getByText('Основное', { exact: true }).first().click().catch(() => {});
      await page.waitForTimeout(500);
    }

    for (let index = 0; index < SIZES.length; index += 1) {
      const spec = SIZES[index];
      const name = `${spec.size}${spec.plus ? '+' : ''}`;
      const file = slug(spec.label, index + 1);
      const sizeButton = page.locator('.layout-size-range-selector button')
        .filter({ hasText: new RegExp(`^${spec.label.replace(/[+]/g, '\\+')}$`) }).first();
      if (!(await sizeButton.count())) {
        check(`size ${name}: range button present`, false, spec.label);
        sizes.push({ size: spec.size, plus: Boolean(spec.plus), error: 'size button not found' });
        continue;
      }
      await sizeButton.click({ timeout: 10000 });
      await page.waitForTimeout(900);
      const tiles = await page.locator('.layout-grid-slot-wrapper').count();
      const placement = await placeParticipant(page, 1);
      const measurement = await measureTile(page, 1);
      const record = { size: spec.size, plus: Boolean(spec.plus), label: spec.label, tiles, ...measurement };
      sizes.push(record);
      if (!measurement.labelFound) {
        check(`size ${name}: participant label found`, false, 'label (.layout-grid-slot-dn__text) not rendered');
        continue;
      }
      const ratio = measurement.labelFontSizePx / measurement.slotFontSizePx;
      check(`size ${name}: label font-size is ${LABEL_FONT_RATIO}x slot font-size`,
        Math.abs(ratio - LABEL_FONT_RATIO) <= FONT_RATIO_TOLERANCE,
        `label=${measurement.labelFontSizePx}px slot=${measurement.slotFontSizePx}px ratio=${ratio.toFixed(2)}`);
      check(`size ${name}: label centered in the tile`,
        Math.abs(measurement.dx) <= CENTER_DX && Math.abs(measurement.dy) <= CENTER_DY,
        `dx=${measurement.dx.toFixed(2)} dy=${measurement.dy.toFixed(2)}`);
      check(`size ${name}: label fully inside the tile`,
        measurement.labelInsideTile && measurement.labelInsideTileX,
        `inside=${measurement.labelInsideTile} insideX=${measurement.labelInsideTileX} overflowX=${measurement.overflowX}`);
      if (index === 0 || index === SIZES.length - 1) {
        await shot(page, `layout-size-${file}.png`);
      }
    }
    await closeSlotPanel(page);

    // --- 8: режим отката — откатываем «исправленное» состояние и обязаны упасть
    if (REVERT_CHECK) {
      console.log('\n[REVERT_CHECK] собираем исправленное состояние и откатываем его');
      await page.goto(conferenceUrl, { waitUntil: 'networkidle' });
      await page.waitForTimeout(2500);
      // Положительный контроль: в «исправленном» состоянии контракт выполняется.
      await captureFixedState(page, state);
      await applySimulatedFix(page, state);
      await page.locator(`${TAB_BAR} [data-qa="conference-layouts-tab"]`).click().catch(() => {});
      await page.waitForTimeout(1200);
      await applySimulatedFix(page, state);
      const preRollback = await runContractChecks(page, 'revert-pre');
      await checkLayoutsPanelContract(page, 'revert-pre');
      check('revert check: the pre-rollback state satisfies the contract',
        !preRollback.tabs.some(tab => tab.qa === AUDIO_TAB_QA) && preRollback.panelHasAudio,
        `audioTabPresent=${preRollback.tabs.some(tab => tab.qa === AUDIO_TAB_QA)} panelHasAudio=${preRollback.panelHasAudio}`);
      // Откат: вкладка аудио возвращается, блок аудиоканалов уезжает из панели раскладок.
      await revertFixedState(page, state);
      // НЕ переключаем вкладки после отката: перерисовка вернула бы блок на место
      await page.waitForTimeout(600);
      revertProbe = await page.evaluate(TAB_PROBE);
      check('revert check: the rollback is in place (the audio tab is back, the block left the panel)',
        revertProbe.tabs.some(tab => tab.qa === AUDIO_TAB_QA) && !revertProbe.panelHasAudio,
        `tabs=${revertProbe.tabs.map(t => t.qa).join(',')} panelHasAudio=${revertProbe.panelHasAudio}`);
      // Контрактные проверки на откатанном состоянии — они обязаны упасть.
      await runContractChecks(page, 'revert-post');
      await checkLayoutsPanelContract(page, 'revert-post');
      // Тот же откат — на ветке СЕССИИ (там контракт тоже fatal).
      await page.goto(`${BASE}/pnm/monitoring/sessions/${SESSION_ID}`, { waitUntil: 'networkidle' });
      await page.waitForTimeout(2500);
      await page.locator(`${TAB_BAR} [data-qa="session-main-tabs-layouts"]`).click().catch(() => {});
      await page.waitForTimeout(1200);
      await revertFixedState(page, state, 'session');
      await page.waitForTimeout(600);
      const sessionRevert = await checkSessionTabBarContract(page, 'revert-post');
      const sessionRevertProbe = await page.evaluate(TAB_PROBE);
      check('revert check (session): the rollback is in place (the audio tab is back, the block left the panel)',
        sessionRevert.tabs.some(tab => /audio/i.test(tab.qa || '')) && !sessionRevertProbe.panelHasAudio,
        `tabs=${sessionRevert.tabs.map(t => t.qa).join(',')} panelHasAudio=${sessionRevertProbe.panelHasAudio}`);
      check('[revert-post] session tab Раскладки: the audio channel block is rendered inside the layouts panel',
        sessionRevertProbe.panelHasAudio,
        `panelHasAudio=${sessionRevertProbe.panelHasAudio} audioCount=${sessionRevertProbe.audioCount}`);
    }

    // --- 9: глобальные проверки
    const envConsoleErrors = consoleErrors.filter(isEnvError);
    const realConsoleErrors = consoleErrors.filter(text => !isEnvError(text) && !ABORTED_PATTERN.test(text));
    const serverFailures = failedRequests.filter(isServerFailure);
    const outage = serverFailures.length > 0;
    if (outage) {
      console.log(`\n     (среда: во время прогона сервер не отвечал — ${serverFailures.length} неудачных запросов; `
        + 'ошибки сети отделены от ошибок UI)');
    }
    check('ui: no pageerror', pageErrors.length === 0, JSON.stringify(pageErrors).slice(0, 500),
      outage ? 'known-issue' : 'fatal');
    check('ui: no console errors', realConsoleErrors.length === 0,
      `${JSON.stringify(realConsoleErrors).slice(0, 800)}`
      + (outage ? ` (отделено сетевых ошибок недоступности сервера: ${envConsoleErrors.length})` : ''),
      outage ? 'known-issue' : 'fatal');
    if (outage) {
      check('ui: the realtime channel (WebSocket) was available during the run', false,
        `неудачных запросов: ${serverFailures.length}, первые: ${JSON.stringify(serverFailures.slice(0, 3)).slice(0, 300)}`,
        'known-issue');
    }
    const writes = nonGetRequests.filter(request => !request.url.includes('/video_layouts/calculate')
      && !request.url.includes('/api/v3/login'));
    check('db: no writes to the live stand (draft only)', writes.length === 0, JSON.stringify(writes).slice(0, 500));
  } catch (error) {
    failure = error;
    console.error((error && error.stack) || error);
  } finally {
    if (browser) await browser.close().catch(() => {});
    const fatalChecks = checks.filter(entry => entry.severity === 'fatal');
    const failedFatal = fatalChecks.filter(entry => !entry.ok);
    const contractChecks = checks.filter(entry => entry.name.includes('conference tab bar')
      || entry.name.includes('tab Раскладки'));
    const gridChecks = checks.filter(entry => entry.name.startsWith('size ') || entry.name.startsWith('layout editor'));
    const ok = !failure && failedFatal.length === 0;
    const result = {
      ok,
      base: BASE,
      conference_id: CONFERENCE_ID,
      participant: PARTICIPANT,
      duration_ms: Date.now() - startedAt,
      modes: { checksSelfTest: CHECKS_SELFTEST, revertCheck: REVERT_CHECK },
      criteria: {
        centerDx: CENTER_DX, centerDy: CENTER_DY, labelFontRatio: LABEL_FONT_RATIO,
        fontRatioTolerance: FONT_RATIO_TOLERANCE, expectedTabs: EXPECTED_TABS.map(tab => tab.qa),
      },
      summary: {
        contractChecksOk: contractChecks.length > 0 && contractChecks.every(entry => entry.ok),
        contractChecksTotal: contractChecks.length,
        gridChecksOk: gridChecks.length > 0 && gridChecks.every(entry => entry.ok),
        gridChecksTotal: gridChecks.length,
        sizesMeasured: sizes.length,
        sizesWithLabel: sizes.filter(item => item.labelFound).length,
        failedFatalChecks: failedFatal.map(entry => entry.name),
        knownIssues: checks.filter(entry => entry.severity === 'known-issue' && !entry.ok).map(entry => entry.name),
      },
      tabWalk,
      sessionWalk,
      geometry: {
        conference: conferenceGeometry,
        session: sessionGeometry,
        clipVariantB: clipGeometryB,
        clipVariantBSession: clipGeometryBSession,
        lowViewports: lowViewportGeometry,
      },
      keyboard,
      editorRoundTrip,
      layoutsPanel,
      revertProbe,
      sizes,
      checks,
      page_errors: pageErrors,
      console_errors: consoleErrors,
      failed_requests: failedRequests,
      non_get_requests: nonGetRequests,
      failure: failure ? String((failure && failure.stack) || failure) : null,
    };
    fs.writeFileSync(path.join(ART, 'result.json'), JSON.stringify(result, null, 2));
    console.log('\n===== SUMMARY =====');
    console.log(JSON.stringify({
      ok: result.ok,
      modes: result.modes,
      contractChecksOk: result.summary.contractChecksOk,
      contractChecksTotal: result.summary.contractChecksTotal,
      gridChecksOk: result.summary.gridChecksOk,
      gridChecksTotal: result.summary.gridChecksTotal,
      sizesWithLabel: result.summary.sizesWithLabel,
      failedFatalChecks: result.summary.failedFatalChecks,
      knownIssues: result.summary.knownIssues,
    }, null, 2));
    console.log(`artifacts: ${ART}`);
    process.exit(ok ? 0 : (failure ? 2 : 1));
  }
})();

#!/usr/bin/env node
/*
 * Живой E2E-измеритель подписи участника в раскладке (PNM 2.2.1.66 + CSS-оверрайд
 * `prostor-pnm-overrides`) и сверка счётчиков списка мониторинга с API.
 *
 * (A) Логин → страница раскладок конференции → «Ручная настройка» →
 *     для каждого размера раскладки (1, 2, 3, 4, 5+, 8, 9, 10+, 25, 34+)
 *     заново сажаем контакт в ячейку 1 и измеряем подпись
 *     (`.layout-grid-slot-dn__text`) относительно плитки (`.layout-grid-slot-wrapper`).
 *     Ключевой регрессионный признак: подпись должна быть вдвое крупнее
 *     плитки (правило `.layout-grid-slot .layout-grid-slot-dn__text{font-size:2em}`),
 *     центрирована и целиком внутри плитки.
 *
 * (B) Счётчики в списке мониторинга (/pnm/monitoring/conferences) против
 *     /api/v3/ui/conferences, против /api/v3/conferences/{id}/contacts
 *     и против счётчиков в шапке карточки комнаты.
 *
 * (A2) Аватар участника в плитке НЕ отображается. Проверка «скрыт» принимает
 *      любой механизм: display:none, visibility:hidden|collapse, opacity:0,
 *      нулевой bbox, обрезка/вынос из плитки, отсутствие в DOM. Дополнительно
 *      проверяется, что подпись ничем не отжимается вниз внутри блока контента
 *      (0 em вместо 1.335 em) — иначе аватар «спрятан», но место под него
 *      осталось и подпись уехала. Негативные контроли: аватары ВНЕ плитки
 *      (панель видеслота, таблица «Получатели», шапка) остаются видимыми;
 *      иконка роли в пустой плитке не сдвигается; плитка с превью кадра
 *      (MediaPreview 24×24 — там аватара в DOM нет вовсе) центрирована.
 *
 * Запуск: NODE_PATH=<node_modules с playwright> node tests/e2e/label-zoom.js
 * Артефакты: artifacts/e2e-label-zoom/{result.json,size-*.png,monitoring-list.png}
 *
 * Служебные режимы:
 *   VKS_APPLY_CANDIDATE=1 — инъекция кандидата фикса в страницу (index.html не
 *       трогаем), ожидается exit 0; VKS_REVERT_CHECK=1 — откат фикса
 *       (аватар снова виден), ожидается exit 1: харнесс обязан ловить откат.
 *
 * Скрипт НИЧЕГО не пишет в живую БД: раскладка не сохраняется («Создать» не
 * нажимается), посадка участника и добавление в «Получатели» — только
 * клиентский черновик (единственный не-GET запрос — POST .../video_layouts/calculate).
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
const ART = path.resolve(process.env.VKS_ARTIFACTS || 'artifacts/e2e-label-zoom');
const CHROMIUM = process.env.VKS_CHROMIUM
  || '/home/pro/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome';
// Строгий режим (исторический): после фикса счётчиков (e81811f) проверка
// «контакты комнаты vs счётчики» fatal всегда, флаг оставлен для совместимости.
const STRICT_COUNTS = process.env.VKS_STRICT_COUNTS === '1';
// Режим самопроверки харнесса: откатываем CSS-фикс (аватар снова виден) и
// ОБЯЗАНЫ упасть (exit 1) на проверке «avatar hidden». exit 0 в этом режиме —
// ложный пропуск: значит харнесс не умеет ловить откат.
const REVERT_CHECK = process.env.VKS_REVERT_CHECK === '1';
// Режим проверки фикса без правки index.html: инъекция кандидата в страницу.
const APPLY_CANDIDATE = process.env.VKS_APPLY_CANDIDATE === '1';
// CSS-кандидат фикса (аватар убран + пересчёт центровки + страховка для плитки
// с превью кадра MediaPreview 24×24, где аватара в DOM нет вовсе).
const CANDIDATE_CSS = `
.layout-grid-slot .layout-grid-slot__avatar { display: none; }
.layout-grid-slot-content:has(.layout-grid-slot-dn) { transform: translateY(-1em); }
.layout-grid-slot-content:has(.layout-grid-slot-dn):not(:has(.layout-grid-slot__avatar)) { transform: translateY(calc(-1em - 12px - 0.335em)); }
`;
// Откат фикса для самопроверки харнесса.
const REVERT_CSS = `
.layout-grid-slot .layout-grid-slot__avatar { display: block !important; }
`;

// Размеры раскладки: id — для JSON, label — точный текст кнопки в
// `.layout-size-range-selector` (последняя, icon-only «overlap», НЕ трогаем).
const SIZES = [
  { size: 1, label: '1' },
  { size: 2, label: '2' },
  { size: 3, label: '3' },
  { size: 4, label: '4' },
  { size: 5, label: '5 +', plus: true },
  { size: 8, label: '8' },
  { size: 9, label: '9' },
  { size: 10, label: '10 +', plus: true },
  { size: 25, label: '25' },
  { size: 34, label: '34 +', plus: true },
];

// Критерии центровки (из tests/e2e/live.js:442-455).
const CENTER_DX = 1.5;
const CENTER_DY = 3;
// Ожидаемое отношение font-size подписи к font-size плитки (CSS-фикс 2em).
const LABEL_FONT_RATIO = 2;
const FONT_RATIO_TOLERANCE = 0.15;
// Допустимое «свободное место над подписью» внутри блока контента, в em плитки.
// 0 — аватар убран из потока; 1.335em — аватар оставлен и отжимает подпись вниз.
const CONTENT_HEADROOM_EM = 0.5;

fs.mkdirSync(ART, { recursive: true });

const checks = [];
const pageErrors = [];
const consoleErrors = [];
const nonGetRequests = [];
const startedAt = Date.now();
let failure = null;

/**
 * Регистрирует проверку. НЕ бросает: отсутствие подписи не должно ронять прогон.
 * severity: 'fatal' (по умолчанию) | 'known-issue' (не влияет на код выхода,
 * пока не включён VKS_STRICT_COUNTS) | 'info' (справочно).
 */
function check(name, ok, detail, severity = 'fatal') {
  const entry = { name, ok: Boolean(ok), severity, detail: detail === undefined ? null : detail };
  checks.push(entry);
  const mark = entry.ok ? 'ok  ' : (severity === 'fatal' ? 'FAIL' : `WARN(${severity})`);
  console.log(`${mark} ${name}${entry.detail ? ` — ${entry.detail}` : ''}`);
  return entry.ok;
}

async function api(token, method, urlPath, body) {
  const response = await fetch(`${BASE}${urlPath}`, {
    method,
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let json = null;
  try { json = text ? JSON.parse(text) : null; } catch { /* not json */ }
  return { status: response.status, json, text };
}

/** Имя файла скриншота: «5 +» → «05-5-plus». */
const slug = (label, index) => {
  const base = String(label).trim().toLowerCase()
    .replace(/\s*\+\s*$/, '-plus')
    .replace(/\s+/g, '-')
    .replace(/-$/, '');
  return `${String(index).padStart(2, '0')}-${base}`;
};

/**
 * Замер плитки (1-based): геометрия подписи + «виден ли аватар участника».
 * Всё считается в браузере, чтобы не тащить геометрию через JSON.
 *
 * «Видимость для пользователя» одним вызовом не проверяется — браузер
 * раскрывает это на несколько независимых сигналов, и реализация фикса может
 * быть любой из них (или их комбинацией):
 *   1) display:none / visibility:hidden|collapse / opacity:0 — в т.ч. у предков;
 *   2) нулевая площадь (width/height:0, scale(0)) — элемент в потоке, но не рисуется;
 *   3) элемент вынесен/обрезан (не попадает в hit-test по центру бокса);
 *   4) элемента нет в DOM вовсе.
 * Плюс `checkVisibility({checkOpacity:true, checkVisibilityCSS:true})` как
 * независимое подтверждение (1) и `offsetParent === null` как признак
 * display:none у предков.
 */
async function measureTile(page, position) {
  const tile = page.locator('.layout-grid-slot-wrapper').nth(position - 1);
  if (!(await tile.count())) return { labelFound: false, error: `tile ${position} not rendered` };
  return tile.evaluate(element => {
    const box = el => {
      const b = el.getBoundingClientRect();
      return {
        left: b.left, top: b.top, right: b.right, bottom: b.bottom,
        width: b.width, height: b.height,
      };
    };
    /** Состояние «виден ли пользователю» для произвольного элемента. */
    const visibleState = el => {
      if (!el) return { visible: false, reason: 'absent-from-dom' };
      const r = el.getBoundingClientRect();
      const cs = getComputedStyle(el);
      const base = {
        display: cs.display, visibility: cs.visibility, opacity: cs.opacity,
        boxW: +r.width.toFixed(2), boxH: +r.height.toFixed(2),
      };
      if (cs.display === 'none') return { ...base, visible: false, reason: 'display:none' };
      if (cs.visibility === 'hidden' || cs.visibility === 'collapse') return { ...base, visible: false, reason: `visibility:${cs.visibility}` };
      if (parseFloat(cs.opacity) === 0) return { ...base, visible: false, reason: 'opacity:0' };
      if (r.width <= 0 || r.height <= 0) return { ...base, visible: false, reason: 'zero-size-box' };
      if (el.offsetParent === null && cs.position !== 'fixed') return { ...base, visible: false, reason: 'offsetParent===null' };
      if (typeof el.checkVisibility === 'function'
        && !el.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true })) {
        return { ...base, visible: false, reason: 'checkVisibility=false' };
      }
      const cx = r.left + r.width / 2, cy = r.top + r.height / 2;
      const inViewport = cx >= 0 && cy >= 0 && cx <= window.innerWidth && cy <= window.innerHeight;
      if (inViewport && cs.pointerEvents !== 'none') {
        const hit = document.elementFromPoint(cx, cy);
        if (!hit || !(hit === el || el.contains(hit))) {
          return { ...base, visible: false, reason: 'not-hit-testable (clipped or moved out)' };
        }
      }
      return { ...base, visible: true, reason: null };
    };

    const text = element.querySelector('.layout-grid-slot-dn__text');
    const content = element.querySelector('.layout-grid-slot-content');
    const avatar = element.querySelector('.layout-grid-slot__avatar');
    const tileBox = box(element);
    const slotFontSizePx = parseFloat(getComputedStyle(element).fontSize);

    // «Блок идентичности» в плитке: вендор рисует ЛИБО аватар (TcAvatar с классом
    // .layout-grid-slot__avatar), ЛИБО превью кадра (MediaPreview → .media-preview,
    // когда у контакта есть preview). Поэтому берём и оба класса, и структурно —
    // все потоковые дети .layout-grid-slot-content, кроме блока подписи.
    const flowChildren = content
      ? [...content.children].filter(el => !el.classList.contains('layout-grid-slot-dn'))
      : [];
    const identityEls = [...new Set([
      ...flowChildren,
      ...(content ? content.querySelectorAll('.layout-grid-slot__avatar, .media-preview') : []),
    ])];
    const identity = identityEls.map(el => {
      const own = visibleState(el);
      // Крашеные потомки (внутренний .tc-avatar, <img> превью): если виден хоть
      // один, «блок идентичности» считается нарисованным.
      const painted = [...el.querySelectorAll('.tc-avatar, img, svg, .tc-avatar__name-icon')]
        .filter(child => visibleState(child).visible);
      const cls = String(el.className);
      return {
        cls: cls.slice(0, 70),
        isPreview: cls.includes('media-preview'),
        ...own,
        paintedDescendants: painted.length,
        visible: own.visible || painted.length > 0,
        reason: own.visible ? own.reason : (painted.length ? 'painted-descendant' : own.reason),
      };
    });
    // Аватар (TcAvatar) и превью кадра (MediaPreview) — разные сущности: вендор
    // рисует ОДНО из них. Требование «аватар убран» относится только к аватару;
    // превью (24×24 px) в плитке остаётся и отжимает подпись — это отдельная ветка.
    const avatarBlocks = identity.filter(item => !item.isPreview);
    const previewBlocks = identity.filter(item => item.isPreview);

    if (!text) {
      return {
        labelFound: false,
        tileText: (element.innerText || '').trim(),
        tileW: tileBox.width, tileH: tileBox.height,
        slotFontSizePx,
        hasAvatar: Boolean(avatar),
        identity,
        anyIdentityVisible: identity.some(item => item.visible),
        avatarBlockFound: avatarBlocks.length > 0,
        avatarVisible: avatarBlocks.some(item => item.visible),
        previewBlockFound: previewBlocks.length > 0,
      };
    }
    const labelBox = box(text);
    const labelStyle = getComputedStyle(text);
    const slotStyle = getComputedStyle(element);
    const avatarBox = avatar ? box(avatar) : null;
    const contentBox = content ? box(content) : null;
    const labelCenterY = labelBox.top + labelBox.height / 2;
    const labelCenterX = labelBox.left + labelBox.width / 2;
    return {
      labelFound: true,
      labelText: (text.innerText || '').trim(),
      labelFontSizePx: parseFloat(labelStyle.fontSize),
      labelLineHeight: labelStyle.lineHeight,
      labelTextOverflow: labelStyle.textOverflow,
      slotFontSizePx,
      tileW: tileBox.width, tileH: tileBox.height,
      labelW: labelBox.width, labelH: labelBox.height,
      dx: labelCenterX - (tileBox.left + tileBox.width / 2),
      dy: labelCenterY - (tileBox.top + tileBox.height / 2),
      labelTopVsTileTop: labelBox.top - tileBox.top,
      labelBottomVsTileBottom: labelBox.bottom - tileBox.bottom,
      labelLeftVsTileLeft: labelBox.left - tileBox.left,
      labelRightVsTileRight: labelBox.right - tileBox.right,
      overflowX: text.scrollWidth > text.clientWidth + 1,
      overflowY: text.scrollHeight > text.clientHeight + 1,
      scrollWidth: text.scrollWidth, clientWidth: text.clientWidth,
      scrollHeight: text.scrollHeight, clientHeight: text.clientHeight,
      labelInsideTile: labelBox.top >= tileBox.top - 0.5 && labelBox.bottom <= tileBox.bottom + 0.5,
      labelInsideTileX: labelBox.left >= tileBox.left - 0.5 && labelBox.right <= tileBox.right + 0.5,
      hasAvatar: Boolean(avatar),
      avatarW: avatarBox ? avatarBox.width : null,
      avatarH: avatarBox ? avatarBox.height : null,
      // Насколько подпись смещена внутри блока контента: 0 — ничто её не отжимает
      // (аватар убран из потока), +1.335em — аватар оставлен и отжимает подпись вниз.
      labelVsContent: contentBox ? {
        dx: +(labelCenterX - (contentBox.left + contentBox.width / 2)).toFixed(3),
        dy: +(labelCenterY - (contentBox.top + contentBox.height / 2)).toFixed(3),
        dyEm: +((labelCenterY - (contentBox.top + contentBox.height / 2)) / slotFontSizePx).toFixed(3),
      } : null,
      identity,
      anyIdentityVisible: identity.some(item => item.visible),
      avatarBlockFound: avatarBlocks.length > 0,
      avatarVisible: avatarBlocks.some(item => item.visible),
      previewBlockFound: previewBlocks.length > 0,
    };
  });
}

/** Аватары ВНЕ плитки раскладки — фикс не должен задевать их. */
async function probeOutsideAvatars(page) {
  return page.evaluate(() => {
    const state = el => {
      const r = el.getBoundingClientRect();
      const cs = getComputedStyle(el);
      const cv = typeof el.checkVisibility === 'function'
        ? el.checkVisibility({ checkOpacity: true, checkVisibilityCSS: true }) : null;
      const visible = cs.display !== 'none' && cs.visibility !== 'hidden' && cs.visibility !== 'collapse'
        && parseFloat(cs.opacity) !== 0 && r.width > 0 && r.height > 0 && cv !== false;
      return {
        cls: String(el.className).slice(0, 60),
        boxW: +r.width.toFixed(1), boxH: +r.height.toFixed(1),
        visible,
        inSlotPanel: Boolean(el.closest('.grid-slot-side-bar')),
        inRecipients: Boolean(el.closest('.layout-table-item-receiver, [class*="receiver"]')),
        inHeader: Boolean(el.closest('.header-profile, header')),
      };
    };
    const list = [...document.querySelectorAll('.tc-avatar, .tc-avatar-container, .media-preview')]
      .filter(el => !el.closest('.layout-grid-slot-wrapper'))
      .map(state);
    return {
      total: list.length,
      visible: list.filter(item => item.visible).length,
      inSlotPanelVisible: list.filter(item => item.inSlotPanel && item.visible).length,
      inRecipientsVisible: list.filter(item => item.inRecipients && item.visible).length,
      inHeaderVisible: list.filter(item => item.inHeader && item.visible).length,
      items: list,
    };
  });
}

/** Иконка роли в пустых плитках: не должна «уезжать» вместе с сеткой. */
async function probeEmptyTiles(page) {
  return page.evaluate(() => {
    const out = [];
    for (const tile of document.querySelectorAll('.layout-grid-slot-wrapper')) {
      const icon = tile.querySelector('.layout-grid-slot__icon');
      if (!icon) continue;
      const content = tile.querySelector('.layout-grid-slot-content');
      const tr = tile.getBoundingClientRect();
      const ir = icon.getBoundingClientRect();
      const cr = content.getBoundingClientRect();
      const slotFontSizePx = parseFloat(getComputedStyle(tile).fontSize);
      const iconVsTileDy = (ir.top + ir.height / 2) - (tr.top + tr.height / 2);
      out.push({
        iconVsTileDyPx: +iconVsTileDy.toFixed(2),
        iconVsTileDyEm: +(iconVsTileDy / slotFontSizePx).toFixed(3),
        iconVsContentDyPx: +((ir.top + ir.height / 2) - (cr.top + cr.height / 2)).toFixed(2),
        contentTransform: getComputedStyle(content).transform,
        iconBox: { w: +ir.width.toFixed(1), h: +ir.height.toFixed(1) },
      });
      if (out.length >= 3) break;
    }
    return out;
  });
}

/**
 * Имитация плитки с ПРЕВЬЮ кадра: вендор рисует MediaPreview (.media-preview, 24×24 px)
 * ВМЕСТО аватара, и тогда в DOM плитки нет .layout-grid-slot__avatar вовсе, а блок
 * превью (24px) + gap (.67em) отжимают подпись вниз. Меняем аватар на превью,
 * меряем и возвращаем обратно (всё синхронно, без гонки с ререндером Vue).
 */
async function simulatePreviewTile(page, position) {
  const tile = page.locator('.layout-grid-slot-wrapper').nth(position - 1);
  if (!(await tile.count())) return { simulated: false, reason: `tile ${position} not rendered` };
  return tile.evaluate(element => {
    const content = element.querySelector('.layout-grid-slot-content');
    const label = element.querySelector('.layout-grid-slot-dn__text');
    if (!content || !label) return { simulated: false, reason: 'no content/label in the tile' };
    const avatar = content.querySelector('.layout-grid-slot__avatar');
    const previous = avatar || content.firstElementChild;
    const preview = document.createElement('div');
    preview.className = 'media-preview';
    preview.style.width = '24px';
    preview.style.height = '24px';
    if (previous) previous.replaceWith(preview); else content.prepend(preview);
    const tr = element.getBoundingClientRect();
    const lr = label.getBoundingClientRect();
    const cr = content.getBoundingClientRect();
    const result = {
      simulated: true,
      hadAvatarElement: Boolean(avatar),
      dy: +((lr.top + lr.height / 2) - (tr.top + tr.height / 2)).toFixed(3),
      dx: +((lr.left + lr.width / 2) - (tr.left + tr.width / 2)).toFixed(3),
      labelVsContentDy: +((lr.top + lr.height / 2) - (cr.top + cr.height / 2)).toFixed(3),
      contentTransform: getComputedStyle(content).transform,
    };
    if (previous) preview.replaceWith(previous); else preview.remove();
    return result;
  });
}

/** Закрыть выехавшую панель видеослота (иначе она сужает сетку). */
async function closeSlotPanel(page) {
  const close = page.locator('.grid-slot-side-bar button').filter({ hasText: 'close' }).first();
  if (await close.count()) {
    await close.click({ timeout: 5000 }).catch(() => {});
    await page.waitForTimeout(400);
  }
  const stillOpen = await page.locator('.grid-slot-side-bar').isVisible().catch(() => false);
  if (stillOpen) {
    await page.keyboard.press('Escape').catch(() => {});
    await page.waitForTimeout(300);
  }
  return !(await page.locator('.grid-slot-side-bar').isVisible().catch(() => false));
}

/** Посадить контакт в ячейку `position` (1-based) через панель видеослота. */
async function placeParticipant(page, position) {
  const cell = page.locator('.layout-grid-slot-wrapper').nth(position - 1);
  if (!(await cell.count())) return { placed: false, error: `tile ${position} not rendered` };
  await cell.scrollIntoViewIfNeeded().catch(() => {});
  await cell.click({ timeout: 10000 });
  await page.waitForTimeout(700);
  // Список контактов комнаты в панели видеослота.
  const scoped = page.locator('.grid-slot-contact-item').getByText(PARTICIPANT, { exact: false });
  let target = scoped.last();
  if (!(await scoped.count())) target = page.getByText(PARTICIPANT, { exact: false }).last();
  if (!(await target.count())) {
    return { placed: false, error: `contact «${PARTICIPANT}» not listed in the slot panel` };
  }
  await target.click({ timeout: 10000 });
  await page.waitForTimeout(700);
  const labeled = await page.locator('.layout-grid-slot-wrapper .layout-grid-slot-dn__text').count();
  return { placed: labeled > 0, error: labeled > 0 ? null : 'slot panel click did not produce a label' };
}

/** Число из контейнера со счётчиком (`.monitoring-*-container` / `__additional-container`). */
const readCounter = element => {
  const texts = [...element.querySelectorAll('span')]
    .map(span => (span.innerText || '').trim())
    .filter(text => /^\d+$/.test(text));
  return texts.length ? Number(texts[texts.length - 1]) : null;
};

(async () => {
  let browser;
  let context;
  let token = '';
  const sizes = [];
  let counters = null;
  let emptyTileBaseline = null;
  let emptyTileFinal = null;
  let previewSim = null;
  let outsideBaseline = null;
  try {
    browser = await chromium.launch({ headless: true, executablePath: CHROMIUM, args: ['--no-sandbox'] });
    context = await browser.newContext({ viewport: { width: 1600, height: 1000 }, ignoreHTTPSErrors: true });
    const page = await context.newPage();
    page.on('console', message => {
      if (message.type() === 'error' && consoleErrors.length < 40) consoleErrors.push(message.text());
    });
    page.on('pageerror', error => {
      if (pageErrors.length < 20) pageErrors.push(String((error && error.stack) || error));
    });
    page.on('request', request => {
      if (request.method() !== 'GET') {
        const url = request.url();
        // Тело логина содержит пароль — в артефакты не пишем.
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
    token = await page.evaluate(() => localStorage.getItem('TrueConfMCU_AccessToken'));
    check('login: token in localStorage', Boolean(token));
    if (!token) throw new Error('login failed: no access token in localStorage');
    check('login: token is JWT', typeof token === 'string' && token.split('.').length === 3);

    // Предусловие: контакт должен быть в комнате — иначе подпись измерять не на чем,
    // и прогон обязан НЕ сообщить об успехе (иначе это ложный зелёный).
    const preconditionResponse = await api(token, 'GET', `/api/v3/conferences/${CONFERENCE_ID}/contacts`);
    const preconditionNames = ((preconditionResponse.json && preconditionResponse.json.contacts) || [])
      .map(contact => contact.display_name);
    const preconditionOk = preconditionNames.includes(PARTICIPANT);
    check('precondition: participant contact present in the conference', preconditionOk,
      `contacts=${JSON.stringify(preconditionNames)}`);
    if (!preconditionOk) {
      throw new Error(`precondition failed: contact «${PARTICIPANT}» is not in the conference — the label cannot be measured`);
    }

    // ------------------------------------------------- (A) измерения подписи
    await page.goto(`${BASE}/pnm/monitoring/conferences/${CONFERENCE_ID}/layouts`, { waitUntil: 'networkidle' });
    await page.waitForTimeout(1500);
    await page.getByText('Ручная настройка', { exact: true }).click();
    await page.waitForTimeout(1000);
    // Базовая геометрия пустых плиток ДО инъекции — для сравнения «до/после».
    emptyTileBaseline = await probeEmptyTiles(page);
    // Снимок аватаров вне плитки ДО инъекции — сравнение «до/после» на одной странице.
    outsideBaseline = await probeOutsideAvatars(page);
    if (APPLY_CANDIDATE || REVERT_CHECK) {
      const css = REVERT_CHECK ? REVERT_CSS : CANDIDATE_CSS;
      await page.addStyleTag({ content: css });
      console.log(`\n[${REVERT_CHECK ? 'VKS_REVERT_CHECK=1' : 'VKS_APPLY_CANDIDATE=1'}] инъекция CSS:\n${css}`);
      await page.waitForTimeout(500);
    }
    const rangeButtons = await page.locator('.layout-size-range-selector button').evaluateAll(els => els.map(el => (el.innerText || '').trim()));
    check('layout: size range selector visible', rangeButtons.length > 0, JSON.stringify(rangeButtons));

    // Добавить контакт в «Получатели» раскладки (клиентский черновик, без записи в БД):
    // так сценарий совпадает с рабочим вендорным сценарием.
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
          check('layout: contact added to layout recipients', true, PARTICIPANT);
          // Негативный контроль #1: аватар в таблице «Получатели» (40×40) обязан
          // остаться видимым — фикс аватара в плитке не должен его задевать.
          const recipientsAvatars = await probeOutsideAvatars(page);
          check('control: recipients-table avatars stay visible',
            recipientsAvatars.visible > 0,
            `visible=${recipientsAvatars.visible}/${recipientsAvatars.total} `
            + `(recipients=${recipientsAvatars.inRecipientsVisible}, header=${recipientsAvatars.inHeaderVisible})`);
        } else {
          check('layout: contact added to layout recipients', false, `«${PARTICIPANT}» not in recipients popup`);
        }
      }
      await page.getByText('Основное', { exact: true }).first().click();
      await page.waitForTimeout(800);
    } catch (error) {
      check('layout: contact added to layout recipients', false, String(error).slice(0, 200));
      await page.getByText('Основное', { exact: true }).first().click().catch(() => {});
      await page.waitForTimeout(500);
    }

    for (let index = 0; index < SIZES.length; index += 1) {
      const spec = SIZES[index];
      const name = `${spec.size}${spec.plus ? '+' : ''}`;
      const file = slug(spec.label, index + 1);
      console.log(`\n--- layout size ${name} ---`);

      // 1. Выбор размера (кнопка с точным текстом; icon-only «overlap» пропускаем).
      const sizeButton = page.locator('.layout-size-range-selector button')
        .filter({ hasText: new RegExp(`^${spec.label.replace(/[+]/g, '\\+')}$`) }).first();
      const sizeButtonFound = await sizeButton.count();
      check(`size ${name}: range button present`, sizeButtonFound > 0, spec.label);
      if (!sizeButtonFound) {
        sizes.push({ size: spec.size, plus: Boolean(spec.plus), label: spec.label, error: 'size button not found' });
        continue;
      }
      await sizeButton.click({ timeout: 10000 });
      await page.waitForTimeout(900);

      const tiles = await page.locator('.layout-grid-slot-wrapper').count();
      check(`size ${name}: tiles rendered`, spec.plus ? tiles >= spec.size : tiles === spec.size, `tiles=${tiles}`);

      // 2. Посадка участника в ячейку 1 (при смене размера посадка сбрасывается).
      const placement = await placeParticipant(page, 1);
      check(`size ${name}: participant placed in cell 1`, placement.placed, placement.error || PARTICIPANT);

      // 3. Замер с открытой панелью видеслота (строгая геометрия) и в покое (панель закрыта).
      const withPanel = await measureTile(page, 1);
      const outsideWithPanel = await probeOutsideAvatars(page);
      await page.screenshot({ path: path.join(ART, `size-${file}-editor.png`), fullPage: true });
      const panelClosed = await closeSlotPanel(page);
      const resting = await measureTile(page, 1);
      const outsideResting = await probeOutsideAvatars(page);
      await page.screenshot({ path: path.join(ART, `size-${file}.png`), fullPage: true });

      const measurement = resting.labelFound ? resting : withPanel;
      const record = {
        size: spec.size,
        plus: Boolean(spec.plus),
        label: spec.label,
        tiles,
        panelClosed,
        placement: placement.placed ? 'placed' : `failed: ${placement.error}`,
        ...measurement,
        withPanel,
        resting,
        outsideWithPanel,
        outsideResting,
      };
      sizes.push(record);

      if (!measurement.labelFound) {
        check(`size ${name}: participant label found`, false, 'label (.layout-grid-slot-dn__text) not rendered');
        continue;
      }
      check(`size ${name}: participant label found`, true, measurement.labelText);
      check(`size ${name}: label font-size is ${LABEL_FONT_RATIO}x slot font-size (CSS 2em fix)`,
        Math.abs(measurement.labelFontSizePx / measurement.slotFontSizePx - LABEL_FONT_RATIO) <= FONT_RATIO_TOLERANCE,
        `label=${measurement.labelFontSizePx}px slot=${measurement.slotFontSizePx}px ratio=${(measurement.labelFontSizePx / measurement.slotFontSizePx).toFixed(2)}`);
      check(`size ${name}: label centered in tile`,
        Math.abs(measurement.dx) <= CENTER_DX && Math.abs(measurement.dy) <= CENTER_DY,
        `dx=${measurement.dx.toFixed(2)} dy=${measurement.dy.toFixed(2)}`);
      check(`size ${name}: label fully inside tile`, measurement.labelInsideTile,
        `top+=${measurement.labelTopVsTileTop.toFixed(1)} bottom+=${measurement.labelBottomVsTileBottom.toFixed(1)}`);
      check(`size ${name}: label does not stick out horizontally`, measurement.labelInsideTileX,
        `left+=${measurement.labelLeftVsTileLeft.toFixed(1)} right+=${measurement.labelRightVsTileRight.toFixed(1)}`);
      // --- аватар участника в плитке НЕ отображается -----------------------------
      // Принимается любой механизм скрытия: display:none, visibility:hidden,
      // opacity:0, нулевой bbox, обрезка/вынос из плитки, отсутствие в DOM.
      check(`size ${name}: tile avatar hidden`,
        !measurement.avatarVisible,
        measurement.identity.length
          ? measurement.identity.map(item => `${item.cls}[${item.reason || 'VISIBLE'}]`).join('; ')
          : 'no identity element in the tile content (removed from DOM)');
      // Ничто не должно отжимать подпись вниз внутри блока контента: если аватар
      // убран из потока — 0 em; если оставлен (visibility/opacity) — 1.335 em.
      // Проверка применима только к ветке с аватаром: в ветке с превью кадра
      // превью 24×24 px остаётся нарисованным и отжимает подпись по замыслу.
      if (measurement.avatarBlockFound) {
        check(`size ${name}: nothing pushes the label down inside the tile content`,
          Boolean(measurement.labelVsContent) && Math.abs(measurement.labelVsContent.dyEm) <= CONTENT_HEADROOM_EM,
          `labelVsContent.dy=${measurement.labelVsContent ? measurement.labelVsContent.dy : 'n/a'}px `
          + `= ${measurement.labelVsContent ? measurement.labelVsContent.dyEm : 'n/a'}em (limit ${CONTENT_HEADROOM_EM}em)`);
      } else {
        check(`size ${name}: nothing pushes the label down inside the tile content`, true,
          'not applicable: the tile renders a frame preview instead of an avatar', 'info');
      }
      // Фикс не должен задевать аватары ВНЕ плитки (панель видеслота, «Получатели», шапка).
      check(`size ${name}: avatars outside the layout tile stay visible`,
        outsideWithPanel.visible > 0,
        `visible=${outsideWithPanel.visible}/${outsideWithPanel.total} `
        + `(panel=${outsideWithPanel.inSlotPanelVisible}, recipients=${outsideWithPanel.inRecipientsVisible}, header=${outsideWithPanel.inHeaderVisible})`);
      console.log(`     tile=${measurement.tileW.toFixed(1)}x${measurement.tileH.toFixed(1)} ` +
        `label=${measurement.labelW.toFixed(1)}x${measurement.labelH.toFixed(1)} ` +
        `font=${measurement.labelFontSizePx}px/${measurement.slotFontSizePx}px ` +
        `dx=${measurement.dx.toFixed(2)} dy=${measurement.dy.toFixed(2)} ` +
        `inside=${measurement.labelInsideTile} overflowX=${measurement.overflowX} ` +
        `avatar=${measurement.identity.length
          ? measurement.identity.map(item => item.reason || 'VISIBLE').join(',')
          : 'absent'} `
        + `headroom=${measurement.labelVsContent ? measurement.labelVsContent.dyEm : 'n/a'}em`);
    }

    // Сравнение «до/после»: аватары вне плитки не должны измениться при фиксе.
    const outsideFinal = await probeOutsideAvatars(page);
    check('avatars outside the layout tile are unaffected by the fix (before/after)',
      outsideBaseline.visible === outsideFinal.visible
      && outsideBaseline.inSlotPanelVisible === outsideFinal.inSlotPanelVisible
      && outsideBaseline.inHeaderVisible === outsideFinal.inHeaderVisible,
      `before visible=${outsideBaseline.visible}/${outsideBaseline.total} panel=${outsideBaseline.inSlotPanelVisible} header=${outsideBaseline.inHeaderVisible}; `
      + `after visible=${outsideFinal.visible}/${outsideFinal.total} panel=${outsideFinal.inSlotPanelVisible} header=${outsideFinal.inHeaderVisible}`);

    // ------------------------------------------------- пустые плитки и превью
    // (D) Иконка роли в пустой плитке не должна сдвинуться вместе с сеткой.
    emptyTileFinal = await probeEmptyTiles(page);
    const emptyShift = emptyTileBaseline.length && emptyTileFinal.length
      ? +(emptyTileFinal[0].iconVsTileDyEm - emptyTileBaseline[0].iconVsTileDyEm).toFixed(3)
      : null;
    check('empty tile: role icon not shifted by the fix',
      emptyShift !== null && Math.abs(emptyShift) <= 0.05,
      `iconVsTileDyEm before=${emptyTileBaseline.length ? emptyTileBaseline[0].iconVsTileDyEm : 'n/a'} `
      + `after=${emptyTileFinal.length ? emptyTileFinal[0].iconVsTileDyEm : 'n/a'} shift=${emptyShift}em `
      + `contentTransform=${emptyTileFinal.length ? emptyTileFinal[0].contentTransform : 'n/a'}`);
    check('empty tile: role icon stays centered inside the tile content',
      emptyTileFinal.length > 0 && Math.abs(emptyTileFinal[0].iconVsContentDyPx) <= 0.5,
      emptyTileFinal.length
        ? `iconVsContentDy=${emptyTileFinal[0].iconVsContentDyPx}px box=${emptyTileFinal[0].iconBox.w}x${emptyTileFinal[0].iconBox.h}`
        : 'no empty tile rendered');
    // Плитка с ПРЕВЬЮ кадра: вендор рисует .media-preview 24×24 вместо аватара, и
    // тогда блока .layout-grid-slot__avatar в DOM нет — сдвиг должен быть другим.
    previewSim = await simulatePreviewTile(page, 1);
    check('preview tile: label stays centered when the tile shows a frame preview',
      previewSim.simulated && Math.abs(previewSim.dy) <= CENTER_DY && Math.abs(previewSim.dx) <= CENTER_DX,
      previewSim.simulated
        ? `dy=${previewSim.dy} dx=${previewSim.dx} labelVsContentDy=${previewSim.labelVsContentDy} `
          + `transform=${previewSim.contentTransform} hadAvatarElement=${previewSim.hadAvatarElement}`
        : `not simulated: ${previewSim.reason}`);
    if (!previewSim.simulated) {
      check('preview tile: simulation is possible', false, previewSim.reason || 'unknown', 'known-issue');
    }

    // ------------------------------------------ (B) счётчики мониторинга vs API
    const conferencesResponse = await api(token, 'GET', '/api/v3/ui/conferences?page=1&page_size=100');
    const conference = (conferencesResponse.json && conferencesResponse.json.conferences || [])
      .find(item => item.id === CONFERENCE_ID);
    check('counters: conference present in /api/v3/ui/conferences', Boolean(conference),
      `status=${conferencesResponse.status}`);
    const rawContacts = (conference && conference.contacts) || {};
    const apiCounts = {
      participants: Number(rawContacts.users || 0) + Number(rawContacts.h323 || 0) + Number(rawContacts.sip || 0),
      devices: Number(rawContacts.rtsp || 0) + Number(rawContacts.vnc || 0) + Number(rawContacts.media || 0) + Number(rawContacts.file || 0),
      raw: rawContacts,
    };

    const contactsResponse = await api(token, 'GET', `/api/v3/conferences/${CONFERENCE_ID}/contacts`);
    const contactList = (contactsResponse.json && contactsResponse.json.contacts) || [];
    const byType = {};
    const byProtocol = {};
    for (const contact of contactList) {
      const type = contact.contact_type || contact.contactType || 'unknown';
      byType[type] = (byType[type] || 0) + 1;
      const scheme = String(contact.connection_url || contact.call_string_id || '').split(':')[0].toLowerCase();
      byProtocol[scheme] = (byProtocol[scheme] || 0) + 1;
    }
    const contactsEndpoint = {
      count: Number(contactsResponse.json && contactsResponse.json.count !== undefined
        ? contactsResponse.json.count
        : contactList.length),
      byType,
      byProtocol,
      names: contactList.map(contact => contact.display_name),
    };

    // Счётчики в списке мониторинга.
    await page.goto(`${BASE}/pnm/monitoring/conferences`, { waitUntil: 'networkidle' });
    await page.waitForTimeout(1500);
    await page.screenshot({ path: path.join(ART, 'monitoring-list.png'), fullPage: true });
    const conferenceName = (conference && conference.display_name) || '';
    const listRows = await page.locator('.monitoring-list-item').evaluateAll(rows => rows.map(row => {
      const title = row.querySelector('.main-list-item__text--primary');
      const additional = [...row.querySelectorAll('.monitoring-list-item__additional-container')].map(box => ({
        title: box.getAttribute('title') || '',
        texts: [...box.querySelectorAll('span')].map(span => (span.innerText || '').trim()).filter(Boolean),
      }));
      return { name: (title && title.innerText || '').trim(), additional };
    }));
    const row = listRows.find(item => item.name === conferenceName)
      || listRows.find(item => (conferenceName && item.name.includes(conferenceName)));
    const listValue = (titlePart) => {
      const box = row && row.additional.find(item => item.title.includes(titlePart));
      if (!box) return null;
      const numeric = box.texts.filter(text => /^\d+$/.test(text));
      return numeric.length ? Number(numeric[numeric.length - 1]) : null;
    };
    const uiCounts = {
      participants: listValue('Участники'),
      devices: listValue('Устройства'),
    };
    check('counters: conference row found in monitoring list', Boolean(row), conferenceName);

    // Счётчики в шапке карточки комнаты.
    await page.goto(`${BASE}/pnm/monitoring/conferences/${CONFERENCE_ID}`, { waitUntil: 'networkidle' });
    await page.waitForTimeout(1500);
    await page.screenshot({ path: path.join(ART, 'conference-card.png'), fullPage: true });
    const headerBoxes = await page.locator('.monitoring-conference__participants-container').evaluateAll(boxes => boxes.map(box => ({
      title: box.getAttribute('title') || '',
      texts: [...box.querySelectorAll('span')].map(span => (span.innerText || '').trim()).filter(Boolean),
    })));
    const headerValue = (titlePart) => {
      const box = headerBoxes.find(item => item.title.includes(titlePart));
      if (!box) return null;
      const numeric = box.texts.filter(text => /^\d+$/.test(text));
      return numeric.length ? Number(numeric[numeric.length - 1]) : null;
    };
    const headerCounts = {
      participants: headerValue('Участники'),
      devices: headerValue('Устройства'),
    };
    check('counters: conference card header counters found',
      headerCounts.participants !== null && headerCounts.devices !== null, JSON.stringify(headerBoxes));

    const uiVsApi = uiCounts.participants === apiCounts.participants && uiCounts.devices === apiCounts.devices;
    const uiVsHeader = uiCounts.participants === headerCounts.participants && uiCounts.devices === headerCounts.devices;
    const uiVsContacts = uiCounts.participants === contactsEndpoint.count;
    counters = {
      uiCounts,
      apiCounts,
      contactsEndpoint,
      headerCounts,
      match: uiVsApi && uiVsHeader && uiVsContacts,
      matchDefinition: 'uiCounts == apiCounts == headerCounts && uiCounts.participants == contactsEndpoint.count',
      matchDetail: { uiVsApi, uiVsHeader, uiVsContacts },
    };
    check('counters: UI list == API /ui/conferences', uiVsApi,
      `ui=${JSON.stringify(uiCounts)} api=${JSON.stringify({ participants: apiCounts.participants, devices: apiCounts.devices })}`);
    check('counters: UI list == conference card header', uiVsHeader,
      `ui=${JSON.stringify(uiCounts)} header=${JSON.stringify(headerCounts)}`);
    check('counters: UI participants == contacts endpoint count', uiVsContacts,
      `ui=${uiCounts.participants} contacts=${contactsEndpoint.count} (${JSON.stringify(contactsEndpoint.names)})`);

    check('ui: no pageerror', pageErrors.length === 0, JSON.stringify(pageErrors).slice(0, 500));
    const writes = nonGetRequests.filter(request => !request.url.includes('/video_layouts/calculate')
      && !request.url.includes('/api/v3/login'));
    check('db: no writes to the live stand (draft only)', writes.length === 0, JSON.stringify(writes).slice(0, 500));
  } catch (error) {
    failure = error;
    console.error(error && error.stack || error);
  } finally {
    if (browser) await browser.close().catch(() => {});
    // Режим самопроверки харнесса: при откате CSS-фикса прогон ОБЯЗАН упасть
    // на проверке «tile avatar hidden». Если он не упал — харнесс не ловит откат
    // и его нельзя считать рабочим (это отдельная fatal-проверка).
    let revertCheck = null;
    if (REVERT_CHECK) {
      const avatarChecks = checks.filter(entry => entry.name.endsWith(': tile avatar hidden'));
      const detected = avatarChecks.length > 0 && avatarChecks.some(entry => !entry.ok);
      revertCheck = {
        detected,
        checked: avatarChecks.length,
        failed: avatarChecks.filter(entry => !entry.ok).map(entry => entry.name),
      };
      check('revert check: rollback of the avatar fix is detected', detected,
        `checked=${avatarChecks.length} failed=${revertCheck.failed.length}`, 'fatal');
      console.log(`\n[REVERT_CHECK] откат CSS-фикса зафиксирован: ${detected ? 'ДА' : 'НЕТ — харнесс не ловит откат!'}`);
    }
    // Код выхода: 0 — все fatal-проверки прошли; 1 — есть проваленная
    // fatal-проверка; 2 — прогон упал целиком (логин/страница).
    const fatalChecks = checks.filter(entry => entry.severity === 'fatal');
    const failedFatal = fatalChecks.filter(entry => !entry.ok);
    const labelChecks = checks.filter(entry => entry.name.startsWith('size ') || entry.name.startsWith('layout:'));
    const counterChecks = checks.filter(entry => entry.name.startsWith('counters:'));
    const labelOk = labelChecks.length > 0 && labelChecks.every(entry => entry.ok);
    const counterOk = counterChecks.every(entry => entry.ok);
    const contactsOk = counterChecks
      .filter(entry => entry.name === 'counters: UI participants == contacts endpoint count')
      .every(entry => entry.ok);
    const ok = !failure && failedFatal.length === 0;
    const result = {
      ok,
      base: BASE,
      conference_id: CONFERENCE_ID,
      participant: PARTICIPANT,
      duration_ms: Date.now() - startedAt,
      criteria: {
        centerDx: CENTER_DX,
        centerDy: CENTER_DY,
        labelFontRatio: LABEL_FONT_RATIO,
        fontRatioTolerance: FONT_RATIO_TOLERANCE,
        contentHeadroomEm: CONTENT_HEADROOM_EM,
        strictCounts: STRICT_COUNTS,
      },
      modes: { revertCheck: REVERT_CHECK, applyCandidate: APPLY_CANDIDATE },
      summary: {
        sizesMeasured: sizes.length,
        sizesWithLabel: sizes.filter(item => item.labelFound).length,
        labelChecksOk: labelOk,
        counterChecksOk: counterOk,
        countersMatchContactsEndpoint: contactsOk,
        avatarHiddenEverywhere: sizes.length > 0 && sizes.every(item => item.anyIdentityVisible === false),
        failedFatalChecks: failedFatal.map(entry => entry.name),
        knownIssues: checks.filter(entry => entry.severity === 'known-issue' && !entry.ok).map(entry => entry.name),
        fontRatios: sizes.map(item => ({
          size: `${item.size}${item.plus ? '+' : ''}`,
          slotFontSizePx: item.slotFontSizePx,
          labelFontSizePx: item.labelFontSizePx,
          ratio: item.slotFontSizePx ? Number((item.labelFontSizePx / item.slotFontSizePx).toFixed(3)) : null,
        })),
      },
      sizes,
      counters,
      emptyTile: { baseline: emptyTileBaseline, final: emptyTileFinal },
      outsideAvatars: { baseline: outsideBaseline },
      previewSimulation: previewSim,
      revertCheck,
      checks,
      page_errors: pageErrors,
      console_errors: consoleErrors,
      non_get_requests: nonGetRequests,
      failure: failure ? String((failure && failure.stack) || failure) : null,
    };
    fs.writeFileSync(path.join(ART, 'result.json'), JSON.stringify(result, null, 2));
    console.log('\n===== SUMMARY =====');
    console.log(JSON.stringify({
      ok: result.ok,
      sizesMeasured: result.summary.sizesMeasured,
      sizesWithLabel: result.summary.sizesWithLabel,
      labelChecksOk: result.summary.labelChecksOk,
      counterChecksOk: result.summary.counterChecksOk,
      countersMatchContactsEndpoint: result.summary.countersMatchContactsEndpoint,
      failedFatalChecks: result.summary.failedFatalChecks,
      knownIssues: result.summary.knownIssues,
      uiCounts: counters && counters.uiCounts,
      apiCounts: counters && { participants: counters.apiCounts.participants, devices: counters.apiCounts.devices },
      headerCounts: counters && counters.headerCounts,
      contactsEndpoint: counters && { count: counters.contactsEndpoint.count, byType: counters.contactsEndpoint.byType },
    }, null, 2));
    console.log(`artifacts: ${ART}`);
    process.exit(ok ? 0 : (failure ? 2 : 1));
  }
})();

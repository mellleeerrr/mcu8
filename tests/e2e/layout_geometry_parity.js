#!/usr/bin/env node
/*
 * T2: единый источник геометрии. Клиент (`codian-layouts.js`) больше не считает
 * слоты локально — он запрашивает `POST /api/v3/ui/video_layouts/calculate`
 * (vks-layout/src/grids.rs) и берёт оттуда; этот скрипт проверяет серверный
 * контракт для всех режимов конструктора и отсутствие локального движка.
 *
 * Запуск: node tests/e2e/layout_geometry_parity.js [--base URL]
 * Требует живой стенд (по умолчанию http://127.0.0.1:8080) и dev-креды
 * (VKS_LOGIN/VKS_PASSWORD, по умолчанию admin/admin_password).
 * Коды возврата: 0 — сервер отвечает корректной геометрией для всех кейсов,
 * 1 — расхождение/ошибка (в т.ч. найден локальный движок в клиенте).
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');

const BASE = (process.env.VKS_BASE_URL || 'http://127.0.0.1:8080').replace(/\/$/, '');
const LOGIN = process.env.VKS_LOGIN || 'admin';
const PASSWORD = process.env.VKS_PASSWORD || 'admin_password';
const CLIENT = path.resolve(__dirname, '../../client/pnm-frontend/deployed/pnm/codian-layouts.js');
const CANVAS_W = 1280;
const CANVAS_H = 720;

const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok: Boolean(ok), detail: detail === undefined ? '' : String(detail) });
  console.log(`[parity] ${ok ? 'OK  ' : 'FAIL'} ${name}${detail ? ' — ' + detail : ''}`);
  return Boolean(ok);
}

/** Режимы конструктора: те же id, что CONSTRUCTOR_MODES в клиенте. */
const MODES = [
  ['grid', (n) => `vks_grid_${n}`],
  ['pin_l', (n) => `vks_pin_l_${n}`],
  ['pin_r', (n) => `vks_pin_r_${n}`],
  ['pin_t', (n) => `vks_pin_t_${n}`],
  ['pin_b', (n) => `vks_pin_b_${n}`],
  ['dual', (n) => `vks_dual_${n}`],
];
const COUNTS = [1, 2, 3, 7, 13, 25, 43, 50, 99, 100];

function validateSlots(slots, expected, label) {
  if (!Array.isArray(slots) || slots.length !== expected) {
    return check(`${label}: ровно ${expected} слотов`, false, `получено ${Array.isArray(slots) ? slots.length : typeof slots}`);
  }
  let ok = true;
  for (const slot of slots) {
    const nums = ['x', 'y', 'width', 'height'].map((key) => Number(slot[key]));
    const finite = nums.every((value) => Number.isFinite(value));
    const inside = finite
      && slot.x >= 0 && slot.y >= 0 && slot.width > 0 && slot.height > 0
      && slot.x + slot.width <= CANVAS_W + 0.001 && slot.y + slot.height <= CANVAS_H + 0.001;
    if (!inside) {
      ok = check(`${label}: слот ${slot.position} внутри канваса`, false, JSON.stringify(slot));
      break;
    }
  }
  if (ok) check(`${label}: ${expected} валидных слотов в 1280x720`, true);
  return ok;
}

async function main() {
  // 1. Статический guard: локального движка в клиенте быть не должно.
  const source = fs.readFileSync(CLIENT, 'utf8');
  for (const name of ['buildSymmetricLayout', 'computeSlots', 'formatCalculatedSlots', 'synthXhr']) {
    check(`клиент: нет локального «${name}»`, !new RegExp(`\\b${name}\\b`).test(source));
  }
  check('клиент: calculate идёт на сервер', /video_layouts\/calculate/.test(source));

  // 2. Логин и серверный контракт.
  const login = await fetch(`${BASE}/api/v3/login`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ login: LOGIN, password: PASSWORD }),
  });
  const loginDoc = await login.json().catch(() => ({}));
  const token = loginDoc.token || loginDoc.access_token || '';
  check('login', login.ok && Boolean(token), `${login.status}`);
  if (!token) return 1;

  let failures = 0;
  for (const [mode, id] of MODES) {
    for (const count of COUNTS) {
      const preset = id(count);
      const response = await fetch(`${BASE}/api/v3/ui/video_layouts/calculate`, {
        method: 'POST',
        headers: { 'content-type': 'application/json', authorization: `Bearer ${token}` },
        body: JSON.stringify({ preset, width: CANVAS_W, height: CANVAS_H }),
      });
      const label = `${mode}/${count}`;
      if (!response.ok) {
        failures += 1;
        check(`${label}: calculate 200`, false, `${response.status} ${preset}`);
        continue;
      }
      const doc = await response.json();
      if (!validateSlots(doc.slots, count, label)) failures += 1;
    }
  }

  // 3. Option-пресеты (`g`/`m`) — бывший крэш `vks_grid_g12_m24_25`.
  for (const preset of ['vks_grid_g12_m24_25', 'vks_pin_l_g10_m20_8', 'vks_dual_g8_m16_14']) {
    const response = await fetch(`${BASE}/api/v3/ui/video_layouts/calculate`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', authorization: `Bearer ${token}` },
      body: JSON.stringify({ preset, width: CANVAS_W, height: CANVAS_H }),
    });
    const doc = await response.json().catch(() => ({}));
    const count = Number(/_(\d+)$/.exec(preset)[1]);
    if (!response.ok) {
      failures += 1;
      check(`option ${preset}: calculate 200`, false, `${response.status}`);
    } else if (!validateSlots(doc.slots, count, `option ${preset}`)) {
      failures += 1;
    }
  }

  check('итог: все серверные кейсы валидны', failures === 0, `failures=${failures}`);
  return failures === 0 ? 0 : 1;
}

main()
  .then((code) => process.exit(code))
  .catch((error) => {
    console.error('[parity] ERROR', error);
    process.exit(1);
  });

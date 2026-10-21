#!/usr/bin/env node
// T3.1 acceptance evidence: мёртвый код/зависимости.
// 1) устаревшие alias-маршруты /records удалены (501 вместо фейкового 200),
//    актуальные /videorecords работают;
// 2) неиспользуемые прямые зависимости удалены из Cargo.toml (vks-layout у
//    vks-server, vks-core у шлюзов);
// 3) известный дефект контракта `video_layouts/mode` починен: интеграционный
//    тест пакета D проходит (создаём и привязываем раскладку, как UI);
// 4) workspace собирается, clippy/fmt чисты (проверяется вызывающей стороной,
//    здесь фиксируем git/worktree provenance).

const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');

const OUT = process.env.EVIDENCE_DIR || 'docs/progress/evidence/t31';
const BASE = (process.env.BASE_URL || 'http://127.0.0.1:8080').replace(/\/$/, '');
fs.mkdirSync(OUT, { recursive: true });

const sh = (cmd) => { try { return execSync(cmd, { encoding: 'utf8' }).trim(); } catch (e) { return String(e.stdout || '') + String(e.stderr || ''); } };
const run = (cmd) => { try { const out = execSync(cmd, { encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'] }); return { code: 0, out }; } catch (e) { return { code: e.status || 1, out: String(e.stdout || '') + String(e.stderr || '') }; } };

const build = {
  git_head: sh('git rev-parse HEAD'),
  git_head_ct: Number(sh('git log -1 --format=%ct HEAD')) || 0,
  git_dirty: sh('git status --porcelain') !== '',
  binary_sha256: null,
  binary_mtime: null,
  server_pid: null,
};
for (const pid of sh('pgrep -f "target/release/vks-server"').split('\n').filter(Boolean)) {
  try {
    const exe = `/proc/${pid}/exe`;
    build.binary_sha256 = crypto.createHash('sha256').update(fs.readFileSync(exe)).digest('hex');
    build.binary_mtime = Math.floor(fs.statSync(exe).mtimeMs / 1000);
    build.server_pid = Number(pid);
    break;
  } catch { /* следующий */ }
}

const summary = { task: 'T3.1', ok: false, started_at: new Date().toISOString(), build, acceptance: {}, totals: {}, error: null };

(async () => {
  try {
    // Login и API-проверки.
    const token = (await (await fetch(BASE + '/api/v3/login', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ login: 'admin', password: 'admin_password' }),
    })).json()).token;
    if (!token) throw new Error('no token');
    const api = async (pathname, method = 'GET') => {
      const response = await fetch(BASE + pathname, { method, headers: { Authorization: `Bearer ${token}` } });
      return { status: response.status, text: await response.text() };
    };

    const records = await api('/api/v3/records');
    const uiRecords = await api('/api/v3/ui/records');
    const videorecords = await api('/api/v3/videorecords');
    summary.acceptance.legacy_records_removed = records.status === 501 && uiRecords.status === 501;
    summary.acceptance.videorecords_works = videorecords.status === 200 && JSON.parse(videorecords.text).count === 0;
    summary.totals.legacy_statuses = { records: records.status, ui_records: uiRecords.status, videorecords: videorecords.status };

    // Статические проверки Cargo.toml: прямые неиспользуемые зависимости удалены.
    const serverToml = fs.readFileSync('crates/vks-server/Cargo.toml', 'utf8');
    const sipToml = fs.readFileSync('crates/vks-gateway-sip/Cargo.toml', 'utf8');
    const h323Toml = fs.readFileSync('crates/vks-gateway-h323/Cargo.toml', 'utf8');
    summary.acceptance.unused_deps_removed =
      !/^vks-layout\s*=/m.test(serverToml) && !/^vks-core\s*=/m.test(sipToml) && !/^vks-core\s*=/m.test(h323Toml);
    summary.acceptance.no_dead_records_handler = !fs.readFileSync('crates/vks-admin/src/api_v3/shell.rs', 'utf8').includes('ui_records');

    // Контракт video_layouts/mode: известный дефект закрыт интеграционным тестом.
    const integration = run('cargo test -p vks-server --test network_integration test_package_d_mcu_pipeline_handover_and_pnm_control_events 2>&1');
    summary.acceptance.layout_mode_contract_test = integration.code === 0 && /1 passed/.test(integration.out);
    const adminTests = run('cargo test -p vks-admin --lib 2>&1');
    const adminMatch = adminTests.out.match(/(\d+) passed/);
    summary.acceptance.admin_tests_green = adminTests.code === 0 && Number(adminMatch && adminMatch[1]) >= 220;
    const workspaceCheck = run('cargo check --workspace --all-targets 2>&1');
    summary.acceptance.workspace_checks = workspaceCheck.code === 0;
    summary.totals.tests = { admin_lib: Number(adminMatch && adminMatch[1]) || null, integration: integration.code === 0 };
    summary.totals.integration_tail = integration.out.trim().split('\n').slice(-3);
  } catch (error) {
    summary.error = String((error && error.stack) || error);
  } finally {
    const failed = Object.entries(summary.acceptance).filter(([, ok]) => ok !== true).map(([key]) => key);
    summary.acceptance_failed = failed;
    summary.ok = build.git_dirty === false && build.binary_mtime >= build.git_head_ct && failed.length === 0;
    summary.finished_at = new Date().toISOString();
    fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(summary, null, 1) + '\n');
    console.log(`T3.1 evidence: ${summary.ok ? 'OK' : 'FAIL'} ${Object.keys(summary.acceptance).length - failed.length}/${Object.keys(summary.acceptance).length} gates`);
    if (failed.length) console.log('failed:', failed.join(', '));
    if (summary.error) console.log(summary.error);
    process.exit(summary.ok ? 0 : 1);
  }
})();

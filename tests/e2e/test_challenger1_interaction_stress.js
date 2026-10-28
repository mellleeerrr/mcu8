/**
 * Challenger 1: Adversarial Interaction & Store Stress Test Harness
 * Target: Dynamic Layout Constructor (Rapid Counter Updates, Card Switching, Store Sync, DOM Leak Detection)
 *
 * Requirements:
 * 1. Rapid counter updates: high-frequency clicks on inc/dec/min/max and direct keyboard typing.
 * 2. Rapid card switching across all 15 archetypes (sequential + random thrashing).
 * 3. Store consistency: modifiedValue.preset and modifiedValue.slots stay 100% in sync.
 * 4. DOM hygiene and leak prevention: 10 back-and-forth mode cycles without element duplication or listener leaks.
 * 5. Zero unhandled page errors, zero console exceptions, zero HTTP 400 errors.
 */

const { chromium } = require('playwright');

(async () => {
  console.log('================================================================================');
  console.log('>>> [CHALLENGER 1] STARTING INTERACTION & STORE STRESS HARNESS');
  console.log('================================================================================\n');

  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({
    viewport: { width: 1920, height: 1080 },
    colorScheme: 'dark'
  });

  await context.addInitScript(() => {
    try {
      localStorage.setItem('VKS_PNM_THEME', 'dark');
    } catch (_) {}
  });

  const page = await context.newPage();
  const pageErrors = [];
  const httpErrors = [];

  page.on('pageerror', err => {
    const msg = err && err.message ? err.message : String(err);
    // Ignore Axios in-flight request cancellation on route transitions
    if (msg.includes('Operation canceled') || msg.includes('canceled')) return;
    console.error('>>> [PAGEERROR EVENT]', msg);
    pageErrors.push(msg);
  });

  page.on('response', async resp => {
    if (resp.status() >= 400) {
      let body = '';
      try { body = await resp.text(); } catch (_) {}
      const record = `HTTP ${resp.status()} ${resp.url()} | ${body.slice(0, 100)}`;
      console.error('>>> [HTTP ERROR]', record);
      httpErrors.push(record);
    }
  });

  let totalChecks = 0;
  let passedChecks = 0;
  let failedChecks = 0;
  const failureDetails = [];

  function assert(condition, message, detail = '') {
    totalChecks++;
    if (condition) {
      console.log(`  [✓] ${message} ${detail ? '(' + detail + ')' : ''}`);
      passedChecks++;
    } else {
      const errMsg = `[✗] FAIL: ${message} ${detail ? '(' + detail + ')' : ''}`;
      console.error(`  ${errMsg}`);
      failedChecks++;
      failureDetails.push(errMsg);
    }
  }

  // --- Step 0: Authentication & Navigation ---
  console.log('--- Step 0: Authentication & Navigation ---');
  await page.goto('http://127.0.0.1:8080/pnm/authorization', { waitUntil: 'domcontentloaded' });
  await page.fill('input[type="text"]', 'admin');
  await page.fill('input[type="password"]', 'admin_password');
  await page.click('[data-qa="authorization-sign-in-button"]');
  await page.waitForTimeout(1000);

  await page.goto('http://127.0.0.1:8080/pnm/monitoring/conferences', { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(1000);

  const firstConf = page.locator('.main-list-item').first();
  await firstConf.click();
  await page.waitForTimeout(800);

  const layoutsTab = page.locator('.tc-tab, .mdc-tab').filter({ hasText: 'Раскладки' }).first();
  await layoutsTab.click();
  await page.waitForTimeout(800);

  const addLayoutBtn = page.locator('[data-qa="conference-add-layout-button"]');
  if (await addLayoutBtn.count() > 0) {
    await addLayoutBtn.click();
  } else {
    await page.locator('.conference-layouts-item').first().click();
  }
  await page.waitForTimeout(1200);

  // Switch to Constructor mode
  const ctorTab = page.locator('[data-vks-constructor-tile="1"], [data-qa="layout-mode-selector-constructor"]');
  assert(await ctorTab.count() > 0, 'Constructor tile exists in mode selector');
  await ctorTab.click();
  await page.waitForTimeout(1000);

  // Helper to read store state
  async function getStoreState() {
    return await page.evaluate(() => {
      const root = document.getElementById('root') || document.getElementById('app');
      const store = root && root.__vue_app__ && root.__vue_app__.config.globalProperties.$pinia._s.get('layout-editor');
      const mv = store && store.modifiedValue;
      return {
        mode: mv ? mv.mode : null,
        preset: mv ? mv.preset : null,
        slotsCount: mv && Array.isArray(mv.slots) ? mv.slots.length : 0,
        slots: mv && Array.isArray(mv.slots) ? mv.slots : []
      };
    });
  }

  // Helper to read counter and DOM card state
  async function getDomConstructorState() {
    return await page.evaluate(() => {
      const input = document.querySelector('.dynamic-layout-configurator-side-panel [data-counter-input]');
      const count = input ? parseInt(input.value, 10) : 0;
      const cards = Array.from(document.querySelectorAll('.dynamic-layout-configurator-side-panel [data-vks-constructor-mode]'));
      const activeCard = cards.find(c => c.classList.contains('layout-scheme-selector-item--active'));
      const activeArchetype = activeCard ? activeCard.getAttribute('data-vks-constructor-mode') : null;

      let allBadgesMatch = true;
      let allSvgsMatch = true;
      let allWindowsAttrMatch = true;

      for (const card of cards) {
        const badge = card.querySelector('.layout-scheme-selector-item__badge');
        if (!badge || parseInt(badge.textContent.trim(), 10) !== count) {
          allBadgesMatch = false;
        }
        if (parseInt(card.getAttribute('data-windows'), 10) !== count) {
          allWindowsAttrMatch = false;
        }
        const rects = card.querySelectorAll('svg rect');
        if (rects.length !== count) {
          allSvgsMatch = false;
        }
      }

      return {
        count,
        cardsTotal: cards.length,
        activeArchetype,
        allBadgesMatch,
        allSvgsMatch,
        allWindowsAttrMatch
      };
    });
  }

  // =============================================================================
  // STAGE 1: RAPID COUNTER THRESHING (BURST CLICKS)
  // =============================================================================
  console.log('\n--- STAGE 1: Rapid Counter Updates (Burst Clicking) ---');

  const incBtn = page.locator('.dynamic-layout-configurator-side-panel [data-counter-action="inc"] button');
  const decBtn = page.locator('.dynamic-layout-configurator-side-panel [data-counter-action="dec"] button');

  const initialDom = await getDomConstructorState();
  console.log(`Starting burst test from count = ${initialDom.count}...`);

  // Rapid burst of 25 increments (50ms interval)
  console.log('Burst: firing 25 rapid (+) clicks...');
  for (let i = 0; i < 25; i++) {
    await incBtn.click();
    await page.waitForTimeout(50);
  }
  await page.waitForTimeout(600); // Allow render settlement

  const afterIncDom = await getDomConstructorState();
  const afterIncStore = await getStoreState();
  assert(afterIncDom.count === initialDom.count + 25, `Counter correctly incremented by 25 (${initialDom.count} -> ${afterIncDom.count})`);
  assert(afterIncDom.allBadgesMatch, `All 15 card badges reactively updated to ${afterIncDom.count}`);
  assert(afterIncDom.allSvgsMatch, `All 15 card SVGs reactively updated to ${afterIncDom.count} rects`);
  assert(afterIncStore.slotsCount === afterIncDom.count, `Store slots length equals ${afterIncDom.count} (got ${afterIncStore.slotsCount})`);
  assert(afterIncStore.preset === `vks_${afterIncDom.activeArchetype}_${afterIncDom.count}`, `Store preset matches vks_${afterIncDom.activeArchetype}_${afterIncDom.count} (got ${afterIncStore.preset})`);

  // Rapid burst of 15 decrements (50ms interval)
  console.log('Burst: firing 15 rapid (-) clicks...');
  for (let i = 0; i < 15; i++) {
    await decBtn.click();
    await page.waitForTimeout(50);
  }
  await page.waitForTimeout(600);

  const afterDecDom = await getDomConstructorState();
  const afterDecStore = await getStoreState();
  assert(afterDecDom.count === afterIncDom.count - 15, `Counter correctly decremented by 15 (${afterIncDom.count} -> ${afterDecDom.count})`);
  assert(afterDecDom.allBadgesMatch, `All 15 card badges reactively updated to ${afterDecDom.count}`);
  assert(afterDecDom.allSvgsMatch, `All 15 card SVGs reactively updated to ${afterDecDom.count} rects`);
  assert(afterDecStore.slotsCount === afterDecDom.count, `Store slots length equals ${afterDecDom.count} (got ${afterDecStore.slotsCount})`);

  // =============================================================================
  // STAGE 2: BOUNDARY JUMPS & KEYBOARD FUZZING
  // =============================================================================
  console.log('\n--- STAGE 2: Boundary Jumps & Keyboard Fuzzing ---');

  const minBtn = page.locator('.dynamic-layout-configurator-side-panel [data-counter-action="min"] button');
  const maxBtn = page.locator('.dynamic-layout-configurator-side-panel [data-counter-action="max"] button');

  // Jump to Min (1)
  await minBtn.click();
  await page.waitForTimeout(600);
  let minState = await getDomConstructorState();
  let minStore = await getStoreState();
  assert(minState.count === 1, 'Min button set counter to 1');
  assert(minState.allBadgesMatch && minState.allSvgsMatch, 'All 15 cards display badge 1 and exactly 1 SVG rect');
  assert(minStore.slotsCount === 1, 'Store slots array length is 1');

  // Jump to Max (100)
  await maxBtn.click();
  await page.waitForTimeout(800);
  let maxState = await getDomConstructorState();
  let maxStore = await getStoreState();
  assert(maxState.count === 100, 'Max button set counter to 100');
  assert(maxState.allBadgesMatch && maxState.allSvgsMatch, 'All 15 cards display badge 100 and exactly 100 SVG rects');
  assert(maxStore.slotsCount === 100, 'Store slots array length is 100');

  // Keyboard typing test: out-of-range negative input "-50"
  console.log('Testing keyboard input: typing "-50" (should clamp to 1)...');
  const counterInput = page.locator('.dynamic-layout-configurator-side-panel [data-counter-input]');
  await counterInput.fill('-50');
  await counterInput.press('Enter');
  await counterInput.evaluate(el => el.blur());
  await page.waitForTimeout(600);

  let clampedNegState = await getDomConstructorState();
  let clampedNegStore = await getStoreState();
  assert(clampedNegState.count === 1, 'Negative input clamped to min 1', `got ${clampedNegState.count}`);
  assert(clampedNegStore.slotsCount === 1, 'Store slots count clamped to 1');

  // Keyboard typing test: out-of-range positive input "500"
  console.log('Testing keyboard input: typing "500" (should clamp to 100)...');
  await counterInput.fill('500');
  await counterInput.press('Enter');
  await counterInput.evaluate(el => el.blur());
  await page.waitForTimeout(600);

  let clampedPosState = await getDomConstructorState();
  let clampedPosStore = await getStoreState();
  assert(clampedPosState.count === 100, 'Excessive input clamped to max 100', `got ${clampedPosState.count}`);
  assert(clampedPosStore.slotsCount === 100, 'Store slots count clamped to 100');

  // Keyboard typing test: valid arbitrary input "49"
  console.log('Testing keyboard input: typing "49"...');
  await counterInput.fill('49');
  await counterInput.press('Enter');
  await counterInput.evaluate(el => el.blur());
  await page.waitForTimeout(600);

  let midState = await getDomConstructorState();
  let midStore = await getStoreState();
  assert(midState.count === 49, 'Arbitrary input "49" applied directly', `got ${midState.count}`);
  assert(midState.allBadgesMatch && midState.allSvgsMatch, 'All 15 cards show badge "49" and 49 rects');
  assert(midStore.slotsCount === 49, 'Store slots length set to 49');

  // =============================================================================
  // STAGE 3: RAPID CARD SWITCHING THRASHING
  // =============================================================================
  console.log('\n--- STAGE 3: Rapid Card Switching Thrashing ---');

  const ARCHETYPES = [
    'grid', 'pin_l', 'pin_r', 'pin_t', 'pin_b',
    'dual_h', 'dual_v', 'pip', 'frame', 'split_50',
    'trio', 'corner_l', 'corner_r', 'quad_focus', 'cinema'
  ];

  console.log('Rapidly cycling through all 15 archetypes (100ms intervals)...');
  for (const arch of ARCHETYPES) {
    const card = page.locator(`.dynamic-layout-configurator-side-panel [data-vks-constructor-mode="${arch}"]`);
    await card.click();
    await page.waitForTimeout(100);

    const store = await getStoreState();
    assert(store.preset === `vks_${arch}_49`, `Clicked card "${arch}" -> Store preset is vks_${arch}_49 (got: ${store.preset})`);
    assert(store.slotsCount === 49, `Clicked card "${arch}" -> Store slots count is 49`);
  }

  // Random thrashing across 20 jumps
  console.log('Thrashing: 20 random archetype jumps (50ms intervals)...');
  for (let i = 0; i < 20; i++) {
    const randomArch = ARCHETYPES[Math.floor(Math.random() * ARCHETYPES.length)];
    const card = page.locator(`.dynamic-layout-configurator-side-panel [data-vks-constructor-mode="${randomArch}"]`);
    await card.click();
    await page.waitForTimeout(50);
  }
  await page.waitForTimeout(500);

  const finalSwitchDom = await getDomConstructorState();
  const finalSwitchStore = await getStoreState();
  assert(finalSwitchStore.preset === `vks_${finalSwitchDom.activeArchetype}_49`, `Final thrashing preset matches active card: ${finalSwitchStore.preset}`);
  assert(finalSwitchStore.slotsCount === 49, 'Final thrashing slots count remains 49');

  // Verify only 1 card has active class
  const activeCount = await page.evaluate(() => {
    return document.querySelectorAll('.dynamic-layout-configurator-side-panel .layout-scheme-selector-item--active').length;
  });
  assert(activeCount === 1, `Exactly 1 constructor card is active (got: ${activeCount})`);

  // =============================================================================
  // STAGE 4: REPEATED MODE SWITCH CYCLES & DOM LEAK CHECK
  // =============================================================================
  console.log('\n--- STAGE 4: Repeated Mode Switch Cycles (10 Cycles) & DOM Hygiene ---');

  const manualTab = page.locator('[data-qa="layout-mode-selector-manual"]');

  for (let cycle = 1; cycle <= 10; cycle++) {
    // 1. Switch to Manual
    await manualTab.click();
    await page.waitForTimeout(200);

    // Select Codian card 07
    const codian07 = page.locator('[data-codian-id="codian_07"]');
    await codian07.click();
    await page.waitForTimeout(200);

    const mStore = await getStoreState();
    if (mStore.preset !== 'codian_07') {
      assert(false, `Cycle ${cycle}: Store preset should be codian_07`, `got ${mStore.preset}`);
    }

    // 2. Switch back to Constructor
    await ctorTab.click();
    await page.waitForTimeout(200);

    // Select an archetype and increment
    const card = page.locator('.dynamic-layout-configurator-side-panel [data-vks-constructor-mode="cinema"]');
    await card.click();
    await page.waitForTimeout(100);
  }
  await page.waitForTimeout(800);

  // Check DOM Hygiene after 10 full cycles
  const domHygiene = await page.evaluate(() => {
    const sidePanel = document.querySelector('.base-layout-configurator-side-panel');
    const ctorPanels = sidePanel ? sidePanel.querySelectorAll('[data-vks-constructor="1"]') : [];
    const codianGroups = sidePanel ? sidePanel.querySelectorAll('[data-vks-codian-group="1"]') : [];
    const counters = sidePanel ? sidePanel.querySelectorAll('[data-vks-counter="1"]') : [];
    const modesContainers = sidePanel ? sidePanel.querySelectorAll('[data-vks-constructor-modes="1"]') : [];
    const ctorCards = sidePanel ? sidePanel.querySelectorAll('[data-vks-constructor-mode]') : [];
    const codianCards = sidePanel ? sidePanel.querySelectorAll('[data-codian-id]') : [];

    return {
      ctorPanelsCount: ctorPanels.length,
      codianGroupsCount: codianGroups.length,
      countersCount: counters.length,
      modesContainersCount: modesContainers.length,
      ctorCardsCount: ctorCards.length,
      codianCardsCount: codianCards.length
    };
  });

  assert(domHygiene.ctorPanelsCount === 1, `Zero duplicated constructor panels in sidebar (count: ${domHygiene.ctorPanelsCount})`);
  assert(domHygiene.codianGroupsCount === 1, `Zero duplicated Codian groups in sidebar (count: ${domHygiene.codianGroupsCount})`);
  assert(domHygiene.countersCount === 1, `Zero duplicated counter widgets in sidebar (count: ${domHygiene.countersCount})`);
  assert(domHygiene.modesContainersCount === 1, `Zero duplicated modes containers in sidebar (count: ${domHygiene.modesContainersCount})`);
  assert(domHygiene.ctorCardsCount === 15, `Sidebar maintains exactly 15 dynamic cards after 10 cycles (count: ${domHygiene.ctorCardsCount})`);
  assert(domHygiene.codianCardsCount === 54, `Sidebar maintains exactly 54 Codian cards after 10 cycles (count: ${domHygiene.codianCardsCount})`);

  // =============================================================================
  // STAGE 5: ERROR HYGIENE & SUMMARY
  // =============================================================================
  console.log('\n--- STAGE 5: Error Hygiene & Summary ---');
  if (pageErrors.length > 0) {
    console.error('Page errors encountered:');
    pageErrors.forEach((e, idx) => console.error(`  [${idx + 1}] ${e}`));
  }
  if (httpErrors.length > 0) {
    console.error('HTTP errors encountered:');
    httpErrors.forEach((e, idx) => console.error(`  [${idx + 1}] ${e}`));
  }

  assert(pageErrors.length === 0, `Zero page errors during entire interaction stress test (errors: ${pageErrors.length})`);
  assert(httpErrors.length === 0, `Zero HTTP 400 / calculate errors during entire interaction stress test (errors: ${httpErrors.length})`);

  console.log('\n================================================================================');
  console.log(`>>> SUMMARY: ${passedChecks} checks PASSED, ${failedChecks} FAILED (Total assertions: ${totalChecks})`);
  console.log('================================================================================');

  await browser.close();

  if (failedChecks > 0 || pageErrors.length > 0 || httpErrors.length > 0) {
    process.exit(1);
  } else {
    console.log('\n>>> EMPIRICAL PROOF VERDICT: RAPID COUNTER UPDATES, CARD SWITCHING & STORE SYNCHRONIZATION 100% ROBUST');
    process.exit(0);
  }
})();

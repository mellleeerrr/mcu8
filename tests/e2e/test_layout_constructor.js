const { chromium } = require('playwright');

(async () => {
  console.log('>>> [Layout Constructor E2E] Starting comprehensive verification...');

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
  page.on('pageerror', err => pageErrors.push(err.message));

  let passed = 0;
  let failed = 0;

  function assert(condition, message, detail = '') {
    if (condition) {
      console.log(`  [✓] ${message} ${detail ? '(' + detail + ')' : ''}`);
      passed++;
    } else {
      console.error(`  [✗] FAIL: ${message} ${detail ? '(' + detail + ')' : ''}`);
      failed++;
    }
  }

  // 1. Authorization
  console.log('Navigating to authorization...');
  await page.goto('http://127.0.0.1:8080/pnm/authorization', { waitUntil: 'domcontentloaded' });
  await page.fill('input[type="text"]', 'admin');
  await page.fill('input[type="password"]', 'admin_password');
  await page.click('[data-qa="authorization-sign-in-button"]');
  await page.waitForTimeout(1000);

  // 2. Open Conferences -> first conference -> "Раскладки"
  console.log('Navigating to conferences...');
  await page.goto('http://127.0.0.1:8080/pnm/monitoring/conferences', { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(1000);

  const firstConf = page.locator('.main-list-item').first();
  await firstConf.click();
  await page.waitForTimeout(800);

  const layoutsTab = page.locator('.tc-tab, .mdc-tab').filter({ hasText: 'Раскладки' }).first();
  await layoutsTab.click();
  await page.waitForTimeout(800);

  // Click on "Создать" layout
  console.log('Opening layout configurator...');
  const addLayoutBtn = page.locator('[data-qa="conference-add-layout-button"]');
  if (await addLayoutBtn.count() > 0) {
    await addLayoutBtn.click();
  } else {
    await page.locator('.conference-layouts-item').first().click();
  }
  await page.waitForTimeout(1200);

  // ==========================================
  // STEP 1: Verify "Ручная настройка" hygiene
  // ==========================================
  console.log('\n--- Step 1: Verify "Ручная настройка" hygiene ---');
  const manualTab = page.locator('[data-qa="layout-mode-selector-manual"]');
  await manualTab.click();
  await page.waitForTimeout(1000);

  const manualState = await page.evaluate(() => {
    const host = document.querySelector('.manual-layout-configurator');
    const legacyBottomCtor = host ? host.querySelector('.main-form-section-16[data-vks-constructor="1"]') : null;
    const legacyBottomCounter = host ? host.querySelector('.main-form-section-16 [data-vks-counter="1"]') : null;
    const sidePanel = host ? host.querySelector('.base-layout-configurator-side-panel') : null;
    const vendorSelector = sidePanel ? sidePanel.querySelector('.layout-scheme-selector.manual-layout-configurator-side-panel') : null;
    const ctorPanel = sidePanel ? sidePanel.querySelector('[data-vks-constructor="1"]') : null;
    const codianGroup = host ? host.querySelector('[data-vks-codian-group="1"]') : null;
    const sizeChips = host ? host.querySelector('.layout-size-range-selector') : null;

    return {
      hasLegacyBottomCtor: !!legacyBottomCtor,
      hasLegacyBottomCounter: !!legacyBottomCounter,
      vendorSelectorVisible: vendorSelector ? window.getComputedStyle(vendorSelector).display !== 'none' : false,
      ctorPanelVisible: ctorPanel ? window.getComputedStyle(ctorPanel).display !== 'none' : false,
      codianGroupVisible: codianGroup ? window.getComputedStyle(codianGroup).display !== 'none' : false,
      sizeChipsVisible: sizeChips ? window.getComputedStyle(sizeChips).display !== 'none' : false
    };
  });

  assert(!manualState.hasLegacyBottomCtor, 'Zero legacy constructor sections in main body during Manual mode');
  assert(!manualState.hasLegacyBottomCounter, 'Zero legacy counters in main body during Manual mode');
  assert(manualState.vendorSelectorVisible, 'Vendor manual scheme selector in sidebar is visible');
  assert(!manualState.ctorPanelVisible, 'Constructor sidebar panel is hidden during Manual mode');
  assert(!manualState.codianGroupVisible, 'Codian MCU 4200 group in main body is cleanly hidden during Manual mode');
  assert(manualState.sizeChipsVisible, 'Vendor size range chips are visible during Manual mode');

  await page.screenshot({ path: '/tmp/test_constructor_step1_manual.png' });

  // ==========================================
  // STEP 2: Verify "Конструктор" mode activation & sidebar placement
  // ==========================================
  console.log('\n--- Step 2: Verify "Конструктор" mode activation & sidebar placement ---');
  const ctorTab = page.locator('[data-vks-constructor-tile="1"], [data-qa="layout-mode-selector-constructor"]');
  assert(await ctorTab.count() > 0, 'Constructor tile exists in mode selector');
  await ctorTab.click();
  await page.waitForTimeout(1000);

  const ctorState = await page.evaluate(() => {
    const host = document.querySelector('.manual-layout-configurator');
    const legacyBottomCtor = host ? host.querySelector('.main-form-section-16[data-vks-constructor="1"]') : null;
    const sidePanel = host ? host.querySelector('.base-layout-configurator-side-panel') : null;
    const vendorSelector = sidePanel ? sidePanel.querySelector('.layout-scheme-selector.manual-layout-configurator-side-panel') : null;
    const ctorPanel = sidePanel ? sidePanel.querySelector('[data-vks-constructor="1"]') : null;
    const codianGroup = host ? host.querySelector('[data-vks-codian-group="1"]') : null;
    const codianCards = codianGroup ? Array.from(codianGroup.querySelectorAll('[data-codian-id]')) : [];
    const sizeChips = host ? host.querySelector('.layout-size-range-selector') : null;

    const counter = ctorPanel ? ctorPanel.querySelector('[data-vks-counter="1"]') : null;
    const counterInput = counter ? counter.querySelector('[data-counter-input]') : null;
    const cards = ctorPanel ? Array.from(ctorPanel.querySelectorAll('[data-vks-constructor-mode]')) : [];
    const rows = ctorPanel ? Array.from(ctorPanel.querySelectorAll('.layout-scheme-selector__row')) : [];

    const tile = document.querySelector('[data-vks-constructor-tile="1"]');
    const isTileActive = tile ? tile.classList.contains('layout-mode-selector-item--active') : false;

    // Check width of side panel and cards to ensure zero overflow
    const sidePanelRect = sidePanel ? sidePanel.getBoundingClientRect() : null;
    const ctorPanelRect = ctorPanel ? ctorPanel.getBoundingClientRect() : null;

    return {
      isTileActive,
      hasLegacyBottomCtor: !!legacyBottomCtor,
      vendorSelectorHidden: vendorSelector ? window.getComputedStyle(vendorSelector).display === 'none' : true,
      ctorPanelVisible: ctorPanel ? window.getComputedStyle(ctorPanel).display !== 'none' : false,
      codianGroupVisible: codianGroup ? window.getComputedStyle(codianGroup).display !== 'none' : false,
      codianCardsCount: codianCards.length,
      sizeChipsHidden: sizeChips ? window.getComputedStyle(sizeChips).display === 'none' : true,
      hasCounter: !!counter,
      counterValue: counterInput ? counterInput.value : null,
      cardsCount: cards.length,
      cardsModes: cards.map(c => c.getAttribute('data-vks-constructor-mode')),
      rowsCount: rows.length,
      sidePanelWidth: sidePanelRect ? sidePanelRect.width : 0,
      ctorPanelWidth: ctorPanelRect ? ctorPanelRect.width : 0
    };
  });

  assert(ctorState.isTileActive, 'Constructor tab is highlighted active');
  assert(!ctorState.hasLegacyBottomCtor, 'Zero legacy constructor sections in main body during Constructor mode');
  assert(ctorState.vendorSelectorHidden, 'Vendor manual scheme selector is hidden in Constructor mode');
  assert(ctorState.ctorPanelVisible, 'Constructor panel is mounted and visible in sidebar');
  assert(ctorState.codianGroupVisible, 'Codian MCU 4200 group is mounted and visible in Constructor mode');
  assert(ctorState.codianCardsCount === 54, `All 54 Codian layout cards present in Constructor mode (found: ${ctorState.codianCardsCount})`);
  assert(ctorState.sizeChipsHidden, 'Vendor size range chips are cleanly hidden in Constructor mode');
  assert(ctorState.hasCounter, 'Video windows counter is present inside sidebar panel');
  assert(ctorState.cardsCount === 5, `All 5 constructor layout presets present in sidebar (found: ${ctorState.cardsCount})`);
  assert(ctorState.rowsCount >= 2, `Cards organized in rows for 300px sidebar width (found ${ctorState.rowsCount} rows)`);

  await page.screenshot({ path: '/tmp/test_constructor_step2_active.png' });

  // ==========================================
  // STEP 3: Verify Interactivity & State
  // ==========================================
  console.log('\n--- Step 3: Verify Interactivity & State in Constructor mode ---');

  // Click on "pin_l" card (Крупный слева)
  const pinLCard = page.locator('.dynamic-layout-configurator-side-panel [data-vks-constructor-mode="pin_l"]');
  assert(await pinLCard.count() > 0, 'Card "Крупный слева" (pin_l) exists');
  await pinLCard.click();
  await page.waitForTimeout(600);

  const pinLActive = await pinLCard.evaluate(el => el.classList.contains('layout-scheme-selector-item--active'));
  assert(pinLActive, 'Card "pin_l" becomes active after click');

  // Click counter increment (+) button
  const incBtn = page.locator('.dynamic-layout-configurator-side-panel [data-counter-action="inc"] button');
  assert(await incBtn.count() > 0, 'Counter increment (+) button exists');

  const beforeCount = await page.evaluate(() => {
    const input = document.querySelector('.dynamic-layout-configurator-side-panel [data-counter-input]');
    return input ? parseInt(input.value, 10) : 0;
  });

  await incBtn.click();
  await page.waitForTimeout(600);

  const afterCount = await page.evaluate(() => {
    const input = document.querySelector('.dynamic-layout-configurator-side-panel [data-counter-input]');
    return input ? parseInt(input.value, 10) : 0;
  });

  assert(afterCount === beforeCount + 1, `Counter increments by 1 (${beforeCount} -> ${afterCount})`);

  // Verify preset in store updated with new N and pin_l
  const currentPreset = await page.evaluate(() => {
    const root = document.getElementById('root') || document.getElementById('app');
    const store = root && root.__vue_app__ && root.__vue_app__.config.globalProperties.$pinia._s.get('layout-editor');
    return store && store.modifiedValue ? store.modifiedValue.preset : null;
  });

  assert(
    currentPreset === `vks_pin_l_${afterCount}`,
    `Store preset updated to vks_pin_l_${afterCount}`,
    `current: ${currentPreset}`
  );

  // ==========================================
  // STEP 3b: Verify Codian Card Selection & State
  // ==========================================
  console.log('\n--- Step 3b: Verify Codian Card Selection in Constructor ---');
  const codian07 = page.locator('[data-codian-id="codian_07"]');
  assert(await codian07.count() > 0, 'Codian card "codian_07" exists in catalog');
  await codian07.click();
  await page.waitForTimeout(600);

  const codian07Active = await codian07.evaluate(el => el.classList.contains('layout-scheme-selector-item--active'));
  assert(codian07Active, 'Codian card "codian_07" becomes active after click');

  // Verify constructor sidebar parametric card is not active
  const anyParametricActive = await page.evaluate(() => {
    return !!document.querySelector('.dynamic-layout-configurator-side-panel .layout-scheme-selector-item--active');
  });
  assert(!anyParametricActive, 'Parametric sidebar cards lose active state when Codian card is selected');

  // Verify counter syncs to codian_07 windows (13)
  const counterValAfterCodian = await page.evaluate(() => {
    const input = document.querySelector('.dynamic-layout-configurator-side-panel [data-counter-input]');
    return input ? parseInt(input.value, 10) : 0;
  });
  assert(counterValAfterCodian === 13, `Counter synced to Codian card window count (13, got: ${counterValAfterCodian})`);

  // Verify preset in store is codian_07
  const presetAfterCodian = await page.evaluate(() => {
    const root = document.getElementById('root') || document.getElementById('app');
    const store = root && root.__vue_app__ && root.__vue_app__.config.globalProperties.$pinia._s.get('layout-editor');
    return store && store.modifiedValue ? store.modifiedValue.preset : null;
  });
  assert(presetAfterCodian === 'codian_07', `Store preset set to codian_07 (got: ${presetAfterCodian})`);

  // Verify Constructor tile remains active
  const ctorTileStillActive = await page.evaluate(() => {
    const tile = document.querySelector('[data-vks-constructor-tile="1"]');
    return tile ? tile.classList.contains('layout-mode-selector-item--active') : false;
  });
  assert(ctorTileStillActive, 'Constructor mode tile remains active with Codian preset');

  // Verify re-clicking Constructor tab is a safe no-op (Bug 1 guard)
  await ctorTab.click();
  await page.waitForTimeout(400);
  const presetAfterReClick = await page.evaluate(() => {
    const root = document.getElementById('root') || document.getElementById('app');
    const store = root && root.__vue_app__ && root.__vue_app__.config.globalProperties.$pinia._s.get('layout-editor');
    return store && store.modifiedValue ? store.modifiedValue.preset : null;
  });
  assert(presetAfterReClick === 'codian_07', `Re-clicking Constructor tab preserves codian_07 without resetting (got: ${presetAfterReClick})`);

  // ==========================================
  // STEP 4: Verify Clean Exit back to Manual
  // ==========================================
  console.log('\n--- Step 4: Verify Clean Exit back to Manual ---');
  await manualTab.click();
  await page.waitForTimeout(1000);

  const backManualState = await page.evaluate(() => {
    const host = document.querySelector('.manual-layout-configurator');
    const legacyBottomCtor = host ? host.querySelector('.main-form-section-16[data-vks-constructor="1"]') : null;
    const sidePanel = host ? host.querySelector('.base-layout-configurator-side-panel') : null;
    const vendorSelector = sidePanel ? sidePanel.querySelector('.layout-scheme-selector.manual-layout-configurator-side-panel') : null;
    const ctorPanel = sidePanel ? sidePanel.querySelector('[data-vks-constructor="1"]') : null;
    const codianGroup = host ? host.querySelector('[data-vks-codian-group="1"]') : null;

    const manualTile = document.querySelector('[data-qa="layout-mode-selector-manual"]');
    const isManualActive = manualTile ? manualTile.classList.contains('layout-mode-selector-item--active') : false;

    return {
      isManualActive,
      hasLegacyBottomCtor: !!legacyBottomCtor,
      vendorSelectorVisible: vendorSelector ? window.getComputedStyle(vendorSelector).display !== 'none' : false,
      ctorPanelVisible: ctorPanel ? window.getComputedStyle(ctorPanel).display !== 'none' : false,
      codianGroupVisible: codianGroup ? window.getComputedStyle(codianGroup).display !== 'none' : false
    };
  });

  assert(backManualState.isManualActive, 'Manual tab is active again after switching back');
  assert(!backManualState.hasLegacyBottomCtor, 'Zero legacy constructor sections in main body after exit');
  assert(backManualState.vendorSelectorVisible, 'Vendor manual scheme selector is restored in sidebar');
  assert(!backManualState.ctorPanelVisible, 'Constructor sidebar panel is hidden after exit');
  assert(!backManualState.codianGroupVisible, 'Codian MCU 4200 group is cleanly hidden in Manual mode');

  // Verify page errors
  assert(pageErrors.length === 0, 'Zero page errors during execution', `errors: ${pageErrors.length}`);

  await page.screenshot({ path: '/tmp/test_constructor_step4_back_manual.png' });
  console.log('\nScreenshots saved to /tmp/test_constructor_*.png');

  console.log(`\n>>> SUMMARY: ${passed} checks PASSED, ${failed} FAILED, ${pageErrors.length} page errors.`);

  await browser.close();

  if (failed > 0 || pageErrors.length > 0) {
    process.exit(1);
  }
})();

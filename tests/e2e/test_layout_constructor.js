const { chromium } = require('playwright');

(async () => {
  console.log('>>> [Layout Constructor E2E] Starting comprehensive verification with Modular Matrix C x R...');

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
  page.on('pageerror', err => {
    console.log('>>> [PAGEERROR EVENT]', err && err.message ? err.message : err, err && err.stack ? err.stack : '');
    pageErrors.push(err && err.message ? err.message : String(err));
  });
  page.on('console', msg => {
    if (msg.type() === 'error') {
      console.log('>>> [CONSOLE ERROR]', msg.text());
    }
  });
  page.on('response', async resp => {
    if (resp.status() >= 400) {
      let body = '';
      try { body = await resp.text(); } catch (_) {}
      console.log('>>> [HTTP ERROR]', resp.status(), resp.url(), body.slice(0, 150));
    }
  });

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
    const base = host ? host.querySelector('.base-layout-configurator') : null;
    const legacyBottomCtor = host ? host.querySelector(':scope > .main-form-section-16[data-vks-constructor="1"]') : null;
    const legacyBottomCounter = host ? host.querySelector(':scope > .main-form-section-16 [data-vks-counter="1"]') : null;
    const legacyBottomCodian = host ? host.querySelector(':scope > .main-form-section-16[data-vks-codian-group="1"]') : null;
    const sidePanel = host ? host.querySelector('.base-layout-configurator-side-panel') : null;
    const content = host ? host.querySelector('.base-layout-configurator-content') : null;
    const vendorSelector = sidePanel ? sidePanel.querySelector('.layout-scheme-selector.manual-layout-configurator-side-panel') : null;
    const ctorPanel = sidePanel ? sidePanel.querySelector('[data-vks-constructor="1"]') : null;
    const codianGroup = sidePanel ? sidePanel.querySelector('[data-vks-codian-group="1"]') : null;
    const codianCards = codianGroup ? Array.from(codianGroup.querySelectorAll('[data-codian-id]')) : [];
    const codianSelector = codianGroup ? codianGroup.querySelector('.layout-scheme-selector') : null;
    const sizeChips = host ? host.querySelector('.layout-size-range-selector') : null;

    const codianRows = codianSelector ? Array.from(codianSelector.querySelectorAll('.layout-scheme-selector__row')) : [];
    const codianRowLengths = codianRows.map(r => r.querySelectorAll('[data-codian-id]').length);
    const isCodianDistributionValid = codianRowLengths.length === 11 &&
      codianRowLengths.slice(0, 10).every(len => len === 5) &&
      codianRowLengths[10] === 4;

    const codianNoOverflow = codianSelector ? codianSelector.scrollWidth <= codianSelector.clientWidth + 1 : false;

    const sidePanelRect = sidePanel ? sidePanel.getBoundingClientRect() : null;
    const codianRect = codianGroup ? codianGroup.getBoundingClientRect() : null;
    const contentRect = content ? content.getBoundingClientRect() : null;
    const baseRect = base ? base.getBoundingClientRect() : null;

    const isCodianLeftOfContent = codianRect && contentRect ? codianRect.right <= contentRect.left + 5 : false;
    const isCodianScrollable = codianSelector ? codianSelector.scrollHeight > codianSelector.clientHeight : false;

    return {
      hasLegacyBottomCtor: !!legacyBottomCtor,
      hasLegacyBottomCounter: !!legacyBottomCounter,
      hasLegacyBottomCodian: !!legacyBottomCodian,
      vendorSelectorVisible: vendorSelector ? window.getComputedStyle(vendorSelector).display !== 'none' : false,
      ctorPanelVisible: ctorPanel ? window.getComputedStyle(ctorPanel).display !== 'none' : false,
      codianGroupVisible: codianGroup ? (window.getComputedStyle(codianGroup).display !== 'none' && codianGroup.offsetParent !== null) : false,
      isCodianInSidePanel: sidePanel ? sidePanel.contains(codianGroup) : false,
      isCodianLeftOfContent,
      isCodianScrollable,
      codianCardsCount: codianCards.length,
      codianRowsCount: codianRows.length,
      isCodianDistributionValid,
      codianRowLengths,
      codianNoOverflow,
      sizeChipsVisible: sizeChips ? window.getComputedStyle(sizeChips).display !== 'none' : false,
      baseHeight: baseRect ? baseRect.height : 0,
      contentWidth: contentRect ? contentRect.width : 0,
      contentHeight: contentRect ? contentRect.height : 0
    };
  });

  assert(!manualState.hasLegacyBottomCtor, 'Zero legacy constructor sections in main body during Manual mode');
  assert(!manualState.hasLegacyBottomCounter, 'Zero legacy counters in main body during Manual mode');
  assert(!manualState.hasLegacyBottomCodian, 'Zero legacy Codian catalog sections at bottom of main body');
  assert(manualState.vendorSelectorVisible, 'Vendor manual scheme selector in sidebar is visible');
  assert(!manualState.ctorPanelVisible, 'Constructor sidebar panel is hidden during Manual mode');
  assert(manualState.codianGroupVisible, 'Codian MCU 4200 group in sidebar is visible during Manual mode');
  assert(manualState.isCodianInSidePanel, 'Codian catalog is mounted inside .base-layout-configurator-side-panel');
  assert(manualState.isCodianLeftOfContent, 'Codian catalog is situated to the LEFT of .base-layout-configurator-content');
  assert(manualState.isCodianScrollable, 'Codian catalog container has active vertical scrolling');
  assert(manualState.codianCardsCount === 54, `All 54 Codian layout cards present in Manual mode (found: ${manualState.codianCardsCount})`);
  assert(manualState.codianRowsCount === 11, `Codian catalog organized into exactly 11 rows (got ${manualState.codianRowsCount} rows)`);
  assert(manualState.isCodianDistributionValid, `Codian catalog: rows 1-10 have 5 cards, row 11 has 4 cards`);
  assert(manualState.codianNoOverflow, 'Codian catalog: zero horizontal overflow in sidebar');
  assert(manualState.sizeChipsVisible, 'Vendor size range chips are visible during Manual mode');
  assert(manualState.baseHeight <= 530, `Base configurator height is constrained to preview height (got: ${manualState.baseHeight}px <= 530px)`);
  assert(
    manualState.contentHeight > 0 && Math.abs((manualState.contentWidth / manualState.contentHeight) - (16 / 9)) < 0.1,
    `Preview content maintains 16:9 aspect ratio (ratio: ${(manualState.contentWidth / manualState.contentHeight).toFixed(2)})`
  );

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
    const base = host ? host.querySelector('.base-layout-configurator') : null;
    const legacyBottomCtor = host ? host.querySelector(':scope > .main-form-section-16[data-vks-constructor="1"]') : null;
    const legacyBottomCodian = host ? host.querySelector(':scope > .main-form-section-16[data-vks-codian-group="1"]') : null;
    const sidePanel = host ? host.querySelector('.base-layout-configurator-side-panel') : null;
    const content = host ? host.querySelector('.base-layout-configurator-content') : null;
    const vendorSelector = sidePanel ? sidePanel.querySelector('.layout-scheme-selector.manual-layout-configurator-side-panel') : null;
    const ctorPanel = sidePanel ? sidePanel.querySelector('[data-vks-constructor="1"]') : null;
    const sizeChips = host ? host.querySelector('.layout-size-range-selector') : null;

    const counter = ctorPanel ? ctorPanel.querySelector('[data-vks-counter="1"]') : null;
    const counterInput = counter ? counter.querySelector('[data-counter-input]') : null;
    const cards = ctorPanel ? Array.from(ctorPanel.querySelectorAll('[data-vks-constructor-mode]')) : [];
    const modesContainer = ctorPanel ? ctorPanel.querySelector('[data-vks-constructor-modes="1"]') : null;
    const modesRows = modesContainer ? Array.from(modesContainer.querySelectorAll('.layout-scheme-selector__row')) : [];
    const modesRowLengths = modesRows.map(r => r.querySelectorAll('[data-vks-constructor-mode]').length);

    // Codian catalog inside Constructor
    const codianGroupInCtor = ctorPanel ? ctorPanel.querySelector('[data-vks-codian-group="1"]') : null;
    const codianCardsInCtor = codianGroupInCtor ? Array.from(codianGroupInCtor.querySelectorAll('[data-codian-id]')) : [];

    const tile = document.querySelector('[data-vks-constructor-tile="1"]');
    const isTileActive = tile ? tile.classList.contains('layout-mode-selector-item--active') : false;

    const sidePanelRect = sidePanel ? sidePanel.getBoundingClientRect() : null;
    const ctorPanelRect = ctorPanel ? ctorPanel.getBoundingClientRect() : null;
    const contentRect = content ? content.getBoundingClientRect() : null;
    const baseRect = base ? base.getBoundingClientRect() : null;

    return {
      isTileActive,
      hasLegacyBottomCtor: !!legacyBottomCtor,
      hasLegacyBottomCodian: !!legacyBottomCodian,
      vendorSelectorHidden: vendorSelector ? window.getComputedStyle(vendorSelector).display === 'none' : true,
      ctorPanelVisible: ctorPanel ? window.getComputedStyle(ctorPanel).display !== 'none' : false,
      hasCodianInCtor: !!codianGroupInCtor,
      codianCardsInCtorCount: codianCardsInCtor.length,
      sizeChipsHidden: sizeChips ? window.getComputedStyle(sizeChips).display === 'none' : true,
      hasCounter: !!counter,
      counterValue: counterInput ? counterInput.value : null,
      cardsCount: cards.length,
      cardsModes: cards.map(c => c.getAttribute('data-vks-constructor-mode')),
      modesRowsCount: modesRows.length,
      modesRowLengths,
      sidePanelWidth: sidePanelRect ? sidePanelRect.width : 0,
      ctorPanelWidth: ctorPanelRect ? ctorPanelRect.width : 0,
      baseHeight: baseRect ? baseRect.height : 0,
      contentWidth: contentRect ? contentRect.width : 0,
      contentHeight: contentRect ? contentRect.height : 0
    };
  });

  assert(ctorState.isTileActive, 'Constructor tab is highlighted active');
  assert(!ctorState.hasLegacyBottomCtor, 'Zero legacy constructor sections in main body during Constructor mode');
  assert(!ctorState.hasLegacyBottomCodian, 'Zero legacy Codian catalog sections at bottom of main body');
  assert(ctorState.vendorSelectorHidden, 'Vendor manual scheme selector is hidden in Constructor mode');
  assert(ctorState.ctorPanelVisible, 'Constructor panel is mounted and visible in sidebar');
  assert(ctorState.hasCounter, 'Video windows counter is present inside sidebar panel');
  assert(ctorState.cardsCount === 6, `Exactly 6 modular constructor layout cards present in sidebar (found: ${ctorState.cardsCount})`);
  assert(ctorState.modesRowsCount === 2, `Constructor modes organized into exactly 2 rows (5 + 1, got: ${ctorState.modesRowsCount})`);
  assert(
    ctorState.modesRowLengths[0] === 5 && ctorState.modesRowLengths[1] === 1,
    `Row 1 has 5 cards, Row 2 has 1 card (lengths: ${ctorState.modesRowLengths.join(', ')})`
  );
  assert(ctorState.hasCodianInCtor, 'Codian catalog is mounted inside Constructor sidebar');
  assert(ctorState.codianCardsInCtorCount === 54, `All 54 Codian cards present in Constructor below modular modes (found: ${ctorState.codianCardsInCtorCount})`);
  assert(ctorState.sizeChipsHidden, 'Vendor size range chips are cleanly hidden in Constructor mode');

  const expectedModes = ['grid', 'pin_l', 'pin_r', 'pin_t', 'pin_b', 'dual'];
  const allModesPresent = expectedModes.every(k => ctorState.cardsModes.includes(k));
  assert(allModesPresent, 'All 6 modular modes are present in DOM (grid, pin_l, pin_r, pin_t, pin_b, dual)');

  // ==========================================
  // STEP 3: Verify Modular Matrix C x R for 12 and 13 Windows (Golden Standard & L-wrap Fix)
  // ==========================================
  console.log('\n--- Step 3: Verify Modular Matrix C x R (N=12 L-wrap Fix & N=13 Golden Standard) ---');

  // Set counter to 12
  const counterInput = page.locator('.dynamic-layout-configurator-side-panel [data-counter-input]');
  await counterInput.fill('12');
  await counterInput.press('Enter');
  await page.waitForTimeout(800);

  // Click pin_l card for 12 windows
  const pinLCard = page.locator('.dynamic-layout-configurator-side-panel [data-vks-constructor-mode="pin_l"]');
  await pinLCard.click();
  await page.waitForTimeout(600);

  // Inspect N=12 slots geometry in store
  const n12Geometry = await page.evaluate(() => {
    const root = document.getElementById('root') || document.getElementById('app');
    const store = root && root.__vue_app__ && root.__vue_app__.config.globalProperties.$pinia._s.get('layout-editor');
    const slots = store && store.modifiedValue ? store.modifiedValue.slots : [];
    return slots;
  });

  assert(n12Geometry.length === 12, `N=12 generates exactly 12 slots (got: ${n12Geometry.length})`);

  // Main window for 12 windows in 1280x720: 3x3 block in 5x4 matrix
  // unitW = 1280 / 5 = 256, unitH = 720 / 4 = 180
  // main window width = 3 * 256 = 768, height = 3 * 180 = 540
  const main12 = n12Geometry[0];
  assert(
    main12 && main12.x === 0 && main12.y === 0 && main12.width === 768 && main12.height === 540,
    `N=12 Main window is 3x3 block (768x540 at 0,0, got: ${main12 ? main12.width + 'x' + main12.height : 'null'})`
  );

  // Right block for 12 windows: 6 windows in 2 columns x 3 rows! (NOT a single column!)
  const rightWindows12 = n12Geometry.slice(1, 7);
  const rightXPositions = Array.from(new Set(rightWindows12.map(s => s.x)));
  const rightYPositions = Array.from(new Set(rightWindows12.map(s => s.y)));
  assert(
    rightXPositions.length === 2,
    `N=12 Right block uses EXACTLY 2 COLUMNS (L-wrap bug eliminated! Unique X: ${rightXPositions.join(', ')})`
  );
  assert(
    rightYPositions.length === 3,
    `N=12 Right block uses 3 ROWS (Unique Y: ${rightYPositions.join(', ')})`
  );
  assert(
    rightWindows12.every(s => s.width === 256 && s.height === 180),
    `N=12 All 6 right windows have uniform 256x180 dimensions (no squished strips!)`
  );

  // Bottom row for 12 windows: 5 windows full-width across the container!
  const bottomWindows12 = n12Geometry.slice(7);
  assert(bottomWindows12.length === 5, `N=12 Bottom row has exactly 5 windows`);
  assert(
    bottomWindows12[0].x === 0 && (bottomWindows12[4].x + bottomWindows12[4].width) === 1280,
    `N=12 Bottom row spans full container width (0 to 1280px)`
  );
  assert(
    bottomWindows12.every(s => s.y === 540 && s.width === 256 && s.height === 180),
    `N=12 All 5 bottom windows have uniform 256x180 dimensions`
  );

  await page.screenshot({ path: '/tmp/test_constructor_12_windows.png' });
  console.log('  [Screenshot] Saved N=12 layout to /tmp/test_constructor_12_windows.png');

  // Set counter to 13 (Golden standard)
  await counterInput.fill('13');
  await counterInput.press('Enter');
  await page.waitForTimeout(800);

  const n13Geometry = await page.evaluate(() => {
    const root = document.getElementById('root') || document.getElementById('app');
    const store = root && root.__vue_app__ && root.__vue_app__.config.globalProperties.$pinia._s.get('layout-editor');
    const slots = store && store.modifiedValue ? store.modifiedValue.slots : [];
    return slots;
  });

  assert(n13Geometry.length === 13, `N=13 generates exactly 13 slots (got: ${n13Geometry.length})`);
  const main13 = n13Geometry[0];
  assert(
    main13 && main13.x === 0 && main13.y === 0 && main13.width === 640 && main13.height === 360,
    `N=13 Main window is 2x2 block (640x360 in 4x4 matrix, got: ${main13 ? main13.width + 'x' + main13.height : 'null'})`
  );

  await page.screenshot({ path: '/tmp/test_constructor_13_windows.png' });
  console.log('  [Screenshot] Saved N=13 layout to /tmp/test_constructor_13_windows.png');

  // ==========================================
  // STEP 4: Verify Bidirectional Sync between Codian Presets and Constructor
  // ==========================================
  console.log('\n--- Step 4: Verify Bidirectional Sync between Codian Presets and Constructor ---');

  // Click codian_07 (13 windows) inside Constructor
  const codian07Card = page.locator('.dynamic-layout-configurator-side-panel [data-codian-id="codian_07"]');
  assert(await codian07Card.count() > 0, 'Card codian_07 exists inside Constructor sidebar');
  await codian07Card.click();
  await page.waitForTimeout(600);

  const syncState = await page.evaluate(() => {
    const root = document.getElementById('root') || document.getElementById('app');
    const store = root && root.__vue_app__ && root.__vue_app__.config.globalProperties.$pinia._s.get('layout-editor');
    const mv = store && store.modifiedValue;
    const input = document.querySelector('.dynamic-layout-configurator-side-panel [data-counter-input]');
    const ctorItem = document.querySelector('[data-vks-constructor-tile="1"]');
    const codian07 = document.querySelector('[data-codian-id="codian_07"]');
    const pinL = document.querySelector('[data-vks-constructor-mode="pin_l"]');

    return {
      counterVal: input ? parseInt(input.value, 10) : 0,
      preset: mv ? mv.preset : null,
      slotsCount: mv && Array.isArray(mv.slots) ? mv.slots.length : 0,
      isCtorTileActive: ctorItem ? ctorItem.classList.contains('layout-mode-selector-item--active') : false,
      isCodian07Active: codian07 ? codian07.classList.contains('layout-scheme-selector-item--active') : false,
      isPinLActive: pinL ? pinL.classList.contains('layout-scheme-selector-item--active') : false
    };
  });

  assert(syncState.counterVal === 13, `Clicking codian_07 synced counter to 13 (got: ${syncState.counterVal})`);
  assert(syncState.preset === 'codian_07', `Store preset set to codian_07 (got: ${syncState.preset})`);
  assert(syncState.slotsCount === 13, `Store slots set to 13 (got: ${syncState.slotsCount})`);
  assert(syncState.isCtorTileActive, 'Constructor mode tile remains active when clicking Codian card inside Constructor');
  assert(syncState.isCodian07Active, 'Codian card codian_07 is active in Constructor');
  assert(!syncState.isPinLActive, 'Modular pin_l card active state is cleared when selecting Codian preset');

  await page.screenshot({ path: '/tmp/test_constructor_codian_sync.png' });
  console.log('  [Screenshot] Saved Codian sync to /tmp/test_constructor_codian_sync.png');

  // Now click modular card pin_r to switch back to modular mode
  const pinRCard = page.locator('.dynamic-layout-configurator-side-panel [data-vks-constructor-mode="pin_r"]');
  await pinRCard.click();
  await page.waitForTimeout(600);

  const syncBackState = await page.evaluate(() => {
    const root = document.getElementById('root') || document.getElementById('app');
    const store = root && root.__vue_app__ && root.__vue_app__.config.globalProperties.$pinia._s.get('layout-editor');
    const mv = store && store.modifiedValue;
    const codian07 = document.querySelector('[data-codian-id="codian_07"]');
    const pinR = document.querySelector('[data-vks-constructor-mode="pin_r"]');

    return {
      preset: mv ? mv.preset : null,
      slotsCount: mv && Array.isArray(mv.slots) ? mv.slots.length : 0,
      isCodian07Active: codian07 ? codian07.classList.contains('layout-scheme-selector-item--active') : false,
      isPinRActive: pinR ? pinR.classList.contains('layout-scheme-selector-item--active') : false
    };
  });

  assert(syncBackState.preset === 'vks_pin_r_13', `Store preset set back to modular vks_pin_r_13 (got: ${syncBackState.preset})`);
  assert(syncBackState.slotsCount === 13, `Store slots count is 13 (got: ${syncBackState.slotsCount})`);
  assert(syncBackState.isPinRActive, 'Modular card pin_r is now active');
  assert(!syncBackState.isCodian07Active, 'Codian card codian_07 is no longer active');

  // ==========================================
  // STEP 5: Verify Dual Mode (2 Prominent Speakers)
  // ==========================================
  console.log('\n--- Step 5: Verify Dual Mode ---');
  const dualCard = page.locator('.dynamic-layout-configurator-side-panel [data-vks-constructor-mode="dual"]');
  assert(await dualCard.count() > 0, 'Dual mode card exists');
  await dualCard.click();
  await page.waitForTimeout(600);

  const dualState = await page.evaluate(() => {
    const root = document.getElementById('root') || document.getElementById('app');
    const store = root && root.__vue_app__ && root.__vue_app__.config.globalProperties.$pinia._s.get('layout-editor');
    const mv = store && store.modifiedValue;
    const slots = mv ? mv.slots : [];
    return {
      preset: mv ? mv.preset : null,
      slotsCount: slots.length,
      main1: slots[0],
      main2: slots[1]
    };
  });

  assert(dualState.preset === 'vks_dual_13', `Store preset is vks_dual_13 (got: ${dualState.preset})`);
  assert(dualState.slotsCount === 13, `Dual mode has 13 slots`);
  assert(
    dualState.main1 && dualState.main2 && dualState.main1.width === dualState.main2.width && dualState.main1.height === dualState.main2.height,
    'Dual mode has 2 equal prominent speaker windows side-by-side'
  );

  await page.screenshot({ path: '/tmp/test_constructor_dual.png' });
  console.log('  [Screenshot] Saved Dual mode to /tmp/test_constructor_dual.png');

  // ==========================================
  // STEP 6: Verify Clean Exit to Manual mode & Error Hygiene
  // ==========================================
  console.log('\n--- Step 6: Verify Clean Exit to Manual mode & Error Hygiene ---');
  await manualTab.click();
  await page.waitForTimeout(1000);

  const exitState = await page.evaluate(() => {
    const sidePanel = document.querySelector('.base-layout-configurator-side-panel');
    const vendorSelector = sidePanel ? sidePanel.querySelector('.layout-scheme-selector.manual-layout-configurator-side-panel') : null;
    const ctorPanel = sidePanel ? sidePanel.querySelector('[data-vks-constructor="1"]') : null;
    const codianGroup = sidePanel ? sidePanel.querySelector('[data-vks-codian-group="1"]') : null;

    return {
      vendorVisible: vendorSelector ? window.getComputedStyle(vendorSelector).display !== 'none' : false,
      ctorHidden: ctorPanel ? window.getComputedStyle(ctorPanel).display === 'none' : true,
      codianInSidePanel: sidePanel ? sidePanel.contains(codianGroup) : false
    };
  });

  assert(exitState.vendorVisible, 'Vendor manual scheme selector is visible again');
  assert(exitState.ctorHidden, 'Constructor panel is hidden after clicking Manual tab');
  assert(exitState.codianInSidePanel, 'Codian catalog moved back to sidebar root under vendor selector');

  if (pageErrors.length > 0) {
    console.error('Page errors details:');
    pageErrors.forEach((err, idx) => console.error(`  [${idx + 1}] ${err}`));
  }
  assert(pageErrors.length === 0, 'Zero page errors during execution', `errors: ${pageErrors.length}`);

  await page.screenshot({ path: '/tmp/test_constructor_modular_final.png' });
  console.log('\nFinal screenshot saved to /tmp/test_constructor_modular_final.png');

  console.log(`\n>>> SUMMARY: ${passed} checks PASSED, ${failed} FAILED, ${pageErrors.length} page errors.`);

  await browser.close();

  if (failed > 0 || pageErrors.length > 0) {
    process.exit(1);
  }
})();

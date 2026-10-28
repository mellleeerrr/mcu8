const { chromium } = require('playwright');

(async () => {
  console.log('>>> [Challenger 2] Starting Empirical Adversarial Verification of UI Geometry, Aspect Ratio & Mode Switching...');

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
  const consoleErrors = [];

  page.on('pageerror', err => {
    const msg = err && err.message ? err.message : String(err);
    console.error('>>> [PAGEERROR EVENT]', msg);
    pageErrors.push(msg);
  });

  page.on('console', msg => {
    if (msg.type() === 'error') {
      const txt = msg.text();
      // Ignore routine network or harmless favicon errors if any
      console.error('>>> [CONSOLE ERROR]', txt);
      consoleErrors.push(txt);
    }
  });

  let passed = 0;
  let failed = 0;

  function assert(condition, message, detail = '') {
    if (condition) {
      console.log(`  [PASS] ${message} ${detail ? '(' + detail + ')' : ''}`);
      passed++;
    } else {
      console.error(`  [FAIL] ${message} ${detail ? '(' + detail + ')' : ''}`);
      failed++;
    }
  }

  // 1. Authorization
  console.log('\n--- Step 0: Authorization & Navigation ---');
  await page.goto('http://127.0.0.1:8080/pnm/authorization', { waitUntil: 'domcontentloaded' });
  await page.fill('input[type="text"]', 'admin');
  await page.fill('input[type="password"]', 'admin_password');
  await page.click('[data-qa="authorization-sign-in-button"]');
  await page.waitForTimeout(1000);

  // 2. Open Conferences -> first conference -> "Раскладки"
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

  // Switch to Constructor mode initially
  const ctorTab = page.locator('[data-vks-constructor-tile="1"], [data-qa="layout-mode-selector-constructor"]');
  await ctorTab.click();
  await page.waitForTimeout(800);

  // ==========================================
  // SECTION 1: SIDEBAR GEOMETRY UNDER VARIOUS RESOLUTIONS
  // ==========================================
  console.log('\n--- Section 1: Sidebar Geometry under 1920x1080, 1366x768, 1280x720 ---');

  const resolutions = [
    { width: 1920, height: 1080, name: '1080p FHD' },
    { width: 1366, height: 768, name: '1366x768 WXGA' },
    { width: 1280, height: 720, name: '720p HD' }
  ];

  for (const res of resolutions) {
    console.log(`\nTesting Resolution: ${res.width}x${res.height} (${res.name})`);
    await page.setViewportSize({ width: res.width, height: res.height });
    await page.waitForTimeout(500);

    const geom = await page.evaluate(() => {
      const sidePanel = document.querySelector('.base-layout-configurator-side-panel');
      const ctorPanel = document.querySelector('.dynamic-layout-configurator-side-panel, [data-vks-constructor="1"]');
      const modesContainer = document.querySelector('[data-vks-constructor-modes="1"]');
      const cards = Array.from(document.querySelectorAll('.dynamic-layout-configurator-side-panel [data-vks-constructor-mode]'));
      const rows = modesContainer ? Array.from(modesContainer.querySelectorAll('.layout-scheme-selector__row')) : [];

      // Check card dimensions
      const cardMeasurements = cards.map(c => {
        const rect = c.getBoundingClientRect();
        return {
          width: Math.round(rect.width),
          height: Math.round(rect.height)
        };
      });

      // Check row structure and gaps
      const rowDetails = rows.map(r => {
        const rCards = Array.from(r.querySelectorAll('[data-vks-constructor-mode]'));
        const rects = rCards.map(c => c.getBoundingClientRect());
        const gaps = [];
        for (let i = 0; i < rects.length - 1; i++) {
          gaps.push(Math.round(rects[i + 1].left - rects[i].right));
        }
        return {
          cardCount: rCards.length,
          scrollWidth: r.scrollWidth,
          clientWidth: r.clientWidth,
          hasHorizontalOverflow: r.scrollWidth > r.clientWidth + 1,
          gaps
        };
      });

      return {
        sidePanelExists: !!sidePanel,
        sidePanelScrollWidth: sidePanel ? sidePanel.scrollWidth : 0,
        sidePanelClientWidth: sidePanel ? sidePanel.clientWidth : 0,
        sidePanelNoOverflow: sidePanel ? sidePanel.scrollWidth <= sidePanel.clientWidth + 1 : false,

        ctorPanelExists: !!ctorPanel,
        ctorPanelScrollWidth: ctorPanel ? ctorPanel.scrollWidth : 0,
        ctorPanelClientWidth: ctorPanel ? ctorPanel.clientWidth : 0,
        ctorPanelNoOverflow: ctorPanel ? ctorPanel.scrollWidth <= ctorPanel.clientWidth + 1 : false,

        modesContainerNoOverflow: modesContainer ? modesContainer.scrollWidth <= modesContainer.clientWidth + 1 : false,

        totalCards: cards.length,
        cardMeasurements,
        allCards52px: cardMeasurements.every(m => m.width === 52 && m.height === 52),
        rowCount: rows.length,
        rowDetails
      };
    });

    assert(geom.sidePanelExists, `[${res.width}x${res.height}] .base-layout-configurator-side-panel exists`);
    assert(
      geom.sidePanelNoOverflow,
      `[${res.width}x${res.height}] Side panel has zero horizontal overflow (scrollWidth=${geom.sidePanelScrollWidth} <= clientWidth=${geom.sidePanelClientWidth})`
    );
    assert(
      geom.ctorPanelNoOverflow,
      `[${res.width}x${res.height}] Constructor panel has zero horizontal overflow (scrollWidth=${geom.ctorPanelScrollWidth} <= clientWidth=${geom.ctorPanelClientWidth})`
    );
    assert(
      geom.modesContainerNoOverflow,
      `[${res.width}x${res.height}] Constructor modes container has zero horizontal overflow`
    );
    assert(geom.totalCards === 15, `[${res.width}x${res.height}] Exactly 15 cards present (got ${geom.totalCards})`);
    assert(geom.rowCount === 3, `[${res.width}x${res.height}] Exactly 3 rows of cards (got ${geom.rowCount})`);
    assert(
      geom.rowDetails.every(r => r.cardCount === 5),
      `[${res.width}x${res.height}] Each row contains exactly 5 cards (${geom.rowDetails.map(r => r.cardCount).join(', ')})`
    );
    assert(
      geom.allCards52px,
      `[${res.width}x${res.height}] All 15 cards measure 52x52px (measured width=${geom.cardMeasurements[0].width}px, height=${geom.cardMeasurements[0].height}px)`
    );
    const gapsSample = geom.rowDetails[0].gaps;
    assert(
      gapsSample.every(g => g === 2),
      `[${res.width}x${res.height}] Row 1 has uniform 2px gaps between cards (gaps: ${gapsSample.join('px, ')}px)`
    );
  }

  // Restore 1080p
  await page.setViewportSize({ width: 1920, height: 1080 });
  await page.waitForTimeout(500);

  // ==========================================
  // SECTION 2: CANVAS ASPECT RATIO (16:9)
  // ==========================================
  console.log('\n--- Section 2: Canvas Aspect Ratio Verification (Strict 16:9) ---');

  const presetsToTest = [
    { type: 'constructor', mode: 'grid', label: 'Constructor: Uniform Grid' },
    { type: 'constructor', mode: 'pin_l', label: 'Constructor: Pin Left' },
    { type: 'constructor', mode: 'split_50', label: 'Constructor: Split 50/50' },
    { type: 'constructor', mode: 'cinema', label: 'Constructor: Cinema 21:9' },
    { type: 'manual', codianId: 'codian_07', label: 'Manual: Codian 07 (13 windows)' },
    { type: 'manual', codianId: 'codian_01', label: 'Manual: Codian 01 (1 window)' },
    { type: 'manual', codianId: 'codian_52', label: 'Manual: Codian 52 (2 windows)' }
  ];

  for (const preset of presetsToTest) {
    if (preset.type === 'constructor') {
      const ctorTab = page.locator('[data-vks-constructor-tile="1"], [data-qa="layout-mode-selector-constructor"]');
      await ctorTab.click();
      await page.waitForTimeout(300);

      const card = page.locator(`.dynamic-layout-configurator-side-panel [data-vks-constructor-mode="${preset.mode}"]`);
      await card.click();
      await page.waitForTimeout(400);
    } else {
      const manualTab = page.locator('[data-qa="layout-mode-selector-manual"]');
      await manualTab.click();
      await page.waitForTimeout(400);

      const codianCard = page.locator(`[data-codian-id="${preset.codianId}"]`);
      await codianCard.scrollIntoViewIfNeeded();
      await codianCard.click();
      await page.waitForTimeout(400);
    }

    const canvasMetrics = await page.evaluate(() => {
      const content = document.querySelector('.base-layout-configurator-content');
      if (!content) return null;
      const rect = content.getBoundingClientRect();
      const ratio = rect.width / rect.height;
      return {
        width: rect.width,
        height: rect.height,
        ratio,
        deltaFrom16_9: Math.abs(ratio - (16 / 9))
      };
    });

    assert(canvasMetrics !== null, `[${preset.label}] .base-layout-configurator-content exists`);
    assert(
      canvasMetrics && canvasMetrics.height > 0,
      `[${preset.label}] Canvas has non-zero height (${canvasMetrics ? canvasMetrics.width.toFixed(1) : 0}x${canvasMetrics ? canvasMetrics.height.toFixed(1) : 0})`
    );
    assert(
      canvasMetrics && canvasMetrics.deltaFrom16_9 < 0.05,
      `[${preset.label}] Ratio strictly conforms to 16:9 (ratio: ${canvasMetrics.ratio.toFixed(4)}, target: ${(16/9).toFixed(4)}, delta: ${canvasMetrics.deltaFrom16_9.toFixed(4)})`
    );
  }

  // ==========================================
  // SECTION 3: BIDIRECTIONAL MODE SWITCHING (5+ ROUNDTRIPS)
  // ==========================================
  console.log('\n--- Section 3: Bidirectional Mode Switching (6 Iterations) ---');

  const manualSelector = page.locator('[data-qa="layout-mode-selector-manual"]');
  const ctorSelector = page.locator('[data-vks-constructor-tile="1"], [data-qa="layout-mode-selector-constructor"]');

  for (let cycle = 1; cycle <= 6; cycle++) {
    console.log(`\nCycle ${cycle}/6: Switching to Manual Mode...`);
    await manualSelector.click();
    await page.waitForTimeout(400);

    const manualCheck = await page.evaluate(() => {
      const codianGroup = document.querySelector('[data-vks-codian-group="1"]');
      const codianCards = codianGroup ? codianGroup.querySelectorAll('[data-codian-id]').length : 0;
      const codianVisible = codianGroup && window.getComputedStyle(codianGroup).display !== 'none' && codianGroup.offsetParent !== null;

      const ctorPanel = document.querySelector('.dynamic-layout-configurator-side-panel, [data-vks-constructor="1"]');
      const ctorVisible = ctorPanel && window.getComputedStyle(ctorPanel).display !== 'none';

      return {
        codianGroupPresent: !!codianGroup,
        codianCards,
        codianVisible,
        ctorVisible: !!ctorVisible
      };
    });

    assert(manualCheck.codianGroupPresent, `Cycle ${cycle}: Codian MCU 4200 group present in DOM`);
    assert(manualCheck.codianCards === 54, `Cycle ${cycle}: All 54 Codian presets mounted in Manual mode (got ${manualCheck.codianCards})`);
    assert(manualCheck.codianVisible, `Cycle ${cycle}: Codian presets catalog is visible in Manual mode`);
    assert(!manualCheck.ctorVisible, `Cycle ${cycle}: Constructor panel is completely hidden in Manual mode`);

    console.log(`Cycle ${cycle}/6: Switching to Constructor Mode...`);
    await ctorSelector.click();
    await page.waitForTimeout(400);

    const ctorCheck = await page.evaluate(() => {
      const codianGroup = document.querySelector('[data-vks-codian-group="1"]');
      const codianVisible = codianGroup && window.getComputedStyle(codianGroup).display !== 'none' && codianGroup.offsetParent !== null;

      const ctorPanel = document.querySelector('.dynamic-layout-configurator-side-panel, [data-vks-constructor="1"]');
      const ctorVisible = ctorPanel && window.getComputedStyle(ctorPanel).display !== 'none';
      const ctorCards = ctorPanel ? ctorPanel.querySelectorAll('[data-vks-constructor-mode]').length : 0;

      return {
        codianVisible: !!codianVisible,
        ctorVisible: !!ctorVisible,
        ctorCards
      };
    });

    assert(ctorCheck.ctorVisible, `Cycle ${cycle}: Constructor panel is visible in Constructor mode`);
    assert(ctorCheck.ctorCards === 15, `Cycle ${cycle}: All 15 dynamic archetype cards visible in Constructor mode (got ${ctorCheck.ctorCards})`);
    assert(!ctorCheck.codianVisible, `Cycle ${cycle}: Codian presets catalog is completely hidden in Constructor mode`);
  }

  // ==========================================
  // SECTION 4: ERROR HYGIENE (PAGE ERRORS & CONSOLE EXCEPTIONS)
  // ==========================================
  console.log('\n--- Section 4: Error Hygiene Verification ---');
  if (pageErrors.length > 0) {
    console.error('Captured page errors:');
    pageErrors.forEach((e, idx) => console.error(`  [${idx + 1}] ${e}`));
  }
  assert(pageErrors.length === 0, 'Zero page errors (unhandled exceptions) detected throughout', `count: ${pageErrors.length}`);

  if (consoleErrors.length > 0) {
    console.error('Captured console errors:');
    consoleErrors.forEach((e, idx) => console.error(`  [${idx + 1}] ${e}`));
  }
  assert(consoleErrors.length === 0, 'Zero console error messages detected throughout', `count: ${consoleErrors.length}`);

  console.log(`\n========================================================`);
  console.log(`>>> [Challenger 2] SUMMARY: ${passed} PASSED, ${failed} FAILED, ${pageErrors.length} PAGE ERRORS, ${consoleErrors.length} CONSOLE ERRORS`);
  console.log(`========================================================`);

  await browser.close();

  if (failed > 0 || pageErrors.length > 0 || consoleErrors.length > 0) {
    process.exit(1);
  }
})();

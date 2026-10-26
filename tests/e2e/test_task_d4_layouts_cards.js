const { chromium } = require('playwright');

(async () => {
  console.log('>>> [Task D4 E2E] Starting Playwright Conference Tabs, Layout Divider & Presentation Cards Test...');

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

  console.log('Navigating to http://127.0.0.1:8080/pnm/authorization...');
  await page.goto('http://127.0.0.1:8080/pnm/authorization', { waitUntil: 'domcontentloaded' });
  await page.fill('input[type="text"]', 'admin');
  await page.fill('input[type="password"]', 'admin_password');
  await page.click('[data-qa="authorization-sign-in-button"]');
  await page.waitForTimeout(1500);

  console.log('Opening conference details...');
  await page.goto('http://127.0.0.1:8080/pnm/monitoring/conferences', { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(1200);

  const firstConf = page.locator('.main-list-item').first();
  if (await firstConf.count() > 0) {
    await firstConf.click();
    await page.waitForTimeout(800);
  }

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

  // --- Step 1: Layout Screen Divider Line ---
  console.log('\n--- Step 1: Layout Screen Audio Channels Divider Line ---');
  const layoutsTab = page.locator('.tc-tab, .mdc-tab').filter({ hasText: 'Раскладки' }).first();
  if (await layoutsTab.count() > 0) {
    await layoutsTab.click();
    await page.waitForTimeout(800);

    const dividerData = await page.evaluate(() => {
      const el = document.querySelector('.main-card-body.conference-audio-channels, .main-card-body.session-audio-channels');
      if (!el) return null;
      const cs = window.getComputedStyle(el);
      return {
        found: true,
        borderTopColor: cs.borderTopColor,
        borderTopWidth: cs.borderTopWidth
      };
    });

    assert(dividerData && dividerData.found, 'Layout audio channels divider container exists');
    assert(
      dividerData && dividerData.borderTopColor === 'rgb(15, 28, 38)',
      'Layout audio channels divider is dark #0F1C26 (neutralizes white #dfdfdf)',
      dividerData ? dividerData.borderTopColor : 'null'
    );
  } else {
    assert(false, 'Layouts tab found');
  }

  // --- Step 2: Presentation Cards in "Запись и трансляция" ---
  console.log('\n--- Step 2: Presentation Cards in "Запись и трансляция" ---');
  const recTab = page.locator('.tc-tab, .mdc-tab').filter({ hasText: 'Запись и трансляция' }).first();
  if (await recTab.count() > 0) {
    await recTab.click();
    await page.waitForTimeout(800);

    const getRecCards = () => page.evaluate(() => {
      const cards = Array.from(document.querySelectorAll('.recording-option'));
      return cards.map(c => {
        const cs = window.getComputedStyle(c);
        const radio = c.querySelector('.recording-option__radio');
        const rcs = radio ? window.getComputedStyle(radio) : null;
        const isSelected = c.classList.contains('recording-option--selected');
        return {
          isSelected,
          bg: cs.backgroundColor,
          color: cs.color,
          border: cs.borderColor,
          radioBorder: rcs ? rcs.borderColor : null
        };
      });
    });

    let recCardsData = await getRecCards();
    assert(recCardsData.length >= 2, `Found ${recCardsData.length} presentation cards in "Запись и трансляция"`);

    if (!recCardsData.some(c => c.isSelected)) {
      await page.locator('.recording-option').first().click();
      await page.waitForTimeout(400);
      recCardsData = await getRecCards();
    }

    const unselected = recCardsData.find(c => !c.isSelected);
    if (unselected) {
      assert(
        unselected.bg === 'rgb(36, 47, 61)',
        'Unselected presentation card background is #242F3D (NOT white #f8f8f8)',
        unselected.bg
      );
      assert(
        unselected.color === 'rgb(127, 139, 153)',
        'Unselected presentation card text is muted #7F8B99 (NOT dark rgba(0,0,0,0.54))',
        unselected.color
      );
      assert(
        unselected.border === 'rgb(36, 51, 66)',
        'Unselected presentation card border is #243342',
        unselected.border
      );
    } else {
      assert(false, 'Found unselected presentation card');
    }

    const selected = recCardsData.find(c => c.isSelected);
    if (selected) {
      assert(
        selected.bg === 'rgb(43, 82, 120)',
        'Selected presentation card background is active #2B5278',
        selected.bg
      );
      assert(
        selected.color === 'rgb(255, 255, 255)',
        'Selected presentation card text is white #FFFFFF',
        selected.color
      );
    } else {
      assert(false, 'Found selected presentation card');
    }
  } else {
    assert(false, 'Recording & streaming tab found');
  }

  // --- Step 3: Conference Mode Selector Cards in "Основное" ---
  console.log('\n--- Step 3: Conference Mode Selector Cards in "Основное" ---');
  const mainTab = page.locator('.tc-tab, .mdc-tab').filter({ hasText: 'Основное' }).first();
  if (await mainTab.count() > 0) {
    await mainTab.click();
    await page.waitForTimeout(800);

    const modeCardsData = await page.evaluate(() => {
      const items = Array.from(document.querySelectorAll('.conference-mode-selector-item'));
      return items.map(it => {
        const cs = window.getComputedStyle(it);
        const title = it.querySelector('.conference-mode-selector-item__text--primary');
        const desc = it.querySelector('.conference-mode-selector-item__text--secondary');
        const tcs = title ? window.getComputedStyle(title) : null;
        const dcs = desc ? window.getComputedStyle(desc) : null;
        const isActive = it.classList.contains('conference-mode-selector-item--active');
        return {
          isActive,
          bg: cs.backgroundColor,
          border: cs.borderColor,
          titleColor: tcs ? tcs.color : null,
          descColor: dcs ? dcs.color : null
        };
      });
    });

    assert(modeCardsData.length >= 2, `Found ${modeCardsData.length} conference mode cards`);

    const activeMode = modeCardsData.find(m => m.isActive);
    if (activeMode) {
      assert(
        activeMode.bg === 'rgb(43, 82, 120)',
        'Active conference mode card background is active #2B5278',
        activeMode.bg
      );
      assert(
        activeMode.titleColor === 'rgb(255, 255, 255)',
        'Active mode card title is white #FFFFFF',
        activeMode.titleColor
      );
    } else {
      assert(false, 'Found active conference mode card');
    }

    const inactiveMode = modeCardsData.find(m => !m.isActive);
    if (inactiveMode) {
      assert(
        inactiveMode.titleColor === 'rgb(255, 255, 255)',
        'Inactive mode card title is white #FFFFFF',
        inactiveMode.titleColor
      );
      assert(
        inactiveMode.descColor === 'rgb(127, 139, 153)',
        'Inactive mode card description is muted #7F8B99 (NOT dark rgba(0,0,0,0.54))',
        inactiveMode.descColor
      );
    } else {
      assert(false, 'Found inactive conference mode card');
    }

    // --- Step 4: Secondary Text Buttons ("Отмена") ---
    console.log('\n--- Step 4: Secondary Text Buttons ("Отмена") ---');
    const cancelBtnData = await page.evaluate(() => {
      const btns = Array.from(document.querySelectorAll('button.tc-button'));
      const cancelBtn = btns.find(b => b.textContent.trim() === 'Отмена');
      if (!cancelBtn) return null;
      const cs = window.getComputedStyle(cancelBtn);
      return {
        found: true,
        color: cs.color
      };
    });

    if (cancelBtnData && cancelBtnData.found) {
      assert(
        cancelBtnData.color === 'rgb(127, 139, 153)',
        'Secondary Cancel button text is muted #7F8B99 (NOT dark rgba(0,0,0,0.87))',
        cancelBtnData.color
      );
    } else {
      console.log('  [i] Cancel button not present on current form state, skipping button check');
    }

    // --- Step 5: SVG Rect Artifact Elimination on Form Links ---
    console.log('\n--- Step 5: SVG Rect Artifact Elimination on Form Links ---');
    const svgRectData = await page.evaluate(() => {
      const rects = Array.from(document.querySelectorAll('.main-form-row svg rect, .main-form svg rect'));
      return rects.map(r => window.getComputedStyle(r).fill);
    });

    const whiteRects = svgRectData.filter(fill => fill === 'rgb(255, 255, 255)');
    assert(
      whiteRects.length === 0,
      'Zero white bounding box SVG rects on form links',
      `white rects: ${whiteRects.length}/${svgRectData.length}`
    );
  } else {
    assert(false, 'Main tab found');
  }

  // --- Step 6: Codec Bitrates & Form Descriptions on "Дополнительно" ---
  console.log('\n--- Step 6: Codec Bitrates & Form Descriptions on "Дополнительно" ---');
  const advTab = page.locator('.tc-tab, .mdc-tab').filter({ hasText: 'Дополнительно' }).first();
  if (await advTab.count() > 0) {
    await advTab.click();
    await page.waitForTimeout(800);

    const darkTextElements = await page.evaluate(() => {
      const textEls = Array.from(document.querySelectorAll('.main-form span, .main-form p, .main-form label'));
      const darks = [];
      for (const el of textEls) {
        if (el.children.length === 0 && el.textContent.trim().length > 0) {
          const cs = window.getComputedStyle(el);
          const m = cs.color.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
          if (m) {
            const r = parseInt(m[1]), g = parseInt(m[2]), b = parseInt(m[3]);
            if (r < 75 && g < 75 && b < 75) {
              darks.push({
                text: el.textContent.trim().slice(0, 30),
                color: cs.color
              });
            }
          }
        }
      }
      return darks;
    });

    assert(
      darkTextElements.length === 0,
      'Zero dark text elements on "Дополнительно" form (codecs, bitrates, hints)',
      `dark elements: ${darkTextElements.length}`
    );
  } else {
    assert(false, 'Additional tab found');
  }

  // Capture verification screenshot
  await page.screenshot({ path: '/tmp/task_d4_layouts_cards_fixed.png' });
  console.log('\nScreenshot captured: /tmp/task_d4_layouts_cards_fixed.png');

  // Page errors check
  assert(pageErrors.length === 0, 'Zero page errors logged during execution', `errors: ${pageErrors.length}`);

  console.log(`\n>>> SUMMARY: ${passed} checks PASSED, ${failed} FAILED, ${pageErrors.length} page errors.`);
  await browser.close();

  if (failed > 0 || pageErrors.length > 0) {
    process.exit(1);
  }
})();

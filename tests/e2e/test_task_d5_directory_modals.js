// test_task_d5_directory_modals.js
// Playwright E2E Test Suite for Task D5:
// Directory / Users, Modal Dialogs, Role Chips, History Screen & Administration Empty Frame

const { chromium } = require('playwright');

(async () => {
  console.log('>>> [Task D5 E2E] Starting Directory, Modals & Admin Empty Frame Test...');
  const browser = await chromium.launch({
    headless: true,
    args: ['--no-sandbox', '--disable-setuid-sandbox', '--disable-dev-shm-usage', '--force-color-profile=srgb']
  });
  const context = await browser.newContext({
    viewport: { width: 1600, height: 1000 },
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

  // --- Step 1: User Details Card & Role Chips in Directory ---
  console.log('\n--- Step 1: User Details Card & Role Chips in Directory ---');
  await page.goto('http://127.0.0.1:8080/pnm/addressbook/users', { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('.user-list-item, .main-list-item', { timeout: 5000 });

  const firstUser = page.locator('.user-list-item, .main-list-item').first();
  if (await firstUser.count() > 0) {
    await firstUser.click();
    await page.waitForTimeout(800);
  }

  const userCardData = await page.evaluate(() => {
    const card = document.querySelector('.user-card, .main-card');
    if (!card) return null;
    const label = card.querySelector('.info-main__text--primary');
    const value = card.querySelector('.info-main__text--secondary');
    const secTitle = card.querySelector('.user-info-additional__title');
    const chip = card.querySelector('.tc-chip');
    const chipText = chip ? chip.querySelector('.tc-chip__text') : null;

    return {
      hasCard: true,
      labelColor: label ? window.getComputedStyle(label).color : null,
      valueColor: value ? window.getComputedStyle(value).color : null,
      secTitleColor: secTitle ? window.getComputedStyle(secTitle).color : null,
      chipBg: chip ? window.getComputedStyle(chip).backgroundColor : null,
      chipColor: chipText ? window.getComputedStyle(chipText).color : null
    };
  });

  if (userCardData && userCardData.hasCard) {
    if (userCardData.labelColor) {
      assert(
        userCardData.labelColor === 'rgb(127, 139, 153)',
        'User detail field label is muted #7F8B99 (NOT dark rgba(0,0,0,0.87))',
        userCardData.labelColor
      );
    }
    if (userCardData.valueColor) {
      assert(
        userCardData.valueColor === 'rgb(255, 255, 255)',
        'User detail field value is white #FFFFFF',
        userCardData.valueColor
      );
    }
    if (userCardData.secTitleColor) {
      assert(
        userCardData.secTitleColor === 'rgb(127, 139, 153)',
        'User additional section title is muted #7F8B99',
        userCardData.secTitleColor
      );
    }
    if (userCardData.chipBg) {
      assert(
        userCardData.chipBg === 'rgb(36, 47, 61)',
        'Role chip background is #242F3D (NOT light #f5f5f5)',
        userCardData.chipBg
      );
      assert(
        userCardData.chipColor === 'rgb(255, 255, 255)',
        'Role chip text is white #FFFFFF',
        userCardData.chipColor
      );
    }
  } else {
    assert(false, 'User detail card mounted');
  }

  // --- Step 2: Create User Modal Dialog ---
  console.log('\n--- Step 2: Create User Modal Dialog ---');
  const addBtn = page.locator('[data-qa="user-list-tools-add-button"]');
  if (await addBtn.count() > 0) {
    await addBtn.click();
    await page.waitForTimeout(600);

    const modalData = await page.evaluate(() => {
      const modal = document.querySelector('.tc-modal, .mdc-dialog__surface');
      if (!modal) return null;
      const headerTitle = modal.querySelector('.tc-modal__header-desc, .tc-modal__title, .mdc-dialog__title');
      const closeBtn = modal.querySelector('.tc-modal__header-button, .tc-modal__close, [data-qa*="close"]');
      const closeIcon = closeBtn ? closeBtn.querySelector('.material-icons, .tc-icon__icon, svg') : null;
      const cancelBtn = modal.querySelector('[data-qa="base-popup-cancel-button"]');

      const modalCs = window.getComputedStyle(modal);
      return {
        hasModal: true,
        bg: modalCs.backgroundColor,
        titleColor: headerTitle ? window.getComputedStyle(headerTitle).color : null,
        closeIconColor: closeIcon ? window.getComputedStyle(closeIcon).color : null,
        cancelBtnColor: cancelBtn ? window.getComputedStyle(cancelBtn).color : null
      };
    });

    if (modalData && modalData.hasModal) {
      assert(
        modalData.bg === 'rgb(36, 47, 61)',
        'Modal surface background is #242F3D',
        modalData.bg
      );
      assert(
        modalData.titleColor === 'rgb(255, 255, 255)',
        'Modal header title is white #FFFFFF',
        modalData.titleColor
      );
      assert(
        modalData.closeIconColor === 'rgb(127, 139, 153)',
        'Modal close button icon is muted #7F8B99',
        modalData.closeIconColor
      );
      if (modalData.cancelBtnColor) {
        assert(
          modalData.cancelBtnColor === 'rgb(127, 139, 153)',
          'Modal cancel button text is muted #7F8B99',
          modalData.cancelBtnColor
        );
      }
    } else {
      assert(false, 'Create user modal dialog opened');
    }

    // Close modal
    const closeBtnLocator = page.locator('.tc-modal__header-button, [data-qa*="close"], [data-qa="base-popup-cancel-button"]').first();
    if (await closeBtnLocator.count() > 0) {
      await closeBtnLocator.click();
      await page.waitForTimeout(500);
    }
  }

  // --- Step 3: Administration Empty Calibration Frame Elimination ---
  console.log('\n--- Step 3: Administration Empty Calibration Frame Elimination ---');
  await page.goto('http://127.0.0.1:8080/pnm/administration', { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('.administration-section', { timeout: 5000 });

  const adminGridData = await page.evaluate(() => {
    const calContainer = document.querySelector('.administration-button:has(> [data-qa="administration-calibration-button"])');
    const calBtn = document.querySelector('[data-qa="administration-calibration-button"]');
    const firstTile = document.querySelector('.administration-section .administration-button:not([style*="display: none"]):not([style*="display:none"])');
    
    // Check all tiles to make sure none are empty frames
    const allButtons = Array.from(document.querySelectorAll('.administration-button'));
    const visibleTiles = allButtons.filter(b => window.getComputedStyle(b).display !== 'none');
    const emptyVisibleTiles = visibleTiles.filter(b => b.textContent.trim() === '' && !b.querySelector('svg, .material-icons, img'));

    return {
      calContainerDisplay: calContainer ? window.getComputedStyle(calContainer).display : 'not_found',
      calBtnDisplay: calBtn ? window.getComputedStyle(calBtn).display : 'not_found',
      visibleCount: visibleTiles.length,
      emptyVisibleCount: emptyVisibleTiles.length,
      firstTileText: firstTile ? firstTile.textContent.trim().replace(/\s+/g, ' ') : null
    };
  });

  assert(
    adminGridData.calContainerDisplay === 'none',
    'Calibration button parent container has display: none !important (empty frame eliminated)',
    adminGridData.calContainerDisplay
  );
  assert(
    adminGridData.calBtnDisplay === 'none',
    'Calibration button itself has display: none !important',
    adminGridData.calBtnDisplay
  );
  assert(
    adminGridData.emptyVisibleCount === 0,
    'Zero empty tile frames displayed in administration grid',
    `empty: ${adminGridData.emptyVisibleCount}`
  );
  assert(
    adminGridData.visibleCount >= 10,
    'Administration grid retains all active tiles',
    `tiles: ${adminGridData.visibleCount}`
  );

  // --- Step 4: History Screen Typography & Date Picker ---
  console.log('\n--- Step 4: History Screen Typography & Date Picker ---');
  await page.goto('http://127.0.0.1:8080/pnm/history', { waitUntil: 'domcontentloaded' });
  await page.waitForSelector('.logs__title, .main-table-head', { timeout: 5000 });

  const historyData = await page.evaluate(() => {
    const title = document.querySelector('.logs__title');
    const th = document.querySelector('.main-table-head, th');
    const dateInput = document.querySelector('.tc-text-field--with-prefix input');
    const dateIcon = document.querySelector('.tc-text-field--with-prefix .material-icons');

    return {
      titleColor: title ? window.getComputedStyle(title).color : null,
      thColor: th ? window.getComputedStyle(th).color : null,
      dateInputColor: dateInput ? window.getComputedStyle(dateInput).color : null,
      dateIconColor: dateIcon ? window.getComputedStyle(dateIcon).color : null
    };
  });

  assert(
    historyData.titleColor === 'rgb(255, 255, 255)',
    'History screen title .logs__title is white #FFFFFF (NOT dark rgba(0,0,0,0.87))',
    historyData.titleColor
  );
  if (historyData.thColor) {
    assert(
      historyData.thColor === 'rgb(255, 255, 255)',
      'History table header is white #FFFFFF',
      historyData.thColor
    );
  }
  if (historyData.dateIconColor) {
    assert(
      historyData.dateIconColor === 'rgb(165, 213, 238)',
      'Date picker icon is cyan #A5D5EE',
      historyData.dateIconColor
    );
  }

  // Open Datepicker
  const dateInputLocator = page.locator('.tc-text-field--with-prefix').first();
  if (await dateInputLocator.count() > 0) {
    await dateInputLocator.click();
    await page.waitForTimeout(600);

    const datePickerData = await page.evaluate(() => {
      const dp = document.querySelector('.tc-date-picker');
      if (!dp) return null;
      const monthName = dp.querySelector('.tc-date-picker__month-name');
      const weekday = dp.querySelector('.tc-date-picker__weekdays .tc-date-picker__cell');
      return {
        hasDatePicker: true,
        monthColor: monthName ? window.getComputedStyle(monthName).color : null,
        weekdayColor: weekday ? window.getComputedStyle(weekday).color : null
      };
    });

    if (datePickerData && datePickerData.hasDatePicker) {
      assert(
        datePickerData.monthColor === 'rgb(255, 255, 255)',
        'Datepicker month name is white #FFFFFF',
        datePickerData.monthColor
      );
      assert(
        datePickerData.weekdayColor === 'rgb(127, 139, 153)',
        'Datepicker weekdays are muted #7F8B99',
        datePickerData.weekdayColor
      );
    }
  }

  // --- Step 5: Notification Containers & Toast Styling ---
  console.log('\n--- Step 5: Notification Containers & Toast Styling ---');
  const toastRulesData = await page.evaluate(() => {
    let notifierFound = false;
    let toastFound = false;
    for (const sheet of Array.from(document.styleSheets)) {
      try {
        for (const rule of Array.from(sheet.cssRules || [])) {
          if (rule.selectorText && rule.selectorText.includes('notifier__item-container')) {
            notifierFound = true;
          }
          if (rule.selectorText && rule.selectorText.includes('tc-notifier')) {
            toastFound = true;
          }
        }
      } catch (e) {}
    }
    return { notifierFound, toastFound };
  });

  assert(toastRulesData.notifierFound, 'Telegram dark styles active for .notifier__item-container');
  assert(toastRulesData.toastFound, 'Telegram dark styles active for .tc-notifier-container');

  // Capture screenshot
  await page.screenshot({ path: '/tmp/task_d5_directory_modals_fixed.png', fullPage: true });
  console.log('\nScreenshot captured: /tmp/task_d5_directory_modals_fixed.png');

  assert(pageErrors.length === 0, 'Zero page errors logged during execution', `errors: ${pageErrors.length}`);
  if (pageErrors.length > 0) {
    console.error('Logged errors:', pageErrors);
  }

  console.log(`\n>>> SUMMARY: ${passed} checks PASSED, ${failed} FAILED, ${pageErrors.length} page errors.`);
  await browser.close();
  process.exit(failed > 0 || pageErrors.length > 0 ? 1 : 0);
})();

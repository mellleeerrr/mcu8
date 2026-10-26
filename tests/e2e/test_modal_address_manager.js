/**
 * Empirical E2E Verification Test for "Добавить участника по адресу" Modal
 * Covers: Equipment buttons, Cascading radiobuttons, Codec checkboxes, Warning icons,
 * Autocomplete items, Selects, and Modal surfaces.
 */

const { chromium } = require('playwright');

(async () => {
  console.log('>>> [Modal Address Manager E2E] Starting test...');
  const browser = await chromium.launch({
    headless: true,
    args: ['--no-sandbox', '--disable-setuid-sandbox']
  });
  const page = await browser.newPage();

  const errors = [];
  page.on('pageerror', err => errors.push(err.message));

  let passed = 0;
  let failed = 0;
  function assert(name, condition, actual, expected) {
    if (condition) {
      console.log(`  [PASS] ${name} ${actual ? '(' + actual + ')' : ''}`);
      passed++;
    } else {
      console.error(`  [FAIL] ${name} | Expected: ${expected} | Actual: ${actual}`);
      failed++;
    }
  }

  try {
    // 1. Authenticate
    await page.goto('http://127.0.0.1:8080/pnm/authorization', { waitUntil: 'networkidle' });
    await page.fill('input[type="text"]', 'admin');
    await page.fill('input[type="password"]', 'admin_password');
    await page.click('[data-qa="authorization-sign-in-button"]');
    await page.waitForNavigation({ waitUntil: 'networkidle' });

    // Ensure dark theme
    const isDark = await page.evaluate(() => document.documentElement.classList.contains('theme-dark'));
    if (!isDark) {
      await page.click('#vks-theme-toggle-btn');
      await page.waitForTimeout(300);
    }

    // 2. Open first conference details
    await page.waitForSelector('.main-list-item');
    await page.click('.main-list-item');
    await page.waitForTimeout(500);

    // 3. Click "Добавить участника по адресу" button
    // Selector in conference participants: button or icon with address book / add address
    const addAddressBtn = await page.$('[data-qa="conference-participants-add-by-address"], button[title*="адресу"], .tc-button[title*="адресу"], [title="Добавить участника по адресу"]');
    if (addAddressBtn) {
      await addAddressBtn.click();
    } else {
      // Fallback: look for button inside participant tools
      const btns = await page.$$('.tc-button');
      let clicked = false;
      for (const b of btns) {
        const title = await b.getAttribute('title');
        if (title && title.includes('адресу')) {
          await b.click();
          clicked = true;
          break;
        }
      }
      if (!clicked) {
        console.log('  [INFO] Clicking through DOM trigger for address modal');
        await page.evaluate(() => {
          const btn = Array.from(document.querySelectorAll('button, .tc-button')).find(el => (el.title && el.title.includes('адресу')) || el.textContent.includes('адресу'));
          if (btn) btn.click();
        });
      }
    }
    await page.waitForTimeout(600);

    // 4. Verify modal existence
    const modalExists = await page.$('.tc-modal');
    assert('Modal .tc-modal is mounted', !!modalExists, !!modalExists, 'true');

    if (modalExists) {
      // 4.1 Modal Background
      const modalBg = await page.$eval('.tc-modal', el => window.getComputedStyle(el).backgroundColor);
      assert('Modal background is elevated #242F3D (--c-surface-card)', modalBg === 'rgb(36, 47, 61)', modalBg, 'rgb(36, 47, 61)');

      // 4.2 Modal Title
      const titleColor = await page.$eval('.tc-modal__header-desc', el => window.getComputedStyle(el).color);
      assert('Modal header desc is white #FFFFFF', titleColor === 'rgb(255, 255, 255)', titleColor, 'rgb(255, 255, 255)');

      // 4.3 Outlined Equipment Buttons
      const eqBtns = await page.$$('.address-manager-general__button .tc-button.mdc-button--outlined');
      assert('Found equipment buttons', eqBtns.length > 0, eqBtns.length, '> 0');
      if (eqBtns.length > 0) {
        const btnBg = await eqBtns[0].evaluate(el => window.getComputedStyle(el).backgroundColor);
        const btnBorder = await eqBtns[0].evaluate(el => window.getComputedStyle(el).borderColor);
        const iconFill = await eqBtns[0].evaluate(el => {
          const svg = el.querySelector('svg');
          return svg ? window.getComputedStyle(svg).fill : null;
        });
        assert('Equipment button bg is surface-hover #202B36', btnBg === 'rgb(32, 43, 54)', btnBg, 'rgb(32, 43, 54)');
        assert('Equipment button border is #243342', btnBorder === 'rgb(36, 51, 66)', btnBorder, 'rgb(36, 51, 66)');
        assert('Equipment button icon fill is cyan #A5D5EE', iconFill === 'rgb(165, 213, 238)', iconFill, 'rgb(165, 213, 238)');
      }

      // 4.4 Cascading Radiobuttons
      const radioContent = await page.$('.radiobutton-content, .address-manager-general__cascade');
      assert('Radiobutton content container exists', !!radioContent, !!radioContent, 'true');
      if (radioContent) {
        const activeItem = await page.$('.radiobutton-item--active .tc-button');
        if (activeItem) {
          const activeBg = await activeItem.evaluate(el => window.getComputedStyle(el).backgroundColor);
          const activeColor = await activeItem.evaluate(el => {
            const lbl = el.querySelector('.mdc-button__label, .tc-button__text');
            return lbl ? window.getComputedStyle(lbl).color : null;
          });
          assert('Active radiobutton bg is #2B5278 (--c-active)', activeBg === 'rgb(43, 82, 120)', activeBg, 'rgb(43, 82, 120)');
          assert('Active radiobutton text is white #FFFFFF', activeColor === 'rgb(255, 255, 255)', activeColor, 'rgb(255, 255, 255)');
        }

        const inactiveItem = await page.$('.radiobutton-item:not(.radiobutton-item--active) .tc-button');
        if (inactiveItem) {
          const inactiveColor = await inactiveItem.evaluate(el => {
            const lbl = el.querySelector('.mdc-button__label, .tc-button__text');
            return lbl ? window.getComputedStyle(lbl).color : null;
          });
          assert('Inactive radiobutton text is muted #7F8B99 (NOT black)', inactiveColor === 'rgb(127, 139, 153)', inactiveColor, 'rgb(127, 139, 153)');
        }
      }

      // 4.5 Switch to "Дополнительно" tab
      const tabs = await page.$$('.tc-modal .tc-tab');
      if (tabs.length > 1) {
        await tabs[1].click();
        await page.waitForTimeout(400);

        // Check codec checkboxes and warning icons
        const cbLabels = await page.$$('.tc-modal .tc-checkbox__text');
        assert('Found codec checkboxes in "Дополнительно"', cbLabels.length > 0, cbLabels.length, '> 0');

        const warningIcons = await page.$$('.checkbox-form__checkbox-icon');
        assert('Found codec warning icons', warningIcons.length > 0, warningIcons.length, '> 0');
        if (warningIcons.length > 0) {
          const iconColor = await warningIcons[0].evaluate(el => window.getComputedStyle(el.querySelector('.tc-icon__icon') || el).color);
          const pathFill = await warningIcons[0].evaluate(el => {
            const p = el.querySelector('path');
            return p ? window.getComputedStyle(p).fill : null;
          });
          assert('Codec warning icon color is amber #FF9800', iconColor === 'rgb(255, 152, 0)', iconColor, 'rgb(255, 152, 0)');
          assert('Codec warning path fill is amber #FF9800 (overrides inline black)', pathFill === 'rgb(255, 152, 0)', pathFill, 'rgb(255, 152, 0)');
        }
      }

      // 4.6 Modal Footer buttons
      const cancelBtn = await page.$('.tc-modal__footer button[data-qa="base-popup-cancel-button"], .tc-modal__footer .tc-button-container--base button');
      if (cancelBtn) {
        const cancelColor = await cancelBtn.evaluate(el => window.getComputedStyle(el).color);
        assert('Modal cancel button text is muted #7F8B99', cancelColor === 'rgb(127, 139, 153)', cancelColor, 'rgb(127, 139, 153)');
      }

      const saveBtn = await page.$('.tc-modal__footer button:disabled, .tc-modal__footer button[disabled]');
      if (saveBtn) {
        const saveOpacity = await saveBtn.evaluate(el => window.getComputedStyle(el).opacity);
        assert('Disabled save button opacity is 0.6', parseFloat(saveOpacity) <= 0.65, saveOpacity, '<= 0.65');
      }
    }

    assert('Zero uncaught page errors during modal execution', errors.length === 0, errors.length, '0');
  } catch (err) {
    console.error('Test execution error:', err);
    failed++;
  } finally {
    await browser.close();
  }

  console.log(`\n>>> [Modal Address Manager SUMMARY]: ${passed} PASSED, ${failed} FAILED.`);
  process.exit(failed > 0 ? 1 : 0);
})();

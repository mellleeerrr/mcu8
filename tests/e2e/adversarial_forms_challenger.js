const { chromium } = require('playwright');
const path = require('path');

const BASE_URL = process.env.VKS_BASE_URL || 'http://127.0.0.1:8080';

async function runAdversarialAudit() {
  console.log('================================================================');
  console.log('>>> [M1 CHALLENGER 2] EMPIRICAL ADVERSARIAL FORMS & REVERSIBILITY');
  console.log('================================================================');

  const browser = await chromium.launch({
    headless: true,
    args: ['--no-sandbox', '--disable-setuid-sandbox', '--disable-dev-shm-usage', '--force-color-profile=srgb']
  });

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
    console.error('[PAGE ERROR]:', err.message);
    pageErrors.push(err.message);
  });

  const results = {
    passed: 0,
    failed: 0,
    details: []
  };

  function assert(condition, testName, actualValue = '', expectedValue = '') {
    if (condition) {
      console.log(`  [PASS] ${testName} ${actualValue ? '-> ' + actualValue : ''}`);
      results.passed++;
      results.details.push({ test: testName, status: 'PASS', actual: actualValue });
    } else {
      console.error(`  [FAIL] ${testName} | Expected: ${expectedValue} | Actual: ${actualValue}`);
      results.failed++;
      results.details.push({ test: testName, status: 'FAIL', actual: actualValue, expected: expectedValue });
    }
  }

  function rgbLuminance(rgbStr) {
    const m = rgbStr.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
    if (!m) return 0;
    return 0.299 * parseInt(m[1]) + 0.587 * parseInt(m[2]) + 0.114 * parseInt(m[3]);
  }

  // -------------------------------------------------------------------------
  // SUITE 1: Form Inputs & Floating Labels (Empty, Populated, Focus, Error)
  // -------------------------------------------------------------------------
  console.log('\n--- SUITE 1: Form Inputs & Floating Labels ---');
  await page.goto(`${BASE_URL}/pnm/authorization`, { waitUntil: 'networkidle' });
  await page.waitForSelector('input[type="text"]', { timeout: 10000 });

  const loginInput = page.locator('input[type="text"]').first();
  const passInput = page.locator('input[type="password"]').first();
  const loginLabel = page.locator('.tc-text-field__label, .mdc-floating-label').first();
  const passLabel = page.locator('.tc-text-field__label, .mdc-floating-label').nth(1);

  // 1.1 Empty floating label color (unfocused)
  const emptyLabelColor = await passLabel.evaluate(el => window.getComputedStyle(el).color);
  assert(emptyLabelColor === 'rgb(127, 139, 153)', 'Unfocused empty floating label is #7F8B99', emptyLabelColor, 'rgb(127, 139, 153)');

  // 1.2 Unfocused notched outline border
  const unfocusedLeading = page.locator('.tc-text-field:not(.mdc-text-field--focused) :is(.mdc-notched-outline__leading, .tc-notched-outline__leading)').first();
  const unfocusedBorder = await unfocusedLeading.evaluate(el => window.getComputedStyle(el).borderColor);
  assert(unfocusedBorder === 'rgb(36, 51, 66)', 'Unfocused outline border is #243342', unfocusedBorder, 'rgb(36, 51, 66)');

  // 1.3 Focus state on empty input -> floating label becomes accent cyan
  await loginInput.focus();
  await page.waitForTimeout(250);
  const focusedLabelColor = await loginLabel.evaluate(el => window.getComputedStyle(el).color);
  assert(focusedLabelColor === 'rgb(42, 171, 238)', 'Focused floating label is accent #2AABEE', focusedLabelColor, 'rgb(42, 171, 238)');

  // 1.4 Focus state on notched outline -> leading border becomes accent cyan
  const focusedLeading = page.locator('.mdc-text-field--focused :is(.mdc-notched-outline__leading, .tc-notched-outline__leading)').first();
  const focusedLeadingBorder = await focusedLeading.evaluate(el => window.getComputedStyle(el).borderColor);
  assert(focusedLeadingBorder === 'rgb(42, 171, 238)', 'Focused notched outline leading is #2AABEE', focusedLeadingBorder, 'rgb(42, 171, 238)');

  // 1.5 Populated input (blurred) -> floating label remains muted #7F8B99, NOT black
  await loginInput.fill('admin');
  await passInput.focus();
  await page.waitForTimeout(250);
  const populatedBlurredLabelColor = await loginLabel.evaluate(el => window.getComputedStyle(el).color);
  assert(populatedBlurredLabelColor === 'rgb(127, 139, 153)', 'Populated blurred label is muted #7F8B99 (NOT black)', populatedBlurredLabelColor, 'rgb(127, 139, 153)');

  // 1.6 Input text color is white #FFFFFF
  const inputTextColor = await loginInput.evaluate(el => window.getComputedStyle(el).color);
  assert(inputTextColor === 'rgb(255, 255, 255)', 'Input text color is #FFFFFF', inputTextColor, 'rgb(255, 255, 255)');

  // 1.7 Validation Error State (Trigger error and test blurred vs focused)
  console.log('\n--- SUITE 1.7: Adversarial Validation Error & Focus Collision ---');
  // Trigger auth validation error by submitting empty fields or invalid password
  await loginInput.fill('invalid_user_trigger_error');
  await passInput.fill('wrong_pwd');
  await page.click('[data-qa="authorization-sign-in-button"]');
  await page.waitForTimeout(1000);

  // Now log in with real admin credentials to test authenticated form error states
  await loginInput.fill('admin');
  await passInput.fill('admin_password');
  await page.click('[data-qa="authorization-sign-in-button"]');
  await page.waitForSelector('.main-large-header', { timeout: 10000 });

  // Navigate to Addressbook Users to test Add User form modal validation errors
  await page.goto(`${BASE_URL}/pnm/addressbook/users`, { waitUntil: 'networkidle' });
  await page.waitForTimeout(600);

  const addUserBtn = page.locator('[data-qa="user-list-tools-add-button"], .main-list-tools__add-button, button:has-text("Добавить пользователя")').first();
  let modalOpened = false;
  if (await addUserBtn.count() > 0) {
    await addUserBtn.click();
    await page.waitForTimeout(600);
    modalOpened = await page.locator('.tc-modal, .mdc-dialog').count() > 0;
  }
  assert(modalOpened, 'Add user modal opened successfully');

  if (modalOpened) {
    // In Add User modal, find name and login inputs
    const modalInputs = page.locator('.tc-modal input, .mdc-dialog input');
    const inputCount = await modalInputs.count();
    assert(inputCount >= 1, `Add user modal contains ${inputCount} form inputs`);

    // Find the save/submit button in modal
    const saveBtn = page.locator('.tc-modal__footer button:has-text("Сохранить"), .mdc-dialog__actions button:has-text("Сохранить"), button:has-text("Добавить")').first();
    if (await saveBtn.count() > 0) {
      await saveBtn.click({ force: true }).catch(() => {});
      await page.waitForTimeout(400);

      // Check if error state was triggered
      const errorOutline = page.locator(':is(.tc-text-field--error, .mdc-text-field--invalid, .tc-select-container--error) :is(.mdc-notched-outline__leading, .tc-notched-outline__leading)').first();
      const errorLabel = page.locator(':is(.tc-text-field--error, .mdc-text-field--invalid, .tc-select-container--error) :is(.mdc-floating-label, .tc-floating-label)').first();

      if (await errorOutline.count() > 0) {
        const errOutlineBorder = await errorOutline.evaluate(el => window.getComputedStyle(el).borderColor);
        assert(errOutlineBorder === 'rgb(240, 67, 67)', 'Blurred error outline border is red #F04343', errOutlineBorder, 'rgb(240, 67, 67)');

        if (await errorLabel.count() > 0) {
          const errLabelColor = await errorLabel.evaluate(el => window.getComputedStyle(el).color);
          assert(errLabelColor === 'rgb(240, 67, 67)', 'Blurred error label color is red #F04343', errLabelColor, 'rgb(240, 67, 67)');
        }

        // ADVERSARIAL STRESS TEST: FOCUS COLLISION ON ERROR INPUT
        // When user focuses the invalid field, does it keep red #F04343 or turn cyan #2AABEE?
        const errorInput = page.locator(':is(.tc-text-field--error, .mdc-text-field--invalid) input').first();
        if (await errorInput.count() > 0) {
          await errorInput.focus();
          await page.waitForTimeout(300);

          const focusedErrOutlineBorder = await errorOutline.evaluate(el => window.getComputedStyle(el).borderColor);
          assert(focusedErrOutlineBorder === 'rgb(240, 67, 67)', 'FOCUSED error outline border REMAINS red #F04343 (NO cyan override)', focusedErrOutlineBorder, 'rgb(240, 67, 67)');

          if (await errorLabel.count() > 0) {
            const focusedErrLabelColor = await errorLabel.evaluate(el => window.getComputedStyle(el).color);
            assert(focusedErrLabelColor === 'rgb(240, 67, 67)', 'FOCUSED error label REMAINS red #F04343 (NO cyan override)', focusedErrLabelColor, 'rgb(240, 67, 67)');
          }
        }
      } else {
        console.log('  [INFO] No immediate error outline triggered on save click; testing programmatic validation class');
        // Let's test on the input directly by toggling invalid class
        const firstInputContainer = page.locator('.tc-modal .tc-text-field, .tc-modal .mdc-text-field').first();
        if (await firstInputContainer.count() > 0) {
          await firstInputContainer.evaluate(el => el.classList.add('tc-text-field--error', 'mdc-text-field--invalid'));
          await page.waitForTimeout(300);
          const injectedErrBorder = await firstInputContainer.locator('.mdc-notched-outline__leading, .tc-notched-outline__leading').evaluate(el => window.getComputedStyle(el).borderColor);
          assert(injectedErrBorder === 'rgb(240, 67, 67)', 'Synthetic error notched outline is red #F04343', injectedErrBorder, 'rgb(240, 67, 67)');

          // Focus it
          await firstInputContainer.locator('input').focus();
          await page.waitForTimeout(300);
          const focusedInjectedBorder = await firstInputContainer.locator('.mdc-notched-outline__leading, .tc-notched-outline__leading').evaluate(el => window.getComputedStyle(el).borderColor);
          assert(focusedInjectedBorder === 'rgb(240, 67, 67)', 'Synthetic FOCUSED error notched outline remains red #F04343', focusedInjectedBorder, 'rgb(240, 67, 67)');
        }
      }
    }

    // Close modal
    const cancelModalBtn = page.locator('.tc-modal__footer button:has-text("Отмена"), .mdc-dialog__actions button:has-text("Отмена"), .tc-modal__close').first();
    if (await cancelModalBtn.count() > 0) {
      await cancelModalBtn.click();
      await page.waitForTimeout(400);
    }
  }

  // -------------------------------------------------------------------------
  // SUITE 2: Select Dropdowns, Open Menus, Hover States
  // -------------------------------------------------------------------------
  console.log('\n--- SUITE 2: Select Dropdowns, Open Menus, Option Hover States ---');
  await page.goto(`${BASE_URL}/pnm/monitoring/conferences`, { waitUntil: 'networkidle' });
  await page.waitForSelector('.monitoring-screen, .main-list', { timeout: 10000 });
  await page.waitForTimeout(600);

  // Click first conference to inspect details form
  const firstConf = page.locator('.main-list-item').first();
  if (await firstConf.count() > 0) {
    await firstConf.click();
    await page.waitForTimeout(600);
    const mainTab = page.locator('.mdc-tab, .tc-tab').filter({ hasText: 'Основное' }).first();
    if (await mainTab.count() > 0) {
      await mainTab.click();
      await page.waitForTimeout(600);
    }
  }

  // Find a select dropdown
  const selectElem = page.locator('.tc-select, .mdc-select').first();
  const selectCount = await selectElem.count();
  assert(selectCount > 0, `Found ${selectCount} select component(s) on conference form`);

  if (selectCount > 0) {
    // 2.1 Closed select value text contrast
    const selectValue = page.locator('.tc-select .tc-text-field__input--readonly-special, .tc-select-container input, .mdc-select__selected-text').first();
    const selectValColor = await selectValue.evaluate(el => window.getComputedStyle(el).color);
    assert(selectValColor === 'rgb(255, 255, 255)', 'Select value text color is #FFFFFF', selectValColor, 'rgb(255, 255, 255)');

    // 2.2 Select chevron icon cyan accent
    const selectChevron = page.locator('.tc-select :is(.tc-text-field__postfix-container, .tc-select__postfix) :is(.tc-icon__icon, .material-icons, svg)').first();
    if (await selectChevron.count() > 0) {
      const chevronColor = await selectChevron.evaluate(el => window.getComputedStyle(el).color || window.getComputedStyle(el).fill);
      assert(chevronColor === 'rgb(165, 213, 238)', 'Select chevron is cyan #A5D5EE', chevronColor, 'rgb(165, 213, 238)');
    }

    // 2.3 Open Select Dropdown Menu
    const selectTrigger = selectElem.locator(':is(.tc-text-field__postfix-container, .tc-select__postfix, .tc-icon)').first();
    if (await selectTrigger.count() > 0) {
      await selectTrigger.click();
    } else {
      await selectElem.click();
    }
    await page.waitForTimeout(500);

    const openMenu = page.locator('.mdc-menu-surface--open').first();
    const isMenuOpen = await openMenu.count() > 0;
    assert(isMenuOpen, 'Select dropdown menu opened successfully');

    if (isMenuOpen) {
      // 2.4 Menu background and border
      const menuBg = await openMenu.evaluate(el => window.getComputedStyle(el).backgroundColor);
      const menuBorder = await openMenu.evaluate(el => window.getComputedStyle(el).borderColor);
      assert(menuBg === 'rgb(36, 47, 61)', 'Open menu background is #242F3D (--c-surface-card)', menuBg, 'rgb(36, 47, 61)');
      assert(menuBorder === 'rgb(36, 51, 66)', 'Open menu border is #243342 (--c-border)', menuBorder, 'rgb(36, 51, 66)');

      // 2.5 Option items contrast and hover state
      const menuOptions = openMenu.locator('.tc-context-menu-item, .mdc-deprecated-list-item, .mdc-list-item, .tc-select__option, [role="option"], li');
      const optCount = await menuOptions.count();
      assert(optCount > 0, `Open menu contains ${optCount} option items`);

      if (optCount > 0) {
        const firstOpt = menuOptions.first();
        const optColor = await firstOpt.evaluate(el => window.getComputedStyle(el).color);
        assert(optColor === 'rgb(255, 255, 255)', 'Menu option text color is #FFFFFF', optColor, 'rgb(255, 255, 255)');

        // Hover over option
        await firstOpt.hover();
        await page.waitForTimeout(200);
        const hoveredOptBg = await firstOpt.evaluate(el => window.getComputedStyle(el).backgroundColor);
        assert(hoveredOptBg === 'rgb(32, 43, 54)', 'Menu option hover background is #202B36 (--c-surface-hover)', hoveredOptBg, 'rgb(32, 43, 54)');
      }

      // Close menu by clicking outside or pressing Escape
      await page.keyboard.press('Escape');
      await page.waitForTimeout(300);
    }
  }

  // -------------------------------------------------------------------------
  // SUITE 3: Disabled Buttons and Disabled Form Controls
  // -------------------------------------------------------------------------
  console.log('\n--- SUITE 3: Disabled Buttons & Inputs (Opacity 0.6, No Dark-on-Dark) ---');
  await page.goto(`${BASE_URL}/pnm/administration/date_time`, { waitUntil: 'networkidle' });
  await page.waitForSelector('.administration-screen', { timeout: 10000 });
  await page.waitForTimeout(500);

  // 3.1 Inspect existing disabled elements
  const disabledControls = page.locator(':is(input, textarea, select):disabled, :is(.tc-text-field--disabled, .mdc-text-field--disabled, .tc-select--disabled, .tc-select-container--disabled)');
  const disabledCount = await disabledControls.count();
  console.log(`Found ${disabledCount} disabled input controls on date_time screen`);

  if (disabledCount > 0) {
    const firstDisabled = disabledControls.first();
    const opacity = await firstDisabled.evaluate(el => parseFloat(window.getComputedStyle(el).opacity));
    assert(opacity >= 0.55 && opacity <= 0.65, 'Disabled control container opacity is ~0.6', `opacity: ${opacity}`, '0.6');

    const cursor = await firstDisabled.evaluate(el => window.getComputedStyle(el).cursor);
    assert(cursor === 'not-allowed', 'Disabled control cursor is not-allowed', cursor, 'not-allowed');

    const childInput = firstDisabled.locator('input, textarea, select, .tc-text-field__input').first();
    if (await childInput.count() > 0) {
      const childColor = await childInput.evaluate(el => window.getComputedStyle(el).color);
      assert(childColor === 'rgb(127, 139, 153)', 'Disabled input child color is muted #7F8B99 (NOT black)', childColor, 'rgb(127, 139, 153)');

      // Verify NO compound opacity collapse
      const childComputedOpacity = await childInput.evaluate(el => parseFloat(window.getComputedStyle(el).opacity));
      assert(childComputedOpacity >= 0.9, 'Disabled child input does NOT apply redundant opacity (NO compound 0.36 collapse)', `child opacity: ${childComputedOpacity}`, '1');
    }
  }

  // 3.2 Dynamic Adversarial Stress Test: Disabled Button Matrix
  console.log('\n--- SUITE 3.2: Adversarial Disabled Button Matrix ---');
  const disabledButtonAudit = await page.evaluate(() => {
    // Create temporary test container with various button types to evaluate styling under html.theme-dark
    const container = document.createElement('div');
    container.id = 'adv-disabled-test-container';
    container.style.position = 'absolute';
    container.style.top = '0';
    container.style.left = '0';
    container.style.zIndex = '99999';
    container.style.visibility = 'hidden';

    // 1. Raised primary button disabled
    const btn1 = document.createElement('button');
    btn1.className = 'tc-button mdc-button mdc-button--unelevated tc-button--primary';
    btn1.disabled = true;
    btn1.innerHTML = '<span class="mdc-button__label tc-button__text">Disabled Primary</span>';
    container.appendChild(btn1);

    // 2. Outlined button disabled
    const btn2 = document.createElement('button');
    btn2.className = 'tc-button mdc-button mdc-button--outlined';
    btn2.disabled = true;
    btn2.innerHTML = '<span class="mdc-button__label tc-button__text">Disabled Outlined</span>';
    container.appendChild(btn2);

    // 3. Text button with icon disabled
    const btn3 = document.createElement('button');
    btn3.className = 'tc-button mdc-button mdc-button--text';
    btn3.disabled = true;
    btn3.innerHTML = '<i class="material-icons tc-button__icon">settings</i><span class="mdc-button__label tc-button__text">Disabled Text</span>';
    container.appendChild(btn3);

    document.body.appendChild(container);

    const s1 = window.getComputedStyle(btn1);
    const s1Label = window.getComputedStyle(btn1.querySelector('.tc-button__text'));
    const s2 = window.getComputedStyle(btn2);
    const s3 = window.getComputedStyle(btn3);
    const s3Icon = window.getComputedStyle(btn3.querySelector('.material-icons'));

    const result = {
      btn1: { opacity: s1.opacity, cursor: s1.cursor, bg: s1.backgroundColor, labelColor: s1Label.color },
      btn2: { opacity: s2.opacity, cursor: s2.cursor },
      btn3: { opacity: s3.opacity, cursor: s3.cursor, iconColor: s3Icon.color, iconFill: s3Icon.fill }
    };

    container.remove();
    return result;
  });

  assert(parseFloat(disabledButtonAudit.btn1.opacity) === 0.6, 'Disabled primary button opacity is 0.6', disabledButtonAudit.btn1.opacity, '0.6');
  assert(disabledButtonAudit.btn1.cursor === 'not-allowed', 'Disabled primary button cursor is not-allowed', disabledButtonAudit.btn1.cursor, 'not-allowed');
  assert(disabledButtonAudit.btn1.labelColor === 'rgb(127, 139, 153)', 'Disabled primary button text is #7F8B99 (NOT black or white bleed)', disabledButtonAudit.btn1.labelColor, 'rgb(127, 139, 153)');
  assert(disabledButtonAudit.btn1.bg === 'rgba(255, 255, 255, 0.12)', 'Disabled raised button background is rgba(255, 255, 255, 0.12)', disabledButtonAudit.btn1.bg, 'rgba(255, 255, 255, 0.12)');
  assert(disabledButtonAudit.btn3.iconColor === 'rgb(127, 139, 153)', 'Disabled button icon is #7F8B99 (NO cyan override)', disabledButtonAudit.btn3.iconColor, 'rgb(127, 139, 153)');

  // -------------------------------------------------------------------------
  // SUITE 4: Bidirectional Theme Switching Reversibility
  // -------------------------------------------------------------------------
  console.log('\n--- SUITE 4: Bidirectional Multi-Cycle Theme Switching ---');
  await page.goto(`${BASE_URL}/pnm/monitoring/conferences`, { waitUntil: 'networkidle' });
  await page.waitForSelector('.main-large-header', { timeout: 10000 });

  const toggleBtn = page.locator('#vks-theme-toggle-btn');
  assert(await toggleBtn.count() > 0, 'Found theme toggle button #vks-theme-toggle-btn');

  // Cycle Dark -> Light -> Dark -> Light -> Dark (3 full cycles, 6 transitions)
  for (let cycle = 1; cycle <= 3; cycle++) {
    console.log(`\n  >> Theme Switch Cycle ${cycle}: Switching to LIGHT theme...`);
    await toggleBtn.click();
    await page.waitForTimeout(600);

    const isLightClass = await page.evaluate(() => document.documentElement.classList.contains('theme-light') && !document.documentElement.classList.contains('theme-dark'));
    assert(isLightClass, `[Cycle ${cycle}] html has theme-light and NOT theme-dark`);

    // Audit Light Theme surfaces: verify ZERO residual dark styles
    const lightSurfacesAudit = await page.evaluate(() => {
      const bodyBg = window.getComputedStyle(document.body).backgroundColor;
      const headerBg = window.getComputedStyle(document.querySelector('.main-large-header')).backgroundColor;
      const cEl = document.querySelector('.main-card') || document.querySelector('.main-list-item') || document.querySelector('.main-list');
      let cardBg = cEl ? window.getComputedStyle(cEl).backgroundColor : bodyBg;
      if (cardBg === 'rgba(0, 0, 0, 0)' || cardBg === 'transparent') cardBg = bodyBg;
      const textColor = window.getComputedStyle(document.querySelector('.main-card-header__title') || document.querySelector('.main-large-header__title') || document.body).color;

      return { bodyBg, headerBg, cardBg, textColor };
    });

    const bodyBgLum = rgbLuminance(lightSurfacesAudit.bodyBg);
    const headerBgLum = rgbLuminance(lightSurfacesAudit.headerBg);
    const cardBgLum = rgbLuminance(lightSurfacesAudit.cardBg);
    const textLum = rgbLuminance(lightSurfacesAudit.textColor);

    assert(bodyBgLum > 180, `[Cycle ${cycle}] Light body background is bright (lum: ${bodyBgLum.toFixed(1)})`, lightSurfacesAudit.bodyBg);
    assert(headerBgLum > 180, `[Cycle ${cycle}] Light header background is bright (lum: ${headerBgLum.toFixed(1)})`, lightSurfacesAudit.headerBg);
    assert(cardBgLum > 180, `[Cycle ${cycle}] Light card background is bright (lum: ${cardBgLum.toFixed(1)})`, lightSurfacesAudit.cardBg);
    assert(textLum < 100, `[Cycle ${cycle}] Light theme text is dark readable (lum: ${textLum.toFixed(1)})`, lightSurfacesAudit.textColor);

    console.log(`  >> Theme Switch Cycle ${cycle}: Switching BACK to DARK theme...`);
    await toggleBtn.click();
    await page.waitForTimeout(600);

    const isDarkClass = await page.evaluate(() => document.documentElement.classList.contains('theme-dark') && !document.documentElement.classList.contains('theme-light'));
    assert(isDarkClass, `[Cycle ${cycle}] html has theme-dark and NOT theme-light`);

    // Audit Dark Theme surfaces: verify ZERO residual light styles
    const darkSurfacesAudit = await page.evaluate(() => {
      const bodyBg = window.getComputedStyle(document.body).backgroundColor;
      const headerBg = window.getComputedStyle(document.querySelector('.main-large-header')).backgroundColor;
      const cardBg = window.getComputedStyle(document.querySelector('.main-card') || document.querySelector('.main-list')).backgroundColor;
      const textColor = window.getComputedStyle(document.querySelector('.main-card-header__title') || document.querySelector('.main-large-header__title') || document.body).color;

      return { bodyBg, headerBg, cardBg, textColor };
    });

    const darkBodyBgLum = rgbLuminance(darkSurfacesAudit.bodyBg);
    const darkHeaderBgLum = rgbLuminance(darkSurfacesAudit.headerBg);
    const darkCardBgLum = rgbLuminance(darkSurfacesAudit.cardBg);
    const darkTextLum = rgbLuminance(darkSurfacesAudit.textColor);

    assert(darkBodyBgLum < 40, `[Cycle ${cycle}] Dark body background is dark (lum: ${darkBodyBgLum.toFixed(1)})`, darkSurfacesAudit.bodyBg);
    assert(darkHeaderBgLum < 40, `[Cycle ${cycle}] Dark header background is dark (lum: ${darkHeaderBgLum.toFixed(1)})`, darkSurfacesAudit.headerBg);
    assert(darkCardBgLum < 50, `[Cycle ${cycle}] Dark card background is dark (lum: ${darkCardBgLum.toFixed(1)})`, darkSurfacesAudit.cardBg);
    assert(darkTextLum > 200, `[Cycle ${cycle}] Dark text is white readable (lum: ${darkTextLum.toFixed(1)})`, darkSurfacesAudit.textColor);
  }

  // -------------------------------------------------------------------------
  // SUITE 5: Zero White SVG Bounding Boxes & Zero Yellow Autofill
  // -------------------------------------------------------------------------
  console.log('\n--- SUITE 5: SVG Bounding Boxes & Autofill Overlays ---');
  
  // 5.1 SVG Bounding Boxes
  const svgAudit = await page.evaluate(() => {
    const allSvgs = Array.from(document.querySelectorAll('svg'));
    const offendingRects = [];
    for (const svg of allSvgs) {
      const rects = Array.from(svg.querySelectorAll('rect'));
      for (const r of rects) {
        const fillAttr = r.getAttribute('fill') || '';
        const computedFill = window.getComputedStyle(r).fill;
        const computedBg = window.getComputedStyle(r).backgroundColor;
        const w = r.getAttribute('width');
        const h = r.getAttribute('height');

        // Check if bounding rect renders solid white
        if (fillAttr.toLowerCase() === '#fff' || fillAttr.toLowerCase() === '#ffffff' || fillAttr.toLowerCase() === 'white') {
          if (computedFill === 'rgb(255, 255, 255)') {
            offendingRects.push({
              svgId: svg.id || svg.className.baseVal,
              rectId: r.id,
              width: w,
              height: h,
              computedFill,
              fillAttr
            });
          }
        }
      }
    }
    return {
      totalSvgs: allSvgs.length,
      offendingCount: offendingRects.length,
      offendingRects
    };
  });

  assert(svgAudit.offendingCount === 0, `Zero white SVG bounding boxes (scanned ${svgAudit.totalSvgs} SVGs)`, `violations: ${svgAudit.offendingCount}`, '0');
  if (svgAudit.offendingCount > 0) {
    console.error('Offending SVG rects:', JSON.stringify(svgAudit.offendingRects, null, 2));
  }

  // 5.2 Browser Autofill Rules & Live Simulation
  const autofillAudit = await page.evaluate(() => {
    // 1. Check stylesheet rules
    let hasDarkAutofillRule = false;
    let autofillBoxShadow = '';
    let autofillTextColor = '';

    for (const sheet of document.styleSheets) {
      try {
        for (const rule of sheet.cssRules) {
          if (rule.selectorText && (rule.selectorText.includes(':-webkit-autofill') || rule.selectorText.includes(':autofill'))) {
            hasDarkAutofillRule = true;
            autofillBoxShadow = rule.style.boxShadow || rule.style.getPropertyValue('-webkit-box-shadow');
            autofillTextColor = rule.style.getPropertyValue('-webkit-text-fill-color');
          }
        }
      } catch (_) {}
    }

    // 2. Synthetic autofill simulation
    const testInput = document.createElement('input');
    testInput.type = 'text';
    testInput.id = 'adv-autofill-test';
    testInput.style.position = 'absolute';
    testInput.style.top = '0';
    testInput.style.left = '0';
    testInput.style.visibility = 'hidden';
    document.body.appendChild(testInput);

    // Apply :-webkit-autofill matching simulation or check stylesheet properties
    const inputStyle = window.getComputedStyle(testInput);
    testInput.remove();

    return {
      hasDarkAutofillRule,
      autofillBoxShadow,
      autofillTextColor
    };
  });

  assert(autofillAudit.hasDarkAutofillRule, 'Dark autofill rule exists in stylesheets');
  assert(autofillAudit.autofillBoxShadow.includes('1000px') && autofillAudit.autofillBoxShadow.includes('inset'), 'Autofill box shadow is 1000px inset', autofillAudit.autofillBoxShadow);
  assert(autofillAudit.autofillTextColor === 'var(--c-text)', 'Autofill text color is var(--c-text)', autofillAudit.autofillTextColor, 'var(--c-text)');

  await browser.close();

  console.log('\n================================================================');
  console.log(`>>> [M1 CHALLENGER 2 SUMMARY]: ${results.passed} PASSED, ${results.failed} FAILED, ${pageErrors.length} PAGE ERRORS`);
  console.log('================================================================');

  if (results.failed > 0 || pageErrors.length > 0) {
    process.exit(1);
  }
}

runAdversarialAudit().catch(err => {
  console.error('[FATAL EXCEPTION]:', err);
  process.exit(1);
});

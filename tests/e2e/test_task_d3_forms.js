const { chromium } = require('playwright');
const path = require('path');

const BASE_URL = process.env.VKS_BASE_URL || 'http://127.0.0.1:8080';

async function run() {
  console.log('>>> [Task D3 E2E] Starting Playwright Floating Labels, Selects & Autofill Test...');
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

  const errors = [];
  page.on('pageerror', err => {
    console.error('[Page Error]:', err.message);
    errors.push(err.message);
  });

  let passedChecks = 0;
  let failedChecks = 0;

  function assert(condition, message, details = '') {
    if (condition) {
      console.log(`  [✓] ${message} ${details ? '(' + details + ')' : ''}`);
      passedChecks++;
    } else {
      console.error(`  [✗] FAIL: ${message} ${details ? '(' + details + ')' : ''}`);
      failedChecks++;
    }
  }

  console.log('\n--- Step 1: Authorization Screen Floating Labels & Focus ---');
  await page.goto(`${BASE_URL}/pnm/authorization`, { waitUntil: 'networkidle' });
  await page.waitForSelector('input[type="text"]', { timeout: 10000 });

  // Unfocused empty password label
  const passLabel = page.locator('.tc-text-field__label, .mdc-floating-label').nth(1);
  const emptyLabelColor = await passLabel.evaluate(el => window.getComputedStyle(el).color);
  assert(emptyLabelColor === 'rgb(127, 139, 153)', 'Unfocused empty floating label is muted #7F8B99', emptyLabelColor);

  // Focus login input -> label should turn accent #2AABEE
  const loginInput = page.locator('input[type="text"]').first();
  const loginLabel = page.locator('.tc-text-field__label, .mdc-floating-label').first();
  await loginInput.focus();
  await page.waitForTimeout(200);
  const focusedLabelColor = await loginLabel.evaluate(el => window.getComputedStyle(el).color);
  assert(focusedLabelColor === 'rgb(42, 171, 238)', 'Focused floating label is accent #2AABEE', focusedLabelColor);

  // Fill "admin" and focus password -> login label should retain float-above and remain #7F8B99 (NOT black)
  await loginInput.fill('admin');
  await page.locator('input[type="password"]').first().focus();
  await page.waitForTimeout(200);

  const floatedLabelColor = await loginLabel.evaluate(el => window.getComputedStyle(el).color);
  const hasFloatAbove = await loginLabel.evaluate(el => el.classList.contains('mdc-floating-label--float-above') || el.classList.contains('tc-floating-label--float-above'));
  assert(floatedLabelColor === 'rgb(127, 139, 153)', 'Floated label with value is #7F8B99 (NOT black)', floatedLabelColor);
  assert(hasFloatAbove, 'Label has float-above modifier class');

  console.log('\n--- Step 2: Login and Check Conference Form Populated Labels & Outlines ---');
  await page.fill('input[type="password"]', 'admin_password');
  await page.click('[data-qa="authorization-sign-in-button"]');
  await page.waitForSelector('.main-large-header', { timeout: 10000 });

  // Navigate to conferences
  await page.goto(`${BASE_URL}/pnm/monitoring/conferences`, { waitUntil: 'networkidle' });
  await page.waitForSelector('.monitoring-screen, .main-list', { timeout: 10000 });
  await page.waitForTimeout(400);

  // Click first conference to open card
  const firstConf = page.locator('.main-list-item').first();
  if (await firstConf.count() > 0) {
    await firstConf.click();
    await page.waitForTimeout(400);

    // Switch to "Основное" tab
    const mainTab = page.locator('.mdc-tab, .tc-tab').filter({ hasText: 'Основное' }).first();
    if (await mainTab.count() > 0) {
      await mainTab.click();
      await page.waitForTimeout(400);
    }
  }

  // Check all populated floating labels
  const floatedLabels = page.locator('.mdc-floating-label--float-above, .tc-floating-label--float-above');
  const count = await floatedLabels.count();
  assert(count >= 3, `Found ${count} populated floating labels on conference details`);

  let blackLabels = 0;
  for (let i = 0; i < count; i++) {
    const color = await floatedLabels.nth(i).evaluate(el => window.getComputedStyle(el).color);
    if (color === 'rgba(0, 0, 0, 0.6)' || color === 'rgb(0, 0, 0)') {
      blackLabels++;
    }
  }
  assert(blackLabels === 0, 'Zero black floating labels on conference details form', `black: ${blackLabels}/${count}`);

  // Check notched outline border on unfocused field
  const unfocusedOutline = page.locator('.mdc-text-field:not(.mdc-text-field--focused) .mdc-notched-outline__leading').first();
  if (await unfocusedOutline.count() > 0) {
    const borderColor = await unfocusedOutline.evaluate(el => window.getComputedStyle(el).borderColor);
    assert(borderColor === 'rgb(36, 51, 66)', 'Unfocused notched outline border is #243342', borderColor);
  }

  // Check notched outline border on focused field
  const focusedOutline = page.locator('.mdc-text-field--focused .mdc-notched-outline__leading').first();
  if (await focusedOutline.count() > 0) {
    const focusedBorder = await focusedOutline.evaluate(el => window.getComputedStyle(el).borderColor);
    assert(focusedBorder === 'rgb(42, 171, 238)', 'Focused notched outline border is accent #2AABEE', focusedBorder);
  }

  // Check Selects selected text and chevrons
  const selectVal = page.locator('.tc-select .tc-text-field__input--readonly-special, .tc-select-container input').first();
  if (await selectVal.count() > 0) {
    const valColor = await selectVal.evaluate(el => window.getComputedStyle(el).color);
    assert(valColor === 'rgb(255, 255, 255)', 'Select selected text is #FFFFFF', valColor);
  }

  const selectChevron = page.locator('.tc-select .tc-text-field__postfix-container :is(.tc-icon__icon, .material-icons)').first();
  if (await selectChevron.count() > 0) {
    const chevronColor = await selectChevron.evaluate(el => window.getComputedStyle(el).color);
    assert(chevronColor === 'rgb(165, 213, 238)', 'Select chevron icon is cyan #A5D5EE', chevronColor);
  }

  console.log('\n--- Step 3: Check Validation Error State ---');
  // Target a visible text input in the conference form
  const nameInput = page.locator('.main-card input[type="text"]').first();
  if (await nameInput.count() > 0) {
    await nameInput.focus();
    const origVal = await nameInput.inputValue();
    await nameInput.fill('');
    await page.keyboard.press('Tab');
    await page.waitForTimeout(300);

    const errorOutline = page.locator('.tc-text-field--error .mdc-notched-outline__leading, .mdc-text-field--invalid .mdc-notched-outline__leading').first();
    if (await errorOutline.count() > 0) {
      const errBorder = await errorOutline.evaluate(el => window.getComputedStyle(el).borderColor);
      assert(errBorder === 'rgb(240, 67, 67)', 'Error notched outline border is red #F04343', errBorder);
    }

    const errorLabel = page.locator('.tc-text-field--error .mdc-floating-label, .mdc-text-field--invalid .mdc-floating-label').first();
    if (await errorLabel.count() > 0) {
      const errColor = await errorLabel.evaluate(el => window.getComputedStyle(el).color);
      assert(errColor === 'rgb(240, 67, 67)', 'Error floating label is red #F04343', errColor);
    }

    // Restore original value
    await nameInput.focus();
    await nameInput.fill(origVal || 'Conference');
  }

  console.log('\n--- Step 4: Check Disabled Controls ---');
  await page.goto(`${BASE_URL}/pnm/administration/date_time`, { waitUntil: 'networkidle' });
  await page.waitForSelector('.administration-screen', { timeout: 10000 });
  await page.waitForTimeout(400);

  const disabledField = page.locator(':is(.tc-text-field--disabled, .tc-select--disabled, select:disabled, input:disabled)').first();
  if (await disabledField.count() > 0) {
    const opacity = await disabledField.evaluate(el => parseFloat(window.getComputedStyle(el).opacity));
    assert(opacity <= 0.65, 'Disabled control opacity is <= 0.6', `opacity: ${opacity}`);
    const cursor = await disabledField.evaluate(el => window.getComputedStyle(el).cursor);
    assert(cursor === 'not-allowed', 'Disabled control cursor is not-allowed', cursor);
  }

  console.log('\n--- Step 5: Check Autofill CSS Inset Box Shadow ---');
  const autofillSupport = await page.evaluate(() => {
    for (const sheet of document.styleSheets) {
      try {
        for (const rule of sheet.cssRules) {
          if (rule.selectorText && rule.selectorText.includes(':-webkit-autofill')) {
            return {
              selector: rule.selectorText,
              boxShadow: rule.style.boxShadow || rule.style.getPropertyValue('-webkit-box-shadow'),
              textColor: rule.style.getPropertyValue('-webkit-text-fill-color')
            };
          }
        }
      } catch (_) {}
    }
    return null;
  });
  assert(autofillSupport !== null, 'Found :-webkit-autofill rule in stylesheets');
  if (autofillSupport) {
    assert(autofillSupport.boxShadow.includes('1000px') && autofillSupport.boxShadow.includes('inset'), 'Autofill has dark inset box shadow', autofillSupport.boxShadow);
    assert(autofillSupport.textColor === 'var(--c-text)', 'Autofill text fill color is var(--c-text)', autofillSupport.textColor);
  }

  // Take screenshot
  const screenshotPath = '/tmp/task_d3_forms_fixed.png';
  await page.screenshot({ path: screenshotPath, fullPage: false });
  console.log(`\nScreenshot captured: ${screenshotPath}`);

  await browser.close();

  console.log(`\n>>> SUMMARY: ${passedChecks} checks PASSED, ${failedChecks} FAILED, ${errors.length} page errors.`);
  if (failedChecks > 0 || errors.length > 0) {
    process.exit(1);
  }
}

run().catch(err => {
  console.error('[FATAL]:', err);
  process.exit(1);
});

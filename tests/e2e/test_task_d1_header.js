const { chromium } = require('playwright');
const path = require('path');

const BASE_URL = process.env.VKS_BASE_URL || 'http://127.0.0.1:8080';

async function run() {
  console.log('>>> [Task D1 E2E] Starting Playwright Header & Stats Hygiene Test...');
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

  console.log(`Navigating to ${BASE_URL}/pnm/authorization...`);
  await page.goto(`${BASE_URL}/pnm/authorization`, { waitUntil: 'networkidle' });

  await page.waitForSelector('input[type="text"]', { timeout: 10000 });
  console.log('Logging in as admin...');
  await page.fill('input[type="text"]', 'admin');
  await page.fill('input[type="password"]', 'admin_password');
  await page.click('[data-qa="authorization-sign-in-button"]');

  await page.waitForSelector('.main-large-header', { timeout: 10000 });
  console.log('Logged in successfully. Ensuring dark theme...');
  await page.evaluate(() => {
    if (!document.documentElement.classList.contains('theme-dark')) {
      document.documentElement.classList.add('theme-dark');
    }
  });
  await page.waitForTimeout(500);

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

  console.log('\n--- Checking 1: Active vs Inactive Navigation Buttons ---');
  const inactiveNavBtn = page.locator('.navigation-menu-button:not(.navigation-menu-button--active) .tc-button').first();
  await inactiveNavBtn.waitFor({ state: 'visible', timeout: 5000 });
  
  const inactiveNavText = await page.locator('.navigation-menu-button:not(.navigation-menu-button--active) .tc-button__text').first().evaluate(el => window.getComputedStyle(el).color);
  assert(inactiveNavText === 'rgb(127, 139, 153)', 'Inactive nav item text is muted #7F8B99', inactiveNavText);

  const inactiveNavIcon = await page.locator('.navigation-menu-button:not(.navigation-menu-button--active) :is(.tc-button__icon, .material-icons, svg)').first().evaluate(el => window.getComputedStyle(el).color);
  assert(inactiveNavIcon === 'rgb(127, 139, 153)', 'Inactive nav item icon is muted #7F8B99', inactiveNavIcon);

  const activeNavBtn = page.locator('.navigation-menu-button--active .tc-button').first();
  await activeNavBtn.waitFor({ state: 'visible', timeout: 5000 });
  
  const activeNavBg = await activeNavBtn.evaluate(el => window.getComputedStyle(el).backgroundColor);
  assert(activeNavBg === 'rgb(14, 22, 33)', 'Active nav item background is #0E1621', activeNavBg);

  const activeNavText = await page.locator('.navigation-menu-button--active .tc-button__text').first().evaluate(el => window.getComputedStyle(el).color);
  assert(activeNavText === 'rgb(255, 255, 255)', 'Active nav item text is #FFFFFF', activeNavText);

  const activeNavIcon = await page.locator('.navigation-menu-button--active :is(.tc-button__icon, .material-icons, svg)').first().evaluate(el => window.getComputedStyle(el).color);
  assert(activeNavIcon === 'rgb(255, 255, 255)', 'Active nav item icon is #FFFFFF', activeNavIcon);

  console.log('\n--- Checking 2: Server Statistics Counters & Icons ---');
  const statsCounters = page.locator('.server-stats .server-stats__item:not(:first-child) :is(.server-stats__desc, .server-stats__value, .server-stats__count)');
  const statsCount = await statsCounters.count();
  assert(statsCount > 0, `Found ${statsCount} visible stats counters in header`);
  for (let i = 0; i < statsCount; i++) {
    const color = await statsCounters.nth(i).evaluate(el => window.getComputedStyle(el).color);
    assert(color === 'rgb(255, 255, 255)', `Stats counter [${i}] text is #FFFFFF`, color);
  }

  const statsIconColor = await page.locator('.server-stats .server-stats__item:not(:first-child) :is(.tc-icon__icon, .material-icons, svg)').first().evaluate(el => window.getComputedStyle(el).color);
  assert(statsIconColor === 'rgb(165, 213, 238)', 'Visible stats icon color is cyan #A5D5EE', statsIconColor);

  console.log('\n--- Checking 3: SVG Rect Artifact Elimination ---');
  const activeConfRect = page.locator('svg rect[id*="Прямоугольник_3050"], svg :is(g[id*="Active_conferences"], svg) > rect:first-child[width="24"][height="24"]').first();
  if (await activeConfRect.count() > 0) {
    const fill = await activeConfRect.evaluate(el => window.getComputedStyle(el).fill);
    assert(fill === 'rgba(0, 0, 0, 0)' || fill === 'none', 'SVG white bounding box is transparent', fill);
  } else {
    console.log('  [i] Notice: specific bounding box rect not present in current DOM state');
  }

  console.log('\n--- Checking 4: Profile Dropdown Menu ---');
  const profileBtn = page.locator('[data-qa="header-profile-button"], .header-profile').first();
  await profileBtn.click();
  await page.waitForTimeout(300);

  const profileMenu = page.locator('.header-profile-info').first();
  await profileMenu.waitFor({ state: 'visible', timeout: 5000 });

  const profileNameColor = await page.locator('.profile-preview-info__text').first().evaluate(el => window.getComputedStyle(el).color);
  assert(profileNameColor === 'rgb(255, 255, 255)', 'Profile preview username text is #FFFFFF', profileNameColor);

  const langItem = page.locator('[data-qa="header-profile-main-language"] .header-profile-menu__item-text').first();
  const langColor = await langItem.evaluate(el => window.getComputedStyle(el).color);
  assert(langColor === 'rgb(255, 255, 255)', 'Profile menu item text is #FFFFFF', langColor);

  const logoutItem = page.locator('[data-qa="header-profile-main-logout"]').first();
  const logoutColor = await logoutItem.evaluate(el => window.getComputedStyle(el).color);
  assert(logoutColor === 'rgb(240, 67, 67)', 'Logout button is red #F04343 (Danger state preserved)', logoutColor);

  const logoutIcon = logoutItem.locator('.tc-icon__icon, .material-icons, svg').first();
  const logoutIconColor = await logoutIcon.evaluate(el => window.getComputedStyle(el).color);
  assert(logoutIconColor === 'rgb(240, 67, 67)', 'Logout icon is red #F04343', logoutIconColor);

  // Take screenshot
  const screenshotPath = '/tmp/task_d1_header_profile.png';
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

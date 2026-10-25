const { chromium } = require('playwright');
const path = require('path');

const BASE_URL = process.env.VKS_BASE_URL || 'http://127.0.0.1:8080';

async function run() {
  console.log('>>> [Task D2 E2E] Starting Playwright Classic Drawer Navigation Test...');
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

  console.log('\n--- Step 1: Switch to Compact Navigation (Drawer Mode) ---');
  // Open profile menu in header
  await page.click('[data-qa="header-profile-button"], .header-profile');
  await page.waitForTimeout(400);

  // Toggle "Новая навигация" switch in profile menu
  const modeSwitch = page.locator('.header-profile-info .tc-switch, .header-profile__menu .tc-switch').first();
  await modeSwitch.waitFor({ state: 'visible', timeout: 5000 });
  await modeSwitch.click();
  await page.waitForTimeout(500);

  // Verify compact header appears
  const compactHeader = page.locator('.main-compact-header');
  await compactHeader.waitFor({ state: 'visible', timeout: 5000 });
  assert(await compactHeader.isVisible(), 'Compact header is mounted after toggling navigation mode');

  // Verify theme toggle is visible and accessible in compact header
  const themeToggle = page.locator('#vks-theme-toggle-btn');
  assert(await themeToggle.isVisible(), 'Theme toggle button remains mounted and accessible in compact header');

  console.log('\n--- Step 2: Open Sidebar Drawer and Validate Contrast ---');
  const burgerBtn = page.locator('.sidebar-toggler__button, [class*="toggler"]').first();
  await burgerBtn.waitFor({ state: 'visible', timeout: 5000 });
  await burgerBtn.click();
  // Wait for slide-left transition (300ms)
  await page.waitForTimeout(450);

  const sidebarPanel = page.locator('.sidebar-panel');
  await sidebarPanel.waitFor({ state: 'visible', timeout: 5000 });
  assert(await sidebarPanel.isVisible(), 'Sidebar panel drawer is open');

  const panelBg = await sidebarPanel.evaluate(el => window.getComputedStyle(el).backgroundColor);
  assert(panelBg === 'rgb(23, 33, 43)', 'Drawer panel background is #17212B', panelBg);

  const overlay = page.locator('.sidebar-overlay');
  const overlayBg = await overlay.evaluate(el => window.getComputedStyle(el).backgroundColor);
  assert(overlayBg === 'rgba(0, 0, 0, 0.65)', 'Drawer scrim overlay is rgba(0, 0, 0, 0.65)', overlayBg);

  // Profile preview card inside drawer
  const profileCard = page.locator('.sidebar-panel .profile-preview-info__button-compact');
  const profileCardBg = await profileCard.evaluate(el => window.getComputedStyle(el).backgroundColor);
  assert(profileCardBg === 'rgb(36, 47, 61)', 'Profile card background in drawer is #242F3D', profileCardBg);

  const profileName = page.locator('.sidebar-panel .profile-preview-info__text');
  const profileNameColor = await profileName.evaluate(el => window.getComputedStyle(el).color);
  assert(profileNameColor === 'rgb(255, 255, 255)', 'Profile username in drawer is #FFFFFF', profileNameColor);

  console.log('\n--- Step 3: Validate Drawer Navigation Items ---');
  // Inactive buttons in drawer
  const inactiveBtn = page.locator('.sidebar-panel .navigation-menu-button:not(.navigation-menu-button--active) .tc-button').first();
  const inactiveBtnBg = await inactiveBtn.evaluate(el => window.getComputedStyle(el).backgroundColor);
  assert(inactiveBtnBg === 'rgba(0, 0, 0, 0)', 'Inactive drawer nav button background is transparent', inactiveBtnBg);

  const inactiveBtnText = await page.locator('.sidebar-panel .navigation-menu-button:not(.navigation-menu-button--active) .tc-button__text').first().evaluate(el => window.getComputedStyle(el).color);
  assert(inactiveBtnText === 'rgb(127, 139, 153)', 'Inactive drawer nav button text is muted #7F8B99', inactiveBtnText);

  const inactiveBtnIcon = await page.locator('.sidebar-panel .navigation-menu-button:not(.navigation-menu-button--active) :is(.tc-button__icon, .material-icons, svg)').first().evaluate(el => window.getComputedStyle(el).color);
  assert(inactiveBtnIcon === 'rgb(127, 139, 153)', 'Inactive drawer nav button icon is muted #7F8B99', inactiveBtnIcon);

  // Active button in drawer
  const activeBtn = page.locator('.sidebar-panel .navigation-menu-button--active .tc-button').first();
  const activeBtnBg = await activeBtn.evaluate(el => window.getComputedStyle(el).backgroundColor);
  assert(activeBtnBg === 'rgb(14, 22, 33)', 'Active drawer nav button background is #0E1621', activeBtnBg);

  const activeBtnText = await page.locator('.sidebar-panel .navigation-menu-button--active .tc-button__text').first().evaluate(el => window.getComputedStyle(el).color);
  assert(activeBtnText === 'rgb(255, 255, 255)', 'Active drawer nav button text is #FFFFFF', activeBtnText);

  const activeBtnIcon = await page.locator('.sidebar-panel .navigation-menu-button--active :is(.tc-button__icon, .material-icons, svg)').first().evaluate(el => window.getComputedStyle(el).color);
  assert(activeBtnIcon === 'rgb(255, 255, 255)', 'Active drawer nav button icon is #FFFFFF', activeBtnIcon);

  console.log('\n--- Step 4: Validate Drawer Bottom Switch & Logout ---');
  const switchLabel = page.locator('.sidebar-panel .navigation-mode-switch__text');
  const switchLabelColor = await switchLabel.evaluate(el => window.getComputedStyle(el).color);
  assert(switchLabelColor === 'rgb(255, 255, 255)', 'Navigation mode switch label is #FFFFFF', switchLabelColor);

  const logoutBtn = page.locator('.sidebar-panel__logout-button');
  const logoutColor = await logoutBtn.evaluate(el => window.getComputedStyle(el).color);
  assert(logoutColor === 'rgb(240, 67, 67)', 'Drawer logout button is red #F04343 (Danger state preserved)', logoutColor);

  // Take high-res screenshot
  const screenshotPath = '/tmp/task_d2_drawer_fixed.png';
  await page.screenshot({ path: screenshotPath, fullPage: false });
  console.log(`\nScreenshot captured: ${screenshotPath}`);

  console.log('\n--- Step 5: Toggle Switch Back Inside Drawer to Restore Large Header ---');
  const drawerSwitch = page.locator('.sidebar-panel .navigation-mode-switch .tc-switch').first();
  await drawerSwitch.click();
  await page.waitForTimeout(500);

  const largeHeader = page.locator('.main-large-header');
  await largeHeader.waitFor({ state: 'visible', timeout: 5000 });
  assert(await largeHeader.isVisible(), 'Large header successfully restored after toggling switch back inside drawer');

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

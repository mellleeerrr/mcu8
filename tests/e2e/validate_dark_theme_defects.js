#!/usr/bin/env node
'use strict';

/**
 * Playwright TDD & Precision Color Auditor for PROSTOR MCU (PNM).
 * Validates the 6 defect zones identified in user screenshots:
 * 1. Conferences: list items, labels, card details, icons
 * 2. Addressbook: user list, empty state placeholder
 * 3. History: table headers, table cells, action icons
 * 4. Administration: group subheaders, action tiles (text + icons)
 * 5. Power: warning and danger action cards, titles, hints, buttons (READ ONLY)
 * 6. Header: white icons, profile, notifications, theme toggle
 * 7. Dual-theme switch: dark -> light -> dark reversal without regressions
 */

const fs = require('fs');
const path = require('path');
const { chromium } = require('playwright');
const { artifactsDir } = require('./paths');

const BASE_URL = process.env.VKS_BASE_URL || 'http://127.0.0.1:8080';
const LOGIN = process.env.VKS_LOGIN || 'admin';
const PASSWORD = process.env.VKS_PASSWORD || 'admin_password';
const OUT_DIR = artifactsDir('dark-theme-audit');
const SCREENSHOT_DIR = path.join(OUT_DIR, 'screenshots');
fs.mkdirSync(SCREENSHOT_DIR, { recursive: true });

function parseRgb(colorStr) {
  if (!colorStr) return null;
  const match = colorStr.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)/);
  if (!match) return null;
  return { r: parseInt(match[1], 10), g: parseInt(match[2], 10), b: parseInt(match[3], 10) };
}

function isWhiteOrNearWhite(colorStr) {
  const rgb = parseRgb(colorStr);
  if (!rgb) return false;
  return rgb.r >= 240 && rgb.g >= 240 && rgb.b >= 240;
}

function isSecondaryMuted(colorStr) {
  const rgb = parseRgb(colorStr);
  if (!rgb) return false;
  // #7F8B99 is rgb(127, 139, 153), tolerance +-35
  return rgb.r >= 95 && rgb.r <= 175 && rgb.g >= 110 && rgb.g <= 190 && rgb.b >= 125 && rgb.b <= 215;
}

function isContentCyanIcon(colorStr) {
  const rgb = parseRgb(colorStr);
  if (!rgb) return false;
  // #A5D5EE is rgb(165, 213, 238), tolerance +-40
  return rgb.r >= 120 && rgb.r <= 200 && rgb.g >= 170 && rgb.g <= 245 && rgb.b >= 190 && rgb.b <= 255;
}

async function runValidation() {
  console.log(`[TDD Audit] Launching Chromium against ${BASE_URL}...`);
  const browser = await chromium.launch({
    headless: true,
    args: ['--no-sandbox', '--disable-dev-shm-usage', '--disable-gpu', '--force-color-profile=srgb']
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
  const results = { zones: {}, passed: 0, failed: 0, warnings: 0 };

  try {
    // 0. Authenticate
    console.log('[TDD Audit] Authenticating...');
    await page.goto(`${BASE_URL}/pnm/authorization`);
    await page.waitForSelector('input[type="text"]', { timeout: 10000 });
    await page.fill('input[type="text"]', LOGIN);
    await page.fill('input[type="password"]', PASSWORD);
    await page.click('[data-qa="authorization-sign-in-button"]');
    await page.waitForSelector('.main-large-header', { timeout: 10000 });
    console.log('[TDD Audit] Successfully authenticated.');

    const stabilizeView = async () => {
      await page.addStyleTag({
        content: '*, *::before, *::after { transition: none !important; animation: none !important; caret-color: transparent !important; }'
      });
      await page.evaluate(() => document.fonts.ready);
      await page.waitForTimeout(100);
    };

    // ZONE 1: Conferences & Details
    console.log('[TDD Audit] Auditing Zone 1: Conferences...');
    await page.goto(`${BASE_URL}/pnm/monitoring/conferences`);
    await page.waitForSelector('.monitoring-screen, .main-list', { timeout: 8000 });
    await stabilizeView();

    const zone1 = { name: 'Conferences', checks: [] };
    const firstConf = page.locator('.main-list-item').first();
    const confCount = await firstConf.count();
    if (confCount > 0) {
      await firstConf.click();
      await page.waitForTimeout(200);
      
      const itemText = await firstConf.locator('.main-list-item__text--primary, .monitoring-list-item__name').first().evaluate(el => window.getComputedStyle(el).color).catch(() => null);
      zone1.checks.push({ check: 'Conference list item primary text is white', color: itemText, pass: isWhiteOrNearWhite(itemText) });

      const itemIcon = await firstConf.locator('.material-icons, .tc-icon__icon, svg').first().evaluate(el => window.getComputedStyle(el).color).catch(() => null);
      zone1.checks.push({ check: 'Conference list item icon is cyan #A5D5EE', color: itemIcon, pass: isContentCyanIcon(itemIcon) });
    }

    const detailLabels = await page.locator('.conference-info-main-row-item--fixed-width, .main-card-content label, .main-form__subheader-text').all();
    if (detailLabels.length > 0) {
      const labelColor = await detailLabels[0].evaluate(el => window.getComputedStyle(el).color);
      zone1.checks.push({ check: 'Conference detail label is muted #7F8B99', color: labelColor, pass: isSecondaryMuted(labelColor) });
    }

    await page.screenshot({ path: path.join(SCREENSHOT_DIR, '01_conferences_details.png') });
    results.zones.zone1 = zone1;

    // ZONE 2: Addressbook / Users
    console.log('[TDD Audit] Auditing Zone 2: Addressbook / Users...');
    await page.goto(`${BASE_URL}/pnm/addressbook/users`);
    await page.waitForSelector('.user-screen, .main-list', { timeout: 8000 });
    await stabilizeView();

    const zone2 = { name: 'Addressbook Users', checks: [] };
    const userItem = page.locator('.main-list-item').first();
    if (await userItem.count() > 0) {
      const userNameColor = await userItem.locator('.main-list-item__text--primary').first().evaluate(el => window.getComputedStyle(el).color).catch(() => null);
      zone2.checks.push({ check: 'User name is white #FFFFFF', color: userNameColor, pass: isWhiteOrNearWhite(userNameColor) });

      // User avatar text/icon is white on colored avatar surface
      const userAvatarColor = await userItem.locator('.tc-avatar__name-icon, .material-icons').first().evaluate(el => window.getComputedStyle(el).color).catch(() => null);
      zone2.checks.push({ check: 'User avatar symbol is white #FFFFFF', color: userAvatarColor, pass: isWhiteOrNearWhite(userAvatarColor) });
    }

    const placeholder = page.locator('.main-blank__text--primary');
    if (await placeholder.isVisible()) {
      const plColor = await placeholder.evaluate(el => window.getComputedStyle(el).color);
      zone2.checks.push({ check: 'Placeholder text is white #FFFFFF', color: plColor, pass: isWhiteOrNearWhite(plColor) });
    }

    await page.screenshot({ path: path.join(SCREENSHOT_DIR, '02_directory_users.png') });
    results.zones.zone2 = zone2;

    // ZONE 3: History
    console.log('[TDD Audit] Auditing Zone 3: History...');
    await page.goto(`${BASE_URL}/pnm/history`);
    await page.waitForSelector('.logs-screen, .main-table', { timeout: 8000 });
    await stabilizeView();

    const zone3 = { name: 'History Table', checks: [] };
    const thEl = page.locator('.main-table-head .main-table-el, th').first();
    if (await thEl.count() > 0) {
      const thColor = await thEl.evaluate(el => window.getComputedStyle(el).color);
      zone3.checks.push({ check: 'Table header text is white #FFFFFF', color: thColor, pass: isWhiteOrNearWhite(thColor) });
    }

    const tdEl = page.locator('.main-table-line .main-table-el, td').first();
    if (await tdEl.count() > 0) {
      const tdColor = await tdEl.evaluate(el => window.getComputedStyle(el).color);
      zone3.checks.push({ check: 'Table cell text is white #FFFFFF', color: tdColor, pass: isWhiteOrNearWhite(tdColor) });
    }

    const actionIcon = page.locator('.main-table-line .tc-icon__icon, .main-table-line .material-icons').first();
    if (await actionIcon.count() > 0) {
      const actColor = await actionIcon.evaluate(el => window.getComputedStyle(el).color);
      zone3.checks.push({ check: 'Action download icon is cyan #A5D5EE', color: actColor, pass: isContentCyanIcon(actColor) });
    }

    await page.screenshot({ path: path.join(SCREENSHOT_DIR, '03_history_table.png') });
    results.zones.zone3 = zone3;

    // ZONE 4: Administration
    console.log('[TDD Audit] Auditing Zone 4: Administration...');
    await page.goto(`${BASE_URL}/pnm/administration`);
    await page.waitForSelector('.administration-screen', { timeout: 8000 });
    await stabilizeView();

    const zone4 = { name: 'Administration', checks: [] };
    const subheader = page.locator('.main-form__subheader-text').first();
    if (await subheader.count() > 0) {
      const subColor = await subheader.evaluate(el => window.getComputedStyle(el).color);
      zone4.checks.push({ check: 'Section subheader is muted #7F8B99', color: subColor, pass: isSecondaryMuted(subColor) });
    }

    const adminTile = page.locator('.administration-button').first();
    if (await adminTile.count() > 0) {
      const tileText = await adminTile.locator('.administration-button__text').evaluate(el => window.getComputedStyle(el).color);
      zone4.checks.push({ check: 'Admin tile text is white #FFFFFF', color: tileText, pass: isWhiteOrNearWhite(tileText) });

      const tileIcon = await adminTile.locator('.tc-button__icon, .material-icons').evaluate(el => window.getComputedStyle(el).color);
      zone4.checks.push({ check: 'Admin tile icon is cyan #A5D5EE', color: tileIcon, pass: isContentCyanIcon(tileIcon) });
    }

    await page.screenshot({ path: path.join(SCREENSHOT_DIR, '04_administration_tiles.png') });
    results.zones.zone4 = zone4;

    // ZONE 5: Power (READ ONLY!)
    console.log('[TDD Audit] Auditing Zone 5: Power (READ ONLY)...');
    await page.goto(`${BASE_URL}/pnm/administration/power`);
    await page.waitForSelector('.administration-screen', { timeout: 8000 });
    await stabilizeView();

    const zone5 = { name: 'Power Action Cards', checks: [] };
    const actionCards = await page.locator('.action-card').all();
    if (actionCards.length > 0) {
      for (let i = 0; i < Math.min(actionCards.length, 3); i++) {
        const titleColor = await actionCards[i].locator('.action-card__title, h4, p').first().evaluate(el => window.getComputedStyle(el).color).catch(() => null);
        zone5.checks.push({ check: `Action card #${i+1} title is white #FFFFFF`, color: titleColor, pass: isWhiteOrNearWhite(titleColor) });

        const btnText = await actionCards[i].locator('.tc-button__text, .mdc-button__label').first().evaluate(el => window.getComputedStyle(el).color).catch(() => null);
        zone5.checks.push({ check: `Action card #${i+1} button text is white #FFFFFF`, color: btnText, pass: isWhiteOrNearWhite(btnText) });
      }
    }

    await page.screenshot({ path: path.join(SCREENSHOT_DIR, '05_power_action_cards.png') });
    results.zones.zone5 = zone5;

    // ZONE 6: Header & Toggle
    console.log('[TDD Audit] Auditing Zone 6: Header & Toggle...');
    const zone6 = { name: 'Header Icons & Toggle', checks: [] };
    const headerIcons = await page.locator('.main-large-header__actions-wrapper .material-icons, .main-large-header__actions-wrapper .tc-icon__icon, #vks-theme-toggle-btn svg, .navigation-menu-button--active .tc-button__icon').all();
    if (headerIcons.length > 0) {
      const hColor = await headerIcons[0].evaluate(el => window.getComputedStyle(el).color);
      zone6.checks.push({ check: 'Header icon is white #FFFFFF', color: hColor, pass: isWhiteOrNearWhite(hColor) });
    }

    const toggleBtn = page.locator('#vks-theme-toggle-btn');
    const isToggleVisible = await toggleBtn.isVisible();
    zone6.checks.push({ check: 'Theme toggle button is visible in header', pass: isToggleVisible });

    await page.screenshot({ path: path.join(SCREENSHOT_DIR, '06_header_icons.png') });
    results.zones.zone6 = zone6;

    // ZONE 7: Dual-Theme Reversal (Dark -> Light -> Dark)
    console.log('[TDD Audit] Auditing Zone 7: Dual-Theme Reversal...');
    const zone7 = { name: 'Dual-Theme Reversal', checks: [] };
    if (isToggleVisible) {
      // Toggle to Light
      await toggleBtn.click();
      await page.waitForFunction(() => document.documentElement.classList.contains('theme-light'), { timeout: 3000 });
      zone7.checks.push({ check: 'Switched to Light theme', pass: true });

      // Verify title in Light theme is dark
      const lightHeaderColor = await page.locator('.main-card-header__title, .administration__title, .action-card__title, h3').first().evaluate(el => window.getComputedStyle(el).color).catch(() => null);
      const isTextDarkInLight = lightHeaderColor && !isWhiteOrNearWhite(lightHeaderColor);
      zone7.checks.push({ check: 'Title in light theme is dark (no white-on-white defect)', color: lightHeaderColor, pass: isTextDarkInLight });

      // Verify button/hint in Light theme is dark
      const lightActionHint = await page.locator('.action-card__hint, .main-form__subheader-text, p').first().evaluate(el => window.getComputedStyle(el).color).catch(() => null);
      const isHintDarkInLight = lightActionHint && !isWhiteOrNearWhite(lightActionHint);
      zone7.checks.push({ check: 'Body text in light theme is dark', color: lightActionHint, pass: isHintDarkInLight });

      // Toggle back to Dark
      await toggleBtn.click();
      await page.waitForFunction(() => document.documentElement.classList.contains('theme-dark'), { timeout: 3000 });
      zone7.checks.push({ check: 'Restored Dark theme', pass: true });
    }
    results.zones.zone7 = zone7;

    // Calculate totals
    for (const key of Object.keys(results.zones)) {
      const z = results.zones[key];
      for (const c of z.checks) {
        if (c.pass) results.passed++;
        else results.failed++;
      }
    }

    console.log('\n================ TDD AUDIT RESULTS ================');
    console.log(`Passed checks: ${results.passed}, Failed checks: ${results.failed}`);
    console.log(JSON.stringify(results, null, 2));
    console.log('===================================================\n');

    fs.writeFileSync(path.join(OUT_DIR, 'tdd_validation_report.json'), JSON.stringify(results, null, 2));

  } catch (err) {
    console.error('[TDD Audit] Fatal error:', err);
    results.error = err.message;
  } finally {
    await browser.close();
  }

  return results;
}

runValidation().then(res => {
  if (res.error || res.failed > 0 || res.passed < 15) process.exit(1);
  process.exit(0);
});

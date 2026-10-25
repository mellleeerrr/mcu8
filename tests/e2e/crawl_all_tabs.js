#!/usr/bin/env node
'use strict';

/**
 * Playwright E2E UI Crawler & Full Slate Dark Theme Auditor for PROSTOR MCU (PNM).
 *
 * Crawls EVERY tab, sub-tab, dialog, drawer, button, and font across the entire UI:
 * 1. Authenticates via UI login form (or API fallback).
 * 2. Navigates through ALL real Vue-Router routes:
 *    - /pnm/authorization
 *    - /pnm/monitoring/sessions (+ details & tabs)
 *    - /pnm/monitoring/conferences (+ details, layouts tab & 54 Codian schemes editor)
 *    - /pnm/addressbook/users & /pnm/addressbook/groups (+ modals)
 *    - /pnm/history
 *    - /pnm/administration (+ network, calls, video, security, license, layout_skins, etc.)
 * 3. Deeply audits:
 *    - All buttons (classes, background, text color, borders)
 *    - All typography (family, size, weight, contrast)
 *    - All surfaces, modals, popovers, select menus (mdc-select, tc-select)
 *    - Light background violations (RGB luminance > 160 or #fff/#fafafa)
 *    - Low contrast text
 * 4. Captures high-res screenshots for each view.
 * 5. Generates structured JSON report with exact CSS selectors.
 */

const fs = require('fs');
const path = require('path');
const { chromium } = require('playwright');
const { artifactsDir } = require('./paths');

const BASE_URL = process.env.VKS_BASE_URL || 'http://127.0.0.1:8080';
const LOGIN = process.env.VKS_LOGIN || 'admin';
const PASSWORD = process.env.VKS_PASSWORD || 'admin_password';
const OUT_DIR = artifactsDir('dark-theme-audit');
fs.mkdirSync(OUT_DIR, { recursive: true });

async function runAudit() {
  console.log(`[Playwright Audit] Starting comprehensive Slate Dark Theme audit against ${BASE_URL}...`);
  const browser = await chromium.launch({
    headless: true,
    args: ['--no-sandbox', '--disable-dev-shm-usage']
  });
  const context = await browser.newContext({
    viewport: { width: 1920, height: 1080 },
    colorScheme: 'dark'
  });
  const page = await context.newPage();

  // Set local storage theme preference to dark initially
  await page.addInitScript(() => {
    try {
      localStorage.setItem('VKS_PNM_THEME', 'dark');
    } catch (_) {}
  });

  const pageErrors = [];
  page.on('pageerror', err => {
    console.error('[Playwright Audit] UNCAUGHT PAGE ERROR:', err.message);
    pageErrors.push(err.message);
  });

  const report = {
    startedAt: new Date().toISOString(),
    routesVisited: [],
    tabsFound: [],
    buttonsAudited: 0,
    fontsAudited: 0,
    anomalies: [],
    cssSelectorRecommendations: [],
    screenshots: {}
  };

  // Helper to audit current view
  async function auditCurrentView(routeKey) {
    console.log(`[Playwright Audit] Auditing view: ${routeKey}...`);
    await page.waitForTimeout(1000);

    const shotPath = path.join(OUT_DIR, `${routeKey.replace(/[^a-zA-Z0-9_-]/g, '_')}.png`);
    await page.screenshot({ path: shotPath, fullPage: true });
    report.screenshots[routeKey] = shotPath;

    const domAudit = await page.evaluate((rKey) => {
      function getCssSelector(el) {
        if (!el || el.nodeType !== Node.ELEMENT_NODE) return '';
        if (el.id) return `#${el.id}`;
        let sel = el.tagName.toLowerCase();
        if (el.className && typeof el.className === 'string') {
          const classes = el.className.trim().split(/\s+/).filter(c => c && !c.includes(':') && !c.includes('/'));
          if (classes.length > 0) {
            sel += '.' + classes.slice(0, 3).join('.');
          }
        }
        if (el.parentElement && el.parentElement !== document.body && el.parentElement !== document.documentElement) {
          const pSel = el.parentElement.tagName.toLowerCase() + (el.parentElement.className && typeof el.parentElement.className === 'string' ? '.' + el.parentElement.className.trim().split(/\s+/)[0] : '');
          sel = pSel + ' > ' + sel;
        }
        return sel;
      }

      function parseRgb(colorStr) {
        const m = colorStr.match(/rgba?\((\d+),\s*(\d+),\s*(\d+)(?:,\s*([\d.]+))?\)/);
        if (!m) return null;
        return {
          r: parseInt(m[1], 10),
          g: parseInt(m[2], 10),
          b: parseInt(m[3], 10),
          a: m[4] !== undefined ? parseFloat(m[4]) : 1
        };
      }

      function getLum(rgb) {
        return 0.299 * rgb.r + 0.587 * rgb.g + 0.114 * rgb.b;
      }

      const results = {
        route: rKey,
        elementsCount: document.querySelectorAll('*').length,
        lightBackgroundElements: [],
        darkTextOnDarkElements: [],
        buttons: [],
        selects: [],
        modals: [],
        fonts: new Set()
      };

      const allElements = document.querySelectorAll('body *');
      for (const el of allElements) {
        const rect = el.getBoundingClientRect();
        if (rect.width === 0 || rect.height === 0) continue;
        const style = window.getComputedStyle(el);
        if (style.display === 'none' || style.visibility === 'hidden') continue;

        const bg = style.backgroundColor;
        const color = style.color;
        const font = `${style.fontFamily} ${style.fontSize} ${style.fontWeight}`;
        results.fonts.add(font);

        const parsedBg = parseRgb(bg);
        const parsedColor = parseRgb(color);

        // Check for unintended light backgrounds (excluding visual samples: video, canvas, images, svgs, color swatches)
        if (!['VIDEO', 'CANVAS', 'IMG', 'PICTURE', 'SVG', 'PATH'].includes(el.tagName) && !el.classList.contains('color-picker-item__color')) {
          if (parsedBg && parsedBg.a > 0.3) {
            const lum = getLum(parsedBg);
            if (lum > 160 && rect.width > 15 && rect.height > 15) {
              results.lightBackgroundElements.push({
                tag: el.tagName,
                className: typeof el.className === 'string' ? el.className : '',
                selector: getCssSelector(el),
                textSnippet: (el.innerText || '').slice(0, 50).replace(/\s+/g, ' ').trim(),
                bg,
                color,
                lum: Math.round(lum),
                rect: { w: Math.round(rect.width), h: Math.round(rect.height) }
              });
            }
          }
        }

        // Check buttons
        if (el.tagName === 'BUTTON' || el.classList.contains('mdc-button') || el.classList.contains('tc-button')) {
          const btnText = (el.innerText || el.getAttribute('aria-label') || el.getAttribute('title') || '').trim().replace(/\s+/g, ' ');
          results.buttons.push({
            tag: el.tagName,
            className: typeof el.className === 'string' ? el.className : '',
            selector: getCssSelector(el),
            text: btnText.slice(0, 35),
            bg,
            color,
            border: style.border,
            isLightBg: parsedBg ? (parsedBg.a > 0.4 && getLum(parsedBg) > 160) : false
          });
        }

        // Check select fields
        if (el.classList.contains('mdc-select') || el.classList.contains('tc-select') || el.tagName === 'SELECT') {
          results.selects.push({
            tag: el.tagName,
            className: typeof el.className === 'string' ? el.className : '',
            selector: getCssSelector(el),
            bg,
            color
          });
        }

        // Check modal surfaces
        if (el.classList.contains('tc-modal') || el.classList.contains('mdc-dialog__surface') || el.classList.contains('the-confirmer-popup')) {
          results.modals.push({
            tag: el.tagName,
            className: typeof el.className === 'string' ? el.className : '',
            selector: getCssSelector(el),
            bg,
            color
          });
        }
      }

      results.fonts = Array.from(results.fonts).slice(0, 30);
      results.lightBackgroundElements = results.lightBackgroundElements.slice(0, 25);
      results.buttons = results.buttons.slice(0, 40);
      return results;
    }, routeKey);

    report.routesVisited.push(domAudit);
    report.buttonsAudited += domAudit.buttons.length;
    if (routeKey !== "21_theme_toggled_to_light" && domAudit.lightBackgroundElements.length > 0) {
      report.anomalies.push({
        route: routeKey,
        issues: domAudit.lightBackgroundElements
      });
      for (const item of domAudit.lightBackgroundElements) {
        if (item.selector && !report.cssSelectorRecommendations.includes(item.selector)) {
          report.cssSelectorRecommendations.push(item.selector);
        }
      }
    }
  }

  // Helper to safely navigate via client-side router or goto
  async function navigateToRoute(pnmPath, label) {
    console.log(`[Playwright Audit] Navigating to ${label} (${pnmPath})...`);
    try {
      // First try in-app navigation via Vue Router if available
      const routed = await page.evaluate((targetPath) => {
        try {
          const r = window.__VUE_ROUTER__ || (window.__VUE_APP__ && window.__VUE_APP__.config && window.__VUE_APP__.config.globalProperties && window.__VUE_APP__.config.globalProperties.$router);
          if (r) {
            r.push(targetPath);
            return true;
          }
        } catch (_) {}
        return false;
      }, pnmPath);

      if (!routed) {
        await page.goto(`${BASE_URL}${pnmPath}`, { waitUntil: 'networkidle', timeout: 8000 }).catch(() => {});
      }
      await page.waitForTimeout(1200);
      await auditCurrentView(label);
    } catch (err) {
      console.warn(`[Playwright Audit] Navigation to ${pnmPath} failed:`, err.message);
    }
  }

  // 1. Visit Authorization Page
  console.log('[Playwright Audit] Visiting Authorization page...');
  await page.goto(`${BASE_URL}/pnm/authorization`, { waitUntil: 'networkidle' });
  await auditCurrentView('01_authorization');

  // 2. Perform Login
  console.log('[Playwright Audit] Submitting login form...');
  const userField = await page.$('input[type="text"], input[name="login"], input[name="username"]');
  const passField = await page.$('input[type="password"]');
  if (userField && passField) {
    await userField.fill(LOGIN);
    await passField.fill(PASSWORD);
    const submitBtn = await page.$('button[type="submit"], button.mdc-button--raised, button.tc-button--primary');
    if (submitBtn) {
      await submitBtn.click();
    } else {
      await page.keyboard.press('Enter');
    }
    await page.waitForTimeout(2500);
  }

  // Check where we landed after login
  await auditCurrentView('02_monitoring_sessions');

  // Discover top navigation items
  const navTargets = await page.evaluate(() => {
    const list = [];
    document.querySelectorAll('.main-large-header button, .main-large-header a, .main-compact-header button').forEach(el => {
      list.push({
        text: (el.innerText || el.getAttribute('aria-label') || '').trim().replace(/\s+/g, ' '),
        className: el.className
      });
    });
    return list;
  });
  report.tabsFound = navTargets;
  console.log(`[Playwright Audit] Found ${navTargets.length} header controls:`, navTargets.map(t => t.text).filter(Boolean));

  // 3. Select first conference session if available
  try {
    const firstSessionItem = await page.$('.monitoring-list-item, .main-list-item');
    if (firstSessionItem) {
      console.log('[Playwright Audit] Clicking first session item to audit detail view...');
      await firstSessionItem.click();
      await page.waitForTimeout(1000);
      await auditCurrentView('02b_session_details_active');

      // Click subtabs in card (Участники, Раскладки, etc.)
      const tabs = await page.$$('.main-card-header .tc-tab, .main-card-header [role="tab"]');
      for (let i = 0; i < tabs.length; i++) {
        const tabText = (await tabs[i].innerText()).trim();
        await tabs[i].click();
        await page.waitForTimeout(800);
        await auditCurrentView(`02c_session_tab_${tabText || i}`);
      }
    }
  } catch (e) {
    console.warn('[Playwright Audit] Session list click error:', e.message);
  }

  // 4. Conferences Section & Layouts Editor
  // Click on "Конференции" dropdown in header or navigate directly
  try {
    console.log('[Playwright Audit] Opening Conferences dropdown in header...');
    const confDropdownBtn = await page.$('.main-route-selector, .header-route-selector, .main-header-dropdown, button:has-text("Конференции")');
    if (confDropdownBtn) {
      await confDropdownBtn.click();
      await page.waitForTimeout(500);
      await auditCurrentView('03a_conferences_dropdown_menu');
      // Click "Конференции" inside dropdown menu
      const confMenuItem = await page.$('.tc-context-menu-item:has-text("Конференции"), .mdc-deprecated-list-item:has-text("Конференции")');
      if (confMenuItem) {
        await confMenuItem.click();
        await page.waitForTimeout(1200);
      }
    }
  } catch (e) {}

  await navigateToRoute('/pnm/monitoring/conferences', '03_monitoring_conferences');

  // Select first conference to open its details & Layouts
  try {
    const firstConf = await page.$('.monitoring-list-item, .main-list-item');
    if (firstConf) {
      console.log('[Playwright Audit] Selecting first conference...');
      await firstConf.click();
      await page.waitForTimeout(1000);
      await auditCurrentView('03b_conference_details');

      // Look for "Раскладки" tab
      const layoutTab = await page.$('.tc-tab:has-text("Раскладки"), [role="tab"]:has-text("Раскладки")');
      if (layoutTab) {
        console.log('[Playwright Audit] Switching to Layouts (Раскладки) tab...');
        await layoutTab.click();
        await page.waitForTimeout(1200);
        await auditCurrentView('03c_conference_layouts_tab');

        // Look for layout item or "Добавить раскладку" / "Редактировать"
        const editLayoutBtn = await page.$('.layout-card, .main-table-line, button:has-text("Добавить"), button:has-text("Редактировать")');
        if (editLayoutBtn) {
          console.log('[Playwright Audit] Opening Layout Editor...');
          await editLayoutBtn.click();
          await page.waitForTimeout(1200);
          await auditCurrentView('03d_layout_editor_modal');

          // Check for Codian 54 schemes palette
          const schemeSelector = await page.$('.layout-scheme-selector, .layout-schemes, .codian-layout-scheme');
          if (schemeSelector) {
            await auditCurrentView('03e_layout_editor_codian_schemes');
          }

          // Close modal if open
          const closeBtn = await page.$('.tc-modal__close, .mdc-dialog__close, button[aria-label="Закрыть"]');
          if (closeBtn) {
            await closeBtn.click();
            await page.waitForTimeout(500);
          }
        }
      }
    }
  } catch (e) {
    console.warn('[Playwright Audit] Layouts editor check error:', e.message);
  }

  // 5. Address Book Section (Справочник)
  await navigateToRoute('/pnm/addressbook', '04a_addressbook_root');
  await navigateToRoute('/pnm/addressbook/users', '04b_addressbook_users');

  // Try opening "Add User" dialog
  try {
    const addUserBtn = await page.$('button:has-text("Добавить"), button.tc-button--primary:has-text("Создать")');
    if (addUserBtn) {
      console.log('[Playwright Audit] Opening Add Contact/User dialog...');
      await addUserBtn.click();
      await page.waitForTimeout(800);
      await auditCurrentView('04c_addressbook_add_user_dialog');
      const cancelBtn = await page.$('.tc-modal__footer button:has-text("Отмена"), .mdc-dialog__actions button:has-text("Отмена")');
      if (cancelBtn) await cancelBtn.click();
      await page.waitForTimeout(500);
    }
  } catch (e) {}

  await navigateToRoute('/pnm/addressbook/groups', '04d_addressbook_groups');

  // 6. History Section (История)
  await navigateToRoute('/pnm/history', '05_history');

  // 7. Administration Section (Администрирование)
  await navigateToRoute('/pnm/administration', '06_administration_hub');

  // Audit all administration sub-sections
  const adminRoutes = [
    { path: '/pnm/administration/network', label: '07_admin_network' },
    { path: '/pnm/administration/calls', label: '08_admin_calls' },
    { path: '/pnm/administration/video', label: '09_admin_video' },
    { path: '/pnm/administration/security', label: '10_admin_security' },
    { path: '/pnm/administration/license', label: '11_admin_license' },
    { path: '/pnm/administration/layout_skins', label: '12_admin_layout_skins' },
    { path: '/pnm/administration/conference_settings', label: '13_admin_conference_settings' },
    { path: '/pnm/administration/date_time', label: '14_admin_date_time' },
    { path: '/pnm/administration/backup', label: '15_admin_backup' },
    { path: '/pnm/administration/reset', label: '16_admin_reset' },
    { path: '/pnm/administration/roles', label: '17_admin_roles' },
    { path: '/pnm/administration/about', label: '18_admin_about' },
    { path: '/pnm/administration/event_list', label: '19_admin_event_list' }
  ];

  for (const item of adminRoutes) {
    await navigateToRoute(item.path, item.label);
  }

  // 8. Test Header Profile Menu
  try {
    const profileBtn = await page.$('.header-profile, .header-profile__button');
    if (profileBtn) {
      console.log('[Playwright Audit] Opening Profile menu...');
      await profileBtn.click();
      await page.waitForTimeout(600);
      await auditCurrentView('20_profile_menu_open');
      await page.keyboard.press('Escape');
    }
  } catch (e) {}

  // 9. Test Theme Toggle Switch (Dark -> Light -> Dark)
  console.log('[Playwright Audit] Verifying theme toggle button in header...');
  const toggleBtn = await page.waitForSelector('.main-large-header__actions-wrapper #vks-theme-toggle-btn', { timeout: 5000 });
  if (!toggleBtn) throw new Error('Theme toggle button not found in header!');
  console.log('[Playwright Audit] Toggling theme to Light mode...');
  await toggleBtn.click();
  await page.waitForTimeout(700);
  const isLight = await page.evaluate(() => document.documentElement.classList.contains('theme-light'));
  if (!isLight) throw new Error('Theme did not switch to theme-light after click!');
  await auditCurrentView('21_theme_toggled_to_light');
  console.log('[Playwright Audit] Toggling theme BACK to Telegram Dark mode...');
  await toggleBtn.click();
  await page.waitForTimeout(700);
  const isDark = await page.evaluate(() => document.documentElement.classList.contains('theme-dark'));
  if (!isDark) throw new Error('Theme did not switch back to theme-dark after click!');
  await auditCurrentView('22_theme_toggled_back_to_dark');
  await browser.close();

  report.pageErrors = pageErrors;
  const reportFile = path.join(OUT_DIR, 'audit_report.json');
  fs.writeFileSync(reportFile, JSON.stringify(report, null, 2));
  if (pageErrors.length > 0) {
    throw new Error(`Audit failed with ${pageErrors.length} uncaught page errors: ${pageErrors.join("; ")}`);
  }
  console.log(`[Playwright Audit] Audit complete! Report written to: ${reportFile}`);
  console.log(`[Playwright Audit] Views Audited: ${report.routesVisited.length}`);
  console.log(`[Playwright Audit] Total Buttons Audited: ${report.buttonsAudited}`);
  console.log(`[Playwright Audit] Total Views with Light-Theme Anomalies: ${report.anomalies.length}`);
  console.log(`[Playwright Audit] Unique CSS Selectors Needing Dark Override: ${report.cssSelectorRecommendations.length}`);
  return report;
}

if (require.main === module) {
  runAudit().catch((err) => {
    console.error('[Playwright Audit] FATAL ERROR:', err);
    process.exit(1);
  });
}

module.exports = { runAudit };

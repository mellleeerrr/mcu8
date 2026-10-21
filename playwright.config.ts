import { defineConfig } from '@playwright/test';

// Запуск: npx playwright test  (после `npm i -D @playwright/test@1.64.1`)
// Сквозной живой сценарий находится в tests/e2e/live.js и запускается без
// установки пакета: `NODE_PATH=<playwright node_modules> node tests/e2e/live.js`.
export default defineConfig({
  testDir: './tests/e2e',
  timeout: 120_000,
  expect: { timeout: 15_000 },
  use: {
    baseURL: 'http://127.0.0.1:8080/pnm',
    trace: 'on',
    screenshot: 'only-on-failure',
    video: 'retain-on-failure',
    viewport: { width: 1600, height: 1000 },
  },
  reporter: [['list'], ['json', { outputFile: 'artifacts/e2e/playwright-report.json' }]],
});

# Live E2E (Playwright API)

`live.js` uses the `playwright` Node API directly (not the `@playwright/test`
runner), because the scenario is a live browser session against a running
stand, not a fixture-based test suite. There is no `playwright.config.ts` and
`npx playwright test` is not used.

## Install

```bash
cd tests/e2e
npm ci
npx playwright install chromium
```

`npm test` runs `node live.js`. The script auto-detects a local Chromium only
when `VKS_CHROMIUM` is unset; override it if your cache path differs.

## Run

```bash
VKS_BASE_URL=http://127.0.0.1:8080 \
VKS_LOGIN=admin VKS_PASSWORD=admin_password \
VKS_SIP_URI=sip:probe-e2e@192.168.1.132 \
npm test
```

Artifacts go to `VKS_ARTIFACTS` (default `artifacts/e2e`): `result.json`,
`network.json`, screenshots, `trace.zip`. The script creates and removes its
own test rooms; it requires the live server and the remote SIP terminal.
Set `VKS_PASSWORD` for non-dev configs.

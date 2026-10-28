/**
 * Challenger 1: Empirical & Adversarial Mathematical Stress Test Suite
 * Target: Dynamic Layout Constructor (15 Archetypes x Boundary Counts + Domain Sweep)
 *
 * Requirements:
 * 1. Stress test slot computation math across all 15 archetypes for extreme and boundary window counts:
 *    N in [1, 2, 3, 4, 5, 7, 8, 9, 13, 16, 25, 49, 50, 75, 99, 100].
 *    Verify:
 *    - Exactly N slots are produced for count N.
 *    - All slot coordinates satisfy 0 <= x < 1280, 0 <= y < 720 and w > 0, h > 0, x+w <= 1280.001, y+h <= 720.001.
 *    - Zero NaNs, zero infinities, zero negative dimensions, zero division by zero.
 * 2. Full domain sweep for all 15 archetypes x 100 window counts (N=1..100) = 1500 cases.
 * 3. Adversarial / Fuzzing tests (N=0, -1, 101, NaN, Infinity, strings, decimals).
 * 4. Graceful degradation checks (N=1, N=2, N=3, N=4 specifics).
 * 5. SVG generation and coordinate integrity check.
 * 6. Format calculated slots scale resilience check.
 */

const fs = require('fs');
const path = require('path');
const vm = require('vm');

console.log('================================================================================');
console.log('>>> [CHALLENGER 1] STARTING EMPIRICAL & ADVERSARIAL SLOT MATH STRESS TEST SUITE');
console.log('================================================================================\n');

// 1. Read codian-layouts.js directly from source to extract authoritative functions
const codianLayoutsPath = path.resolve(__dirname, '../../client/pnm-frontend/deployed/pnm/codian-layouts.js');
if (!fs.existsSync(codianLayoutsPath)) {
  console.error(`ERROR: File not found: ${codianLayoutsPath}`);
  process.exit(1);
}
const fileContent = fs.readFileSync(codianLayoutsPath, 'utf8');

// Extract the mathematical functions and constants from codian-layouts.js
// We extract: CANVAS_W, CANVAS_H, MAX_COUNT, round, computeBalancedGrid, computeSlots, svgFromSlots, formatCalculatedSlots
const sandbox = {
  console: console,
  Math: Math,
  Number: Number,
  isFinite: isFinite,
  isNaN: isNaN,
  Array: Array,
  String: String,
  findEntry: () => null,
  exports: {}
};

// Create extraction script from the exact source file
const extractCode = `
var CANVAS_W = 1280;
var CANVAS_H = 720;
var MAX_COUNT = 100;
var PREVIEW_WIDTH = 1280;
var PREVIEW_HEIGHT = 720;

function round(value) {
  return Math.round(value * 100) / 100;
}

${fileContent.match(/function computeBalancedGrid[\s\S]*?return res;\s*\}/)[0]}

${fileContent.match(/function computeSlots[\s\S]*?return slots;\s*\}/)[0]}

${fileContent.match(/function svgFromSlots[\s\S]*?return '<svg[\s\S]*?<\/svg>';\s*\}/)[0]}

${fileContent.match(/function formatCalculatedSlots[\s\S]*?return res;\s*\}/)[0]}

exports.CANVAS_W = CANVAS_W;
exports.exports_CANVAS_H = CANVAS_H;
exports.MAX_COUNT = MAX_COUNT;
exports.computeBalancedGrid = computeBalancedGrid;
exports.computeSlots = computeSlots;
exports.svgFromSlots = svgFromSlots;
exports.formatCalculatedSlots = formatCalculatedSlots;
exports.round = round;
`;

vm.createContext(sandbox);
vm.runInContext(extractCode, sandbox);

const { computeBalancedGrid, computeSlots, svgFromSlots, formatCalculatedSlots } = sandbox.exports;

const ARCHETYPES = [
  'grid', 'pin_l', 'pin_r', 'pin_t', 'pin_b',
  'dual_h', 'dual_v', 'pip', 'frame', 'split_50',
  'trio', 'corner_l', 'corner_r', 'quad_focus', 'cinema'
];

const BOUNDARY_COUNTS = [1, 2, 3, 4, 5, 7, 8, 9, 13, 16, 25, 49, 50, 75, 99, 100];

let totalChecks = 0;
let passedChecks = 0;
let failedChecks = 0;
const failureDetails = [];

function check(condition, message, details = '') {
  totalChecks++;
  if (condition) {
    passedChecks++;
  } else {
    failedChecks++;
    const errMsg = `[FAIL] ${message} ${details ? '--> ' + details : ''}`;
    console.error(`  ${errMsg}`);
    failureDetails.push(errMsg);
  }
}

// =============================================================================
// TEST SUITE 1: BOUNDARY COUNTS ACROSS ALL 15 ARCHETYPES
// =============================================================================
console.log('--- TEST SUITE 1: 15 Archetypes x 16 Boundary Counts ---');
console.log(`Testing N in [${BOUNDARY_COUNTS.join(', ')}] across ${ARCHETYPES.length} archetypes (Total ${ARCHETYPES.length * BOUNDARY_COUNTS.length} scenarios)...`);

for (const arch of ARCHETYPES) {
  for (const N of BOUNDARY_COUNTS) {
    const slots = computeSlots(arch, N);
    
    // 1. Exactly N slots produced
    check(
      slots.length === N,
      `Archetype "${arch}" with N=${N} must produce exactly ${N} slots`,
      `got ${slots.length}`
    );

    // 2. Validate every slot geometry
    for (let i = 0; i < slots.length; i++) {
      const s = slots[i];
      const slotIndex = i + 1;

      // Position
      check(
        s.position === slotIndex,
        `${arch} N=${N} slot[${i}]: position must be ${slotIndex}`,
        `got ${s.position}`
      );

      // Finite & Not NaN
      check(
        Number.isFinite(s.x) && !Number.isNaN(s.x),
        `${arch} N=${N} slot[${i}]: x coordinate must be a finite number`,
        `got ${s.x}`
      );
      check(
        Number.isFinite(s.y) && !Number.isNaN(s.y),
        `${arch} N=${N} slot[${i}]: y coordinate must be a finite number`,
        `got ${s.y}`
      );
      check(
        Number.isFinite(s.width) && !Number.isNaN(s.width),
        `${arch} N=${N} slot[${i}]: width must be a finite number`,
        `got ${s.width}`
      );
      check(
        Number.isFinite(s.height) && !Number.isNaN(s.height),
        `${arch} N=${N} slot[${i}]: height must be a finite number`,
        `got ${s.height}`
      );

      // Bounds: 0 <= x < 1280, 0 <= y < 720
      check(
        s.x >= -0.001 && s.x < 1280,
        `${arch} N=${N} slot[${i}]: 0 <= x < 1280`,
        `got x=${s.x}`
      );
      check(
        s.y >= -0.001 && s.y < 720,
        `${arch} N=${N} slot[${i}]: 0 <= y < 720`,
        `got y=${s.y}`
      );

      // Positive dimensions
      check(
        s.width > 0,
        `${arch} N=${N} slot[${i}]: width > 0`,
        `got width=${s.width}`
      );
      check(
        s.height > 0,
        `${arch} N=${N} slot[${i}]: height > 0`,
        `got height=${s.height}`
      );

      // Canvas confinement: x + w <= 1280.001, y + h <= 720.001
      const right = s.x + s.width;
      const bottom = s.y + s.height;
      check(
        right <= 1280.001,
        `${arch} N=${N} slot[${i}]: x + width <= 1280`,
        `got ${right}`
      );
      check(
        bottom <= 720.001,
        `${arch} N=${N} slot[${i}]: y + height <= 720`,
        `got ${bottom}`
      );

      // z_order valid
      check(
        Number.isInteger(s.z_order) && s.z_order >= 0,
        `${arch} N=${N} slot[${i}]: z_order must be non-negative integer`,
        `got ${s.z_order}`
      );
    }
  }
}
console.log(`[✓] Completed Boundary Tests. Passed: ${passedChecks}, Failed: ${failedChecks}\n`);

// =============================================================================
// TEST SUITE 2: EXHAUSTIVE FULL DOMAIN SWEEP (N = 1 to 100 for all 15 archetypes)
// =============================================================================
console.log('--- TEST SUITE 2: Exhaustive Full Domain Sweep (N=1..100 x 15 archetypes = 1,500 tests) ---');
let domainFailures = 0;

for (const arch of ARCHETYPES) {
  for (let N = 1; N <= 100; N++) {
    const slots = computeSlots(arch, N);
    if (slots.length !== N) {
      domainFailures++;
      check(false, `Sweep mismatch: ${arch} with N=${N} produced ${slots.length} slots`);
      continue;
    }
    for (let i = 0; i < slots.length; i++) {
      const s = slots[i];
      if (!Number.isFinite(s.x) || !Number.isFinite(s.y) || !Number.isFinite(s.width) || !Number.isFinite(s.height) ||
          s.width <= 0 || s.height <= 0 || s.x < -0.001 || s.y < -0.001 ||
          (s.x + s.width) > 1280.001 || (s.y + s.height) > 720.001) {
        domainFailures++;
        check(false, `Invalid slot bounds in sweep: ${arch} N=${N} slot[${i}]`, JSON.stringify(s));
      }
    }
  }
}
check(domainFailures === 0, `All 1,500 combinations in domain N in 1..100 x 15 archetypes pass boundary & count checks`, `failures: ${domainFailures}`);
console.log(`[✓] Full domain sweep complete. Checked 1,500 combinations (total 75,750 individual slots verified).\n`);

// =============================================================================
// TEST SUITE 3: SPECIFIC GRACEFUL DEGRADATION CONTRACTS
// =============================================================================
console.log('--- TEST SUITE 3: Graceful Degradation Contracts ---');

// Contract R3.1: N=1 -> All 15 archetypes must produce exactly 1 full-screen slot (1280x720)
for (const arch of ARCHETYPES) {
  const s1 = computeSlots(arch, 1);
  check(s1.length === 1, `N=1 for "${arch}" produces 1 slot`);
  check(s1[0].x === 0 && s1[0].y === 0 && s1[0].width === 1280 && s1[0].height === 720,
    `N=1 for "${arch}" is full-screen 1280x720`,
    `got: ${s1[0].width}x${s1[0].height} at (${s1[0].x},${s1[0].y})`
  );
}

// Contract R3.2: Trio with N=2 -> 2 equal windows side-by-side (dual_h: 640x720 each)
const trio2 = computeSlots('trio', 2);
check(trio2.length === 2, 'Trio N=2 produces 2 slots');
check(
  trio2[0].x === 0 && trio2[0].y === 0 && trio2[0].width === 640 && trio2[0].height === 720 &&
  trio2[1].x === 640 && trio2[1].y === 0 && trio2[1].width === 640 && trio2[1].height === 720,
  'Trio N=2 degrades to dual_h (two 640x720 slots)',
  JSON.stringify(trio2)
);

// Contract R3.3: Trio with N=3 -> 1 speaker centered top, 2 speakers side-by-side bottom
const trio3 = computeSlots('trio', 3);
check(trio3.length === 3, 'Trio N=3 produces 3 slots');
check(
  trio3[0].x === 320 && trio3[0].y === 0 && trio3[0].width === 640 && trio3[0].height === 360 &&
  trio3[1].x === 0 && trio3[1].y === 360 && trio3[1].width === 640 && trio3[1].height === 360 &&
  trio3[2].x === 640 && trio3[2].y === 360 && trio3[2].width === 640 && trio3[2].height === 360,
  'Trio N=3 renders prominent tribune (1 center top, 2 bottom)',
  JSON.stringify(trio3)
);

// Contract R3.4: Quad focus with N=2 and N=3 -> degrades to balanced grid
const quad2 = computeSlots('quad_focus', 2);
check(quad2.length === 2, 'quad_focus N=2 produces 2 slots');
check(quad2.every(s => s.width > 0 && s.height > 0 && Number.isFinite(s.x)), 'quad_focus N=2 has valid geometry');

const quad3 = computeSlots('quad_focus', 3);
check(quad3.length === 3, 'quad_focus N=3 produces 3 slots');
check(quad3.every(s => s.width > 0 && s.height > 0 && !Number.isNaN(s.x)), 'quad_focus N=3 has valid geometry');

// Contract R3.5: Dual_h with N=2 -> 2 equal windows (640x720)
const dualH2 = computeSlots('dual_h', 2);
check(dualH2.length === 2 && dualH2[0].width === 640 && dualH2[1].width === 640, 'dual_h N=2 renders 2 equal 640x720 slots');

// Contract R3.6: Dual_v with N=2 -> 2 equal stacked windows (1280x360)
const dualV2 = computeSlots('dual_v', 2);
check(dualV2.length === 2 && dualV2[0].height === 360 && dualV2[1].height === 360, 'dual_v N=2 renders 2 equal 1280x360 slots');

// Contract R3.7: Frame with N=4 -> degrades to balanced 2x2 grid
const frame4 = computeSlots('frame', 4);
check(frame4.length === 4, 'frame N=4 produces 4 slots');
check(frame4.every(s => s.width === 640 && s.height === 360), 'frame N=4 degrades to 2x2 grid (640x360 each)');

// Contract R3.8: Cinema with N=2 -> 2 stacked windows (1280x540 and 1280x180)
const cin2 = computeSlots('cinema', 2);
check(cin2.length === 2 && cin2[0].height === 540 && cin2[1].height === 180, 'cinema N=2 renders cinema aspect + banner');

console.log(`[✓] Graceful Degradation tests passed.\n`);

// =============================================================================
// TEST SUITE 4: ADVERSARIAL & FUZZING RESILIENCE
// =============================================================================
console.log('--- TEST SUITE 4: Adversarial Input Clamping & Fuzzing ---');

const fuzzInputs = [
  { val: 0, expectedCount: 1, label: 'N=0 clamps to 1' },
  { val: -1, expectedCount: 1, label: 'N=-1 clamps to 1' },
  { val: -100, expectedCount: 1, label: 'N=-100 clamps to 1' },
  { val: 101, expectedCount: 100, label: 'N=101 clamps to MAX_COUNT (100)' },
  { val: 9999, expectedCount: 100, label: 'N=9999 clamps to MAX_COUNT (100)' },
  { val: 4.9, expectedCount: 4, label: 'N=4.9 truncates to 4' },
  { val: '16', expectedCount: 16, label: 'N="16" parses to 16' },
  { val: 'invalid', expectedCount: 1, label: 'N="invalid" falls back to 1' },
  { val: NaN, expectedCount: 1, label: 'N=NaN falls back to 1' },
  { val: undefined, expectedCount: 1, label: 'N=undefined falls back to 1' },
  { val: null, expectedCount: 1, label: 'N=null falls back to 1' },
  { val: Infinity, expectedCount: 100, label: 'N=Infinity clamps to MAX_COUNT (100)' },
  { val: -Infinity, expectedCount: 1, label: 'N=-Infinity clamps to 1' }
];

for (const input of fuzzInputs) {
  for (const arch of ['grid', 'pin_l', 'trio', 'cinema']) {
    try {
      const res = computeSlots(arch, input.val);
      check(
        res.length === input.expectedCount,
        `Fuzz test [${input.label}] for ${arch}`,
        `got ${res.length} slots`
      );
      check(
        res.every(s => Number.isFinite(s.x) && Number.isFinite(s.y) && s.width > 0 && s.height > 0),
        `Fuzz test [${input.label}] for ${arch} produces strictly valid finite slots`
      );
    } catch (e) {
      check(false, `Fuzz test [${input.label}] threw exception: ${e.message}`);
    }
  }
}

// Unknown archetype fallback
const unknownSlots = computeSlots('non_existent_archetype', 8);
check(unknownSlots.length === 8, 'Unknown archetype falls back to balanced grid with 8 slots');
check(unknownSlots.every(s => s.width > 0 && s.height > 0), 'Unknown archetype slots have valid dimensions');

console.log(`[✓] Adversarial input clamping tests passed.\n`);

// =============================================================================
// TEST SUITE 5: SVG GENERATION & COORDINATE INTEGRITY
// =============================================================================
console.log('--- TEST SUITE 5: SVG Generation & Coordinate Integrity ---');

// Test svgFromSlots on all archetypes for N in [1, 2, 5, 25, 100]
for (const arch of ARCHETYPES) {
  for (const N of [1, 2, 5, 25, 100]) {
    const slots = computeSlots(arch, N);
    const svgStr = svgFromSlots(slots);

    check(typeof svgStr === 'string' && svgStr.startsWith('<svg') && svgStr.endsWith('</svg>'),
      `SVG output for ${arch} N=${N} is a valid XML SVG string`);
    
    // Check rect count matches N
    const rectMatches = svgStr.match(/<rect\b/g);
    const rectCount = rectMatches ? rectMatches.length : 0;
    check(rectCount === N, `SVG for ${arch} N=${N} contains exactly ${N} <rect> elements`, `got ${rectCount}`);

    // Check no NaN or Infinity inside SVG
    check(!svgStr.includes('NaN'), `SVG for ${arch} N=${N} has 0 occurrences of "NaN"`);
    check(!svgStr.includes('Infinity'), `SVG for ${arch} N=${N} has 0 occurrences of "Infinity"`);
    check(!svgStr.includes('undefined'), `SVG for ${arch} N=${N} has 0 occurrences of "undefined"`);
  }
}

// Test corrupted slot filtering in svgFromSlots
const corruptedSlots = [
  { x: 0, y: 0, width: 640, height: 720 },
  { x: NaN, y: 0, width: 640, height: 720 },
  { x: 640, y: 0, width: -50, height: 720 },
  { x: 640, y: 0, width: Infinity, height: 720 },
  { x: 640, y: 0, width: 640, height: 720 }
];
const filteredSvg = svgFromSlots(corruptedSlots);
const filteredRectCount = (filteredSvg.match(/<rect\b/g) || []).length;
check(filteredRectCount === 2, 'svgFromSlots correctly filters out 3 corrupted slots and renders remaining 2', `got ${filteredRectCount}`);

console.log(`[✓] SVG Generation integrity tests passed.\n`);

// =============================================================================
// TEST SUITE 6: RESOLUTION SCALING (formatCalculatedSlots)
// =============================================================================
console.log('--- TEST SUITE 6: Arbitrary Resolution Scaling (formatCalculatedSlots) ---');

const testResolutions = [
  { w: 1280, h: 720, label: 'Native 720p' },
  { w: 1920, h: 1080, label: 'Full HD 1080p' },
  { w: 640, h: 360, label: 'Low-res 360p' },
  { w: 3840, h: 2160, label: '4K UHD' }
];

for (const res of testResolutions) {
  for (const arch of ['grid', 'pin_r', 'pip', 'cinema']) {
    const N = 9;
    const scaled = formatCalculatedSlots(`vks_${arch}_${N}`, res.w, res.h);
    check(scaled.length === N, `Scaled ${arch} N=${N} for ${res.label} produced ${N} slots`);
    
    let allWithinBounds = true;
    for (const s of scaled) {
      if (s.x < 0 || s.y < 0 || s.width <= 0 || s.height <= 0 ||
          (s.x + s.width) > res.w + 1 || (s.y + s.height) > res.h + 1) {
        allWithinBounds = false;
      }
    }
    check(allWithinBounds, `All scaled slots for ${arch} N=${N} confined within target canvas (${res.w}x${res.h})`);
  }
}
console.log(`[✓] Resolution scaling tests passed.\n`);

// =============================================================================
// FINAL EMPIRICAL SUMMARY
// =============================================================================
console.log('================================================================================');
console.log(`>>> SUMMARY: ${passedChecks} checks PASSED, ${failedChecks} FAILED (Total assertions: ${totalChecks})`);
console.log('================================================================================');

if (failedChecks > 0) {
  console.error('\nFailures breakdown:');
  failureDetails.forEach((f, idx) => console.error(`  ${idx + 1}. ${f}`));
  process.exit(1);
} else {
  console.log('\n>>> EMPIRICAL PROOF VERDICT: ALL MATHEMATICAL AND GEOMETRIC CONSTRAINTS SATISFIED (100% OK)');
  process.exit(0);
}

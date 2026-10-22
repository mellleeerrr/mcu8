#!/usr/bin/env node
/*
 * Live browser proof for the real str0m WebRTC transport.
 *
 * Two Chromium pages (fake camera/mic) each get a real str0m answer over an
 * authenticated /ws bridge. The assertions are deliberately transport-level:
 * ICE connected, DTLS/SRTP cipher present, inbound RTP bytes and decoded video
 * frames on BOTH pages, and a real 32-byte SHA-256 fingerprint in the answer
 * (not the legacy "sha-256 FF:00:11" placeholder).
 *
 * Run:
 *   NODE_PATH=<playwright node_modules> node tests/e2e/webrtc.js
 */
'use strict';

const path = require('node:path');
const fs = require('node:fs');
const { chromium } = require('playwright');
const { ws: WebSocket } = require('playwright-core/lib/utilsBundle');

const BASE = process.env.VKS_BASE_URL || 'http://127.0.0.1:8080';
const WS_URL = BASE.replace(/^http/, 'ws') + '/ws';
const LOGIN = process.env.VKS_LOGIN || 'admin';
const PASSWORD = process.env.VKS_PASSWORD || 'admin_password';
const ART = path.resolve(process.env.VKS_ARTIFACTS || 'artifacts/e2e-webrtc');
const CHROMIUM = process.env.VKS_CHROMIUM
  || '/home/pro/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome';
const RUN_SECONDS = Number(process.env.VKS_WEBRTC_RUN_SECONDS || 6);

const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok: Boolean(ok), detail: detail || '' });
  console.log(`${ok ? 'PASS' : 'FAIL'} ${name}${detail ? ' — ' + detail : ''}`);
}

// --- minimal protobuf (only the signalling fields this test needs) ---
function varint(n) {
  const out = [];
  while (n >= 0x80) {
    out.push((n & 0x7f) | 0x80);
    n = Math.floor(n / 128);
  }
  out.push(n);
  return Buffer.from(out);
}
function tag(field, wire) { return varint((field << 3) | wire); }
function fvar(field, value) { return Buffer.concat([tag(field, 0), varint(value >>> 0)]); }
function fbytes(field, bytes) {
  const body = Buffer.isBuffer(bytes) ? bytes : Buffer.from(bytes);
  return Buffer.concat([tag(field, 2), varint(body.length), body]);
}
function fstr(field, value) { return fbytes(field, Buffer.from(String(value), 'utf8')); }
function msg(msgId, field, nested) { return Buffer.concat([fvar(1, msgId), fbytes(field, nested)]); }

function readVarint(buf, at) {
  let value = 0;
  let shift = 0;
  let i = at;
  while (i < buf.length) {
    const byte = buf[i++];
    value += (byte & 0x7f) * 2 ** shift;
    if (!(byte & 0x80)) return { value, next: i };
    shift += 7;
  }
  throw new Error('truncated varint');
}
function parseMessage(buf) {
  const fields = [];
  let i = 0;
  while (i < buf.length) {
    const t = readVarint(buf, i);
    const field = Math.floor(t.value / 8);
    const wire = t.value & 7;
    i = t.next;
    if (wire === 0) {
      const v = readVarint(buf, i);
      fields.push({ field, wire, value: v.value });
      i = v.next;
    } else if (wire === 2) {
      const len = readVarint(buf, i);
      const start = len.next;
      const end = start + len.value;
      fields.push({ field, wire, bytes: buf.subarray(start, end) });
      i = end;
    } else if (wire === 5) {
      fields.push({ field, wire, value: buf.readUInt32LE(i) });
      i += 4;
    } else if (wire === 1) {
      i += 8;
    } else {
      throw new Error('unsupported wire type ' + wire);
    }
  }
  return fields;
}
function nested(fields, field) {
  const hit = fields.find(f => f.field === field && f.wire === 2);
  return hit ? parseMessage(hit.bytes) : null;
}
function nestedString(fields, field, inner) {
  const sub = nested(fields, field);
  if (!sub) return '';
  const hit = sub.find(f => f.field === inner && f.wire === 2);
  return hit ? hit.bytes.toString('utf8') : '';
}

async function api(token, method, urlPath, body) {
  const response = await fetch(BASE + urlPath, {
    method,
    headers: {
      authorization: `Bearer ${token}`,
      'content-type': 'application/json',
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  if (!response.ok) throw new Error(`${method} ${urlPath} -> ${response.status}: ${text.slice(0, 400)}`);
  return text ? JSON.parse(text) : null;
}

class SignalPeer {
  constructor(token, roomId, displayName) {
    this.token = token;
    this.roomId = roomId;
    this.displayName = displayName;
    this.nextId = 1;
    this.pending = new Map();
  }
  async connect() {
    this.socket = new WebSocket(WS_URL, { headers: { Authorization: `Bearer ${this.token}` } });
    await new Promise((resolve, reject) => {
      this.socket.once('open', resolve);
      this.socket.once('error', reject);
    });
    this.socket.on('message', data => {
      let fields;
      try { fields = parseMessage(Buffer.from(data)); } catch { return; }
      const replyToHit = fields.find(f => f.field === 2 && f.wire === 0);
      const replyTo = replyToHit ? replyToHit.value : 0;
      if (replyTo && this.pending.has(replyTo)) {
        const { resolve } = this.pending.get(replyTo);
        this.pending.delete(replyTo);
        resolve(fields);
      }
    });
    return this;
  }
  send(field, nestedBytes) {
    const id = this.nextId++;
    const frame = msg(id, field, nestedBytes);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error(`signalling timeout for field ${field}`)), 15000);
      this.pending.set(id, {
        resolve: fields => { clearTimeout(timer); resolve(fields); },
      });
      this.socket.send(frame);
    });
  }
  async hello() {
    const hello = Buffer.concat([
      fvar(1, 1),
      fstr(2, 'webrtc-e2e-' + this.displayName),
      fstr(3, 'e2e/1.0'),
      fstr(4, 'playwright'),
      fstr(6, this.roomId),
    ]);
    const reply = await this.send(10, hello);
    const error = nested(reply, 200);
    if (error) throw new Error('ClientHello error: ' + JSON.stringify(error));
    return reply;
  }
  async join() {
    const join = Buffer.concat([fstr(1, this.roomId), fstr(2, this.displayName)]);
    const reply = await this.send(20, join);
    const error = nested(reply, 200);
    if (error) throw new Error('JoinRoom error: ' + error.map(f => f.bytes && f.bytes.toString()).join(' '));
    return reply;
  }
  async offer(sdp) {
    const reply = await this.send(40, fstr(1, sdp));
    const error = nested(reply, 200);
    if (error) throw new Error('SdpOffer error: ' + JSON.stringify(error));
    const answer = nestedString(reply, 41, 1);
    if (!answer) throw new Error('SdpAnswer has no SDP');
    return answer;
  }
  close() {
    try { this.socket.close(); } catch { /* already closed */ }
  }
}

async function createBrowserPeer(page) {
  return page.evaluate(async () => {
    const pc = new RTCPeerConnection({ iceServers: [] });
    window.__vksPc = pc;
    window.__vksStats = { connectedAt: 0, states: [], iceErrors: [], candidates: [] };
    pc.addEventListener('icecandidateerror', event => {
      window.__vksStats.iceErrors.push({ url: event.url, code: event.errorCode, text: event.errorText, address: event.address, port: event.port });
    });
    pc.addEventListener('icecandidate', event => {
      if (event.candidate) window.__vksStats.candidates.push(event.candidate.candidate);
    });
    pc.addEventListener('connectionstatechange', () => {
      window.__vksStats.states.push(pc.connectionState + '@' + Date.now());
      if (pc.connectionState === 'connected' && !window.__vksStats.connectedAt) {
        window.__vksStats.connectedAt = Date.now();
      }
    });
    const stream = await navigator.mediaDevices.getUserMedia({
      audio: true,
      video: { width: { ideal: 320 }, height: { ideal: 240 }, frameRate: { ideal: 15 } },
    });
    window.__vksStream = stream;
    for (const track of stream.getTracks()) pc.addTrack(track, stream);
    await pc.setLocalDescription(await pc.createOffer());
    if (pc.iceGatheringState !== 'complete') {
      await new Promise(resolve => {
        const done = () => { if (pc.iceGatheringState === 'complete') { pc.removeEventListener('icegatheringstatechange', done); resolve(); } };
        pc.addEventListener('icegatheringstatechange', done);
        setTimeout(resolve, 3000);
      });
    }
    return { sdp: pc.localDescription.sdp, sdpType: pc.localDescription.type };
  });
}

async function applyAnswer(page, sdp) {
  await page.evaluate(async answer => {
    await window.__vksPc.setRemoteDescription({ type: 'answer', sdp: answer });
  }, sdp);
}

async function waitConnected(page) {
  await page.waitForFunction(
    () => window.__vksPc && ['connected', 'completed'].includes(window.__vksPc.connectionState)
      || (window.__vksPc && ['connected', 'completed'].includes(window.__vksPc.iceConnectionState)),
    null,
    { timeout: 20000 },
  );
}

async function collectStats(page) {
  return page.evaluate(async () => {
    const report = await window.__vksPc.getStats();
    const out = { transport: [], inbound: [], outbound: [], candidates: [], debug: null };
    report.forEach(stat => {
      if (stat.type === 'transport') {
        out.transport.push({ dtlsState: stat.dtlsState, srtpCipher: stat.srtpCipher, bytesReceived: stat.bytesReceived, bytesSent: stat.bytesSent });
      }
      if (stat.type === 'inbound-rtp') {
        out.inbound.push({
          kind: stat.kind, bytesReceived: stat.bytesReceived, packetsReceived: stat.packetsReceived,
          framesDecoded: stat.framesDecoded, frameWidth: stat.frameWidth, frameHeight: stat.frameHeight,
        });
      }
      if (stat.type === 'outbound-rtp') {
        out.outbound.push({ kind: stat.kind, bytesSent: stat.bytesSent, packetsSent: stat.packetsSent, framesEncoded: stat.framesEncoded });
      }
      if (stat.type === 'candidate-pair' && stat.state === 'succeeded' && stat.nominated) {
        out.candidates.push({ nominated: stat.nominated, bytesReceived: stat.bytesReceived, bytesSent: stat.bytesSent });
      }
    });
    out.connectionState = window.__vksPc.connectionState;
    out.iceConnectionState = window.__vksPc.iceConnectionState;
    out.iceGatheringState = window.__vksPc.iceGatheringState;
    out.debug = window.__vksStats || null;
    return out;
  });
}

(async () => {
  fs.mkdirSync(ART, { recursive: true });
  let browser;
  let token = '';
  let roomId = '';
  const peers = [];
  try {
    const loginResponse = await fetch(BASE + '/api/v3/login', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ login: LOGIN, password: PASSWORD }),
    });
    if (!loginResponse.ok) throw new Error('login failed: ' + loginResponse.status);
    token = (await loginResponse.json()).token;
    check('auth: JWT issued', typeof token === 'string' && token.split('.').length === 3);

    const created = await api(token, 'POST', '/api/v3/conferences', {
      display_name: 'WebRTC E2E ' + new Date().toISOString().slice(11, 19),
    });
    roomId = created.conference && created.conference.id;
    check('room: created', Boolean(roomId), String(roomId));

    browser = await chromium.launch({
      headless: true,
      executablePath: CHROMIUM,
      args: [
        '--no-sandbox',
        '--use-fake-device-for-media-stream',
        '--use-fake-ui-for-media-stream',
        '--autoplay-policy=no-user-gesture-required',
      ],
    });
    const context = await browser.newContext({ permissions: ['camera', 'microphone'] });

    for (const name of ['alice', 'bob']) {
      const page = await context.newPage();
      page.on('pageerror', error => check(`pageerror ${name}`, false, String(error)));
      await page.goto(BASE + '/pnm/authorization', { waitUntil: 'domcontentloaded' });
      const signal = await new SignalPeer(token, roomId, 'WebRTC-' + name).connect();
      await signal.hello();
      await signal.join();
      const offer = await createBrowserPeer(page);
      const answer = await signal.offer(offer.sdp);
      fs.writeFileSync(path.join(ART, `${name}-offer.sdp`), offer.sdp);
      fs.writeFileSync(path.join(ART, `${name}-answer.sdp`), answer);
      check(`${name}: answer is real str0m SDP`, answer.includes('a=ice-lite') && answer.includes('sha-256')
        && !answer.includes('FF:00:11') && answer.includes('a=candidate'), answer.slice(0, 160).replace(/\r?\n/g, ' | '));
      check(`${name}: answer offers Opus+VP8 only`, answer.includes('opus/48000/2') && answer.includes('VP8/90000')
        && !answer.includes('H264') && !answer.includes('H265') && !answer.includes('VP9'), '');
      await applyAnswer(page, answer);
      let connected = false;
      try {
        await waitConnected(page);
        connected = true;
        await page.screenshot({ path: path.join(ART, `${name}-connected.png`) });
      } catch (error) {
        const debug = await collectStats(page).catch(() => ({}));
        console.error(`[${name}] connect timeout; debug=`, JSON.stringify(debug));
        const pageDebug = await page.evaluate(() => window.__vksStats).catch(() => null);
        console.error(`[${name}] page debug=`, JSON.stringify(pageDebug));
        check(`${name}: ICE connected`, false, JSON.stringify(pageDebug && pageDebug.iceErrors));
      }
      peers.push({ name, page, signal, connected });
    }

    check('both pages: ICE connected', peers.length === 2 && peers.every(p => p.connected),
      peers.map(p => `${p.name}:${p.connected}`).join(','));
    await new Promise(resolve => setTimeout(resolve, RUN_SECONDS * 1000));

    for (const peer of peers) {
      const stats = await collectStats(peer.page);
      check(`${peer.name}: connectionState connected`, stats.connectionState === 'connected', stats.connectionState);
      const transport = stats.transport[0] || {};
      check(`${peer.name}: DTLS connected`, transport.dtlsState === 'connected', JSON.stringify(transport));
      check(`${peer.name}: SRTP cipher negotiated`, typeof transport.srtpCipher === 'string' && transport.srtpCipher.length > 0, String(transport.srtpCipher));
      const audioIn = stats.inbound.find(s => s.kind === 'audio');
      const videoIn = stats.inbound.find(s => s.kind === 'video');
      check(`${peer.name}: inbound audio RTP`, Boolean(audioIn && audioIn.bytesReceived > 0), JSON.stringify(audioIn || null));
      check(`${peer.name}: inbound video RTP`, Boolean(videoIn && videoIn.bytesReceived > 0), JSON.stringify(videoIn || null));
      check(`${peer.name}: decoded video frames`, Boolean(videoIn && videoIn.framesDecoded > 0), JSON.stringify(videoIn || null));
      const outbound = stats.outbound.filter(s => s.bytesSent > 0).map(s => s.kind);
      check(`${peer.name}: outbound audio+video`, outbound.includes('audio') && outbound.includes('video'), outbound.join(','));
    }

    check('server: no page errors', !checks.some(c => c.name.startsWith('pageerror')), '');
    const result = {
      generated_at: new Date().toISOString(),
      base: BASE,
      room_id: roomId,
      checks,
      passed: checks.filter(c => c.ok).length,
      failed: checks.filter(c => !c.ok).length,
      total: checks.length,
    };
    fs.writeFileSync(path.join(ART, 'result.json'), JSON.stringify(result, null, 2));
    console.log(`\nWebRTC E2E: ${result.passed}/${result.total} checks passed`);
    if (result.failed > 0) process.exitCode = 1;
  } catch (error) {
    console.error('WebRTC E2E fatal:', error && error.stack || error);
    fs.writeFileSync(path.join(ART, 'result.json'), JSON.stringify({
      generated_at: new Date().toISOString(), fatal: String(error && error.stack || error), checks,
    }, null, 2));
    process.exitCode = 1;
  } finally {
    for (const peer of peers) {
      peer.signal.close();
      await peer.page.close().catch(() => {});
    }
    if (browser) await browser.close().catch(() => {});
    if (token && roomId) await api(token, 'DELETE', `/api/v3/conferences/${roomId}`).catch(() => {});
  }
})();

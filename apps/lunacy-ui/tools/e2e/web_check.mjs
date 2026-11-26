// Headless Chrome CDP check for the lunacy web build (WASM + WebSocket + fetch).
// Usage: node web_check.mjs <cdpPort> <httpPort> <token> <runId> <coordsJson>

const [cdpPort, httpPort, token, runId, coordsRaw] = process.argv.slice(2);
if (!cdpPort || !httpPort || !token || !runId || !coordsRaw) {
  console.error('usage: web_check.mjs <cdpPort> <httpPort> <token> <runId> <coordsJson>');
  process.exit(2);
}
const coords = JSON.parse(coordsRaw);
const base = `http://127.0.0.1:${httpPort}`;

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

const list = await (await fetch(`http://127.0.0.1:${cdpPort}/json/list`)).json();
const page = Array.isArray(list) ? list.find((t) => t.type === 'page') : null;
if (!page) {
  throw new Error(`no CDP page target found: ${JSON.stringify(list)}`);
}

const ws = new WebSocket(page.webSocketDebuggerUrl);
let seq = 0;
const pending = new Map();
const requests = new Map();
const responses = [];
const wsSockets = [];
const wsFrames = [];
const consoleErrors = [];

const resetNetwork = () => {
  responses.length = 0;
  wsSockets.length = 0;
  wsFrames.length = 0;
};

ws.onmessage = (ev) => {
  const msg = JSON.parse(ev.data);
  if (msg.id && pending.has(msg.id)) {
    const { resolve, reject } = pending.get(msg.id);
    pending.delete(msg.id);
    if (msg.error) reject(new Error(`${msg.error.message} (${msg.error.code})`));
    else resolve(msg.result);
    return;
  }
  if (msg.method === 'Network.requestWillBeSent') {
    const { requestId, request } = msg.params;
    requests.set(requestId, {
      url: request.url,
      method: request.method,
      headers: request.headers || {},
    });
  } else if (msg.method === 'Network.responseReceived') {
    const { requestId, response } = msg.params;
    const req = requests.get(requestId) || { url: response.url, method: 'GET', headers: {} };
    responses.push({
      url: response.url,
      method: req.method,
      status: response.status,
      hasAuth: Boolean(req.headers.Authorization || req.headers.authorization),
    });
  } else if (msg.method === 'Network.webSocketCreated') {
    wsSockets.push({
      requestId: msg.params.requestId,
      url: msg.params.url,
    });
  } else if (msg.method === 'Network.webSocketFrameReceived') {
    const payload = msg.params?.response?.payloadData;
    if (typeof payload === 'string') {
      try {
        wsFrames.push(JSON.parse(payload));
      } catch {
        // Ignore non-JSON frames.
      }
    }
  } else if (msg.method === 'Runtime.consoleAPICalled' && msg.params.type === 'error') {
    consoleErrors.push(msg.params.args.map((a) => a.value ?? a.description ?? '').join(' '));
  } else if (msg.method === 'Runtime.exceptionThrown') {
    consoleErrors.push(msg.params.exceptionDetails?.text || 'exceptionThrown');
  }
};

await new Promise((r, rej) => {
  ws.onopen = r;
  ws.onerror = rej;
});

const send = (method, params = {}) =>
  new Promise((resolve, reject) => {
    const id = ++seq;
    pending.set(id, { resolve, reject });
    ws.send(JSON.stringify({ id, method, params }));
  });

const waitFor = async (pred, timeoutMs, label) => {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (await pred()) return;
    await sleep(100);
  }
  throw new Error(
    `timeout waiting for ${label}; responses=${JSON.stringify(responses.slice(-10))}; wsSockets=${JSON.stringify(wsSockets.slice(-5))}; wsFrames=${JSON.stringify(wsFrames.slice(-5).map((f) => f.type))}`,
  );
};

const clickAt = async (pt) => {
  await send('Input.dispatchMouseEvent', {
    type: 'mouseMoved',
    x: pt.x,
    y: pt.y,
    button: 'none',
    clickCount: 0,
  });
  await send('Input.dispatchMouseEvent', {
    type: 'mousePressed',
    x: pt.x,
    y: pt.y,
    button: 'left',
    clickCount: 1,
  });
  await send('Input.dispatchMouseEvent', {
    type: 'mouseReleased',
    x: pt.x,
    y: pt.y,
    button: 'left',
    clickCount: 1,
  });
};

await send('Network.enable');
await send('Page.enable');
await send('Runtime.enable');
await send('Emulation.setDeviceMetricsOverride', {
  width: 1180,
  height: 760,
  deviceScaleFactor: 1,
  mobile: false,
});

// 1) Initial load without token -> WebGPU boots and /api/ returns 401 or WS reports auth_error
await send('Page.navigate', { url: `${base}/` });
await waitFor(
  async () => {
    const r = await send('Runtime.evaluate', {
      expression: "document.querySelector('#status')?.textContent || 'GONE'",
    });
    const txt = r.result?.value;
    if (txt && txt.startsWith('Lunacy could not start:')) {
      throw new Error(`web boot failed: ${txt}`);
    }
    const got401 = responses.some((x) => x.url.includes('/api/') && x.status === 401);
    const gotWsAuthErr = wsFrames.some((f) => f.type === 'auth_error');
    return txt === 'GONE' && (got401 || gotWsAuthErr);
  },
  15000,
  'initial unauthorized /api/ or WS auth failure',
);

// 2) Seed token in localStorage and reload `/` -> WS /api/ws connects + receives snapshot + GET /api/system 200,
//    and does NOT spam periodic GET /api/projects while WS is connected.
//    Navigate through about:blank before resetNetwork() so any in-flight unauthed request from
//    step 1 cannot land in step 2's response log.
await send('Runtime.evaluate', {
  expression: `localStorage.setItem('lunacy.token', ${JSON.stringify(token)})`,
});
await send('Page.navigate', { url: 'about:blank' });
await sleep(100);
resetNetwork();
await send('Page.navigate', { url: `${base}/` });
await waitFor(
  () =>
    wsSockets.some((s) => s.url.includes('/api/ws')) &&
    wsFrames.some((f) => f.type === 'snapshot') &&
    responses.some((x) => x.url.endsWith('/api/system') && x.status === 200 && x.hasAuth),
  15000,
  'authenticated / load (WS snapshot + GET /api/system)',
);
await sleep(2500);
const projectPolls = responses.filter((x) => x.url.endsWith('/api/projects'));
if (projectPolls.length > 1) {
  throw new Error(`expected no periodic GET /api/projects while WS connected, saw ${projectPolls.length}`);
}
const badOnRoot = responses.filter((x) => x.url.includes('/api/') && (x.status < 200 || x.status >= 300 || !x.hasAuth));
if (badOnRoot.length) throw new Error(`unexpected non-2xx/unauthed call on /: ${JSON.stringify(badOnRoot)}`);

// 3) Deep link `/#/runs/<id>` (navigate via about:blank so it performs a fresh WASM boot)
await send('Page.navigate', { url: 'about:blank' });
await sleep(100);
resetNetwork();
await send('Page.navigate', { url: `${base}/#/runs/${encodeURIComponent(runId)}` });
await waitFor(
  () =>
    wsFrames.some((f) => f.type === 'run_updated' && f.run?.id === runId) ||
    responses.some((x) => x.url.includes(`/api/runs/${runId}`) && x.status === 200 && x.hasAuth),
  15000,
  '/#/runs/<id> deep link',
);

// 4) Path deep link `/runs/<id>` -> redirects to `#/runs/<id>` and loads run cleanly
await send('Page.navigate', { url: 'about:blank' });
await sleep(100);
resetNetwork();
await send('Page.navigate', { url: `${base}/runs/${encodeURIComponent(runId)}` });
await waitFor(
  async () => {
    const h = await send('Runtime.evaluate', { expression: 'location.hash' });
    const loaded =
      wsFrames.some((f) => f.type === 'run_updated' && f.run?.id === runId) ||
      responses.some((x) => x.url.includes(`/api/runs/${runId}`) && x.status === 200 && x.hasAuth);
    return h.result?.value === `#/runs/${runId}` && loaded;
  },
  15000,
  '/runs/<id> redirect + run load',
);
const badOnDeep = responses.filter((x) => x.url.includes('/api/') && (x.status < 200 || x.status >= 300 || !x.hasAuth));
if (badOnDeep.length) throw new Error(`bad API call on /runs/${runId}: ${JSON.stringify(badOnDeep)}`);

// 5) Canvas click flow on web: navigate `/` cleanly, open Settings, edit poll_interval -> 150, Save -> PUT /api/config 200
await send('Page.navigate', { url: 'about:blank' });
await sleep(100);
resetNetwork();
await send('Page.navigate', { url: `${base}/` });
await waitFor(
  () =>
    wsFrames.some((f) => f.type === 'snapshot') ||
    responses.some((x) => x.url.endsWith('/api/projects') && x.status === 200),
  15000,
  'reload / before settings click',
);
await sleep(400);
await clickAt(coords.settings);
await waitFor(
  () => responses.some((x) => x.url.endsWith('/api/config') && x.status === 200 && x.method === 'GET'),
  10000,
  'GET /api/config after clicking Settings',
);
await sleep(400);
await clickAt(coords.poll_input);
await sleep(150);
// Ctrl+A, Backspace, type '150'
await send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'a', code: 'KeyA', modifiers: 2, windowsVirtualKeyCode: 65 });
await send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'a', code: 'KeyA', modifiers: 2, windowsVirtualKeyCode: 65 });
await send('Input.dispatchKeyEvent', { type: 'keyDown', key: 'Backspace', code: 'Backspace', windowsVirtualKeyCode: 8 });
await send('Input.dispatchKeyEvent', { type: 'keyUp', key: 'Backspace', code: 'Backspace', windowsVirtualKeyCode: 8 });
await send('Input.insertText', { text: '150' });
await sleep(200);
await clickAt(coords.save);
await waitFor(
  () => responses.some((x) => x.url.endsWith('/api/config') && x.method === 'PUT' && x.status === 200 && x.hasAuth),
  10000,
  'PUT /api/config 200 from web build',
);

if (consoleErrors.length) {
  throw new Error(`browser console/exception errors: ${JSON.stringify(consoleErrors)}`);
}
console.log(
  JSON.stringify({
    ok: true,
    checked: ['/', '/api/ws', '/#/runs/' + runId, '/runs/' + runId, 'PUT /api/config'],
  }),
);
process.exit(0);

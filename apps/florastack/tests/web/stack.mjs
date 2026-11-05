// The browser build's headless test, which `cargo xtask web --test` runs (xtask/src/web.rs) with Playwright's library
// and these variables: FLORASTACK_URL, the site, served with its headers; FLORASTACK_FRAMES, a folder of PNG frames;
// FLORASTACK_DOWNLOADS, where the output goes, as web.png; FLORASTACK_BROWSER, a Playwright channel (chrome, msedge,
// chromium) or a browser's path.
//
// The page stacks the frames as a user would: they come through its file input, the app adds them to a new project,
// then the page's `window.florastack.dispatch` aligns them, stacks them by the pyramid and exports the output, which the
// page downloads as web.png; on WebGPU, it stacks them again on the CPU's threads, web-cpu.png. xtask then compares
// each with the same stack stacked natively.
import { chromium } from 'playwright-core';
import { mkdirSync, readdirSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const url = process.env.FLORASTACK_URL;
const frames = process.env.FLORASTACK_FRAMES;
const downloads = process.env.FLORASTACK_DOWNLOADS;
const browser = process.env.FLORASTACK_BROWSER || 'chrome';
const MINUTE = 60_000;

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
// A page whose main thread is stuck answers nothing, and Playwright's evaluate has no timeout of its own: every call
// into the page gets one, and the whole test a deadline.
setTimeout(() => {
  console.error('the test took more than 15 minutes');
  process.exit(1);
}, 15 * MINUTE).unref();
const within = (promise, ms, what) =>
  Promise.race([promise, sleep(ms).then(() => Promise.reject(new Error(`${what} took more than ${ms / 1000} s: the page doesn't answer`)))]);
const launch = /[\\/]/.test(browser) ? { executablePath: browser } : { channel: browser };
const instance = await chromium.launch({ ...launch, headless: true });
const errors = [];
const timings = {};
try {
  const page = await instance.newPage({ acceptDownloads: true, viewport: { width: 1280, height: 800 } });
  page.on('console', (m) => console.log(`[page ${m.type()}] ${m.text()}`));
  page.on('pageerror', (e) => errors.push(`page error: ${e.message}`));
  page.on('crash', () => errors.push('the page crashed'));
  page.on('worker', (w) => w.on('close', () => {}));

  let start = Date.now();
  await page.goto(url);
  if (!(await within(page.evaluate(() => crossOriginIsolated), 30_000, 'reading crossOriginIsolated'))) {
    throw new Error('the page is not cross-origin isolated: the server sent no COOP and COEP headers');
  }
  await page.waitForFunction(() => window.florastack?.ready?.(), null, { timeout: 2 * MINUTE });
  timings.start = Date.now() - start;
  const threads = await within(page.evaluate(() => navigator.hardwareConcurrency), 30_000, 'reading the cores');
  console.log(`the app runs, ${threads} threads, after ${timings.start} ms`);

  const files = readdirSync(frames).filter((f) => f.endsWith('.png')).sort().map((f) => join(frames, f));
  start = Date.now();
  await page.setInputFiles('#florastack-files', files);
  // The app adds the files picked as frames, in a new project (`project.addFrames`, a job): wait for them all.
  const dispatch = (name, params, ms = 5 * MINUTE) =>
    within(page.evaluate(([n, p]) => window.florastack.dispatch(n, p), [name, params]), ms, name);
  for (;;) {
    if (errors.length) throw new Error(errors.join('\n'));
    const listed = await within(page.evaluate(() => window.florastack.dispatch('project.frames', {}).catch(() => null)), 30_000, 'project.frames');
    const jobs = await dispatch('job.list', {}, 30_000);
    const running = jobs.jobs.filter((j) => !['done', 'failed', 'cancelled'].includes(j.state));
    if (listed && listed.frames.length === files.length && running.length === 0) break;
    if (Date.now() - start > 5 * MINUTE) throw new Error(`the frames weren't added: ${JSON.stringify(listed)}`);
    await sleep(250);
  }
  timings.add = Date.now() - start;
  console.log(`${files.length} frames added in ${timings.add} ms`);

  start = Date.now();
  const aligned = await dispatch('align.run', {});
  timings.align = Date.now() - start;
  console.log(`aligned in ${timings.align} ms: ${JSON.stringify(aligned).slice(0, 200)}`);

  // The device that stacks: WebGPU when the browser has it and its device passes the self-test, which runs on the GPU
  // worker meanwhile; the CPU's threads otherwise.
  let compute;
  for (start = Date.now(); ; await sleep(250)) {
    compute = await dispatch('compute.info', {}, 30_000);
    if (!(compute.reason ?? '').includes('still being tested') || Date.now() - start > 2 * MINUTE) break;
  }
  console.log(`stacking on the ${compute.device}: ${compute.adapter ? JSON.stringify(compute.adapter) : compute.reason}`);

  mkdirSync(downloads, { recursive: true });
  // Stacks by the pyramid and downloads the output as `name`.png: an export to the page's /downloads folder, whose
  // files the page downloads once no job runs.
  const stack = async (name) => {
    let begun = Date.now();
    const stacked = await dispatch('stack.pyramid', {});
    timings[name] = Date.now() - begun;
    console.log(`${name}: stacked in ${timings[name]} ms: ${JSON.stringify(stacked).slice(0, 300)}`);
    const settings = { format: 'png', depth: 'u8', metadata: false, template: name };
    const downloaded = page.waitForEvent('download', { timeout: 2 * MINUTE });
    await dispatch('export.run', { output: stacked.output, folder: `/downloads/${name}`, settings });
    const download = await downloaded;
    await download.saveAs(join(downloads, `${name}.png`));
    console.log(`${name}: downloaded ${download.suggestedFilename()}`);
  };
  await stack('web');
  // On the GPU, the CPU's threads too.
  if (compute.device === 'gpu') {
    await dispatch('compute.set', { device: 'cpu' }, 30_000);
    await stack('web-cpu');
  }
  writeFileSync(join(downloads, 'timings.json'), JSON.stringify({ threads, device: compute.device, adapter: compute.adapter, ...timings }, null, 2));
  if (errors.length) throw new Error(errors.join('\n'));
} finally {
  await instance.close();
}

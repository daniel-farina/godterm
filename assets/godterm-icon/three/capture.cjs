// Render one icon page in headless Chrome (GPU via ANGLE/Metal) and save the 2048x2048 canvas.
// usage: NODE_PATH=<dir with puppeteer-core> node capture.cjs page.html out.png
const puppeteer = require('puppeteer-core');
const path = require('path');
(async () => {
  const [page, out] = process.argv.slice(2);
  const browser = await puppeteer.launch({
    executablePath: '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
    headless: 'new',
    args: ['--use-angle=metal', '--enable-gpu', '--ignore-gpu-blocklist', '--enable-unsafe-swiftshader',
           '--hide-scrollbars', '--allow-file-access-from-files', '--window-size=2048,2048'],
  });
  const p = await browser.newPage();
  await p.setViewport({ width: 2048, height: 2048, deviceScaleFactor: 1 });
  p.on('console', m => console.log('[page]', m.text()));
  p.on('pageerror', e => console.log('[pageerror]', e.message));
  const [file, query] = page.split('?');
  await p.goto('file://' + path.resolve(file) + (query ? '?' + query : ''), { waitUntil: 'load' });
  await p.waitForFunction('window.__ready === true', { timeout: 180000 });
  const data = await p.evaluate(() => document.querySelector('canvas').toDataURL('image/png'));
  require('fs').writeFileSync(out, Buffer.from(data.split(',')[1], 'base64'));
  console.log('saved', out);
  await browser.close();
})().catch(e => { console.error(e); process.exit(1); });

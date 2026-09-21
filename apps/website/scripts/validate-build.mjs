import fs from 'node:fs';
import path from 'node:path';

const root = path.resolve(import.meta.dirname, '..');
const dist = path.join(root, 'dist');
const required = [
  'index.html', 'en/index.html', 'download/index.html', 'en/download/index.html',
  'changelog/index.html', 'en/changelog/index.html',
  'docs/index.html', 'en/docs/index.html',
  'docs/install-macos/index.html', 'en/docs/install-macos/index.html',
  'docs/install-linux/index.html', 'en/docs/install-linux/index.html',
  'docs/model-routing/index.html', 'en/docs/model-routing/index.html',
  'docs/task-routing/index.html', 'en/docs/task-routing/index.html',
  'docs/cli/index.html', 'en/docs/cli/index.html',
  'docs/decision-extensions/index.html', 'en/docs/decision-extensions/index.html',
  'docs/decision-api/index.html', 'en/docs/decision-api/index.html',
  'docs/jev-decider/index.html', 'en/docs/jev-decider/index.html',
  'api/decision.openapi.json', 'install.sh', 'install/standalone.py', '404.html', 'sitemap-index.xml',
];
for (const relative of required) {
  if (!fs.existsSync(path.join(dist, relative))) throw new Error(`missing website output: ${relative}`);
}
const htmlFiles = [];
for (const entry of fs.readdirSync(dist, { recursive: true, withFileTypes: true })) {
  if (entry.isFile() && entry.name.endsWith('.html')) htmlFiles.push(path.join(entry.parentPath, entry.name));
}
for (const file of htmlFiles) {
  const html = fs.readFileSync(file, 'utf8');
  if (html.includes('noindex') || html.includes('官网视觉原型')) throw new Error(`prototype marker leaked into ${file}`);
  for (const match of html.matchAll(/(?:href|src)="(\/[^"?#]+)["?#]/g)) {
    const target = decodeURIComponent(match[1]);
    if (target.startsWith('/releases/')) continue;
    const candidate = target.endsWith('/') ? path.join(dist, target, 'index.html') : path.join(dist, target);
    if (!fs.existsSync(candidate)) throw new Error(`broken local reference in ${file}: ${target}`);
  }
}
console.log(`Validated ${htmlFiles.length} static HTML pages and ${required.length} required outputs.`);

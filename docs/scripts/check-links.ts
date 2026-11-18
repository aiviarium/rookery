// Checks the links between pages: every internal link, every link written as a
// file name and every #heading must point to something the site builds. The
// links in docs.config.ts are checked too. External links are not fetched.
//
// Run with `npm run links`. It exits with 1 when a link is broken.
import { readdirSync, readFileSync } from 'node:fs';
import { join, relative, sep } from 'node:path';
import GithubSlugger from 'github-slugger';
import { printErrors, validateFiles, type ScanResult } from 'next-validate-link';
import config from '../docs.config';

const contentDir = 'content/docs';

function walk(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return walk(path);
    return /\.mdx?$/.test(entry.name) ? [path] : [];
  });
}

/**
 * content/docs/components/tabs.mdx is served at /docs/components/tabs; an index.mdx at its folder.
 * A folder in parentheses, such as (guide), groups pages without a part in the URL.
 */
function pathToUrl(file: string): string | undefined {
  const rel = relative(contentDir, file).split(sep).join('/');
  if (rel.startsWith('..') || !/\.mdx?$/.test(rel)) return undefined;
  const slugs = rel
    .replace(/\.mdx?$/, '')
    .split('/')
    .filter((part) => !/^\(.+\)$/.test(part));
  if (slugs[slugs.length - 1] === 'index') slugs.pop();
  return ['/docs', ...slugs].join('/');
}

/** Heading ids as Fumadocs makes them: github-slugger, or an explicit [#id] after the text. */
function headingIds(content: string): string[] {
  const slugger = new GithubSlugger();
  const ids: string[] = [];
  let fence = false;
  for (const line of content.split('\n')) {
    if (/^\s*(```|~~~)/.test(line)) fence = !fence;
    const heading = !fence && /^#{1,6}\s+(.+?)\s*$/.exec(line);
    if (!heading) continue;
    const explicit = /\[#([^\]]+)\]$/.exec(heading[1]);
    const text = heading[1].replace(/`/g, '').replace(/\[([^\]]*)\]\([^)]*\)/g, '$1');
    ids.push(explicit ? explicit[1] : slugger.slug(text));
  }
  return ids;
}

const files = walk(contentDir).map((path) => {
  const content = readFileSync(path, 'utf8');
  return { path, content, url: pathToUrl(path) };
});

const scanned: ScanResult = { urls: new Map(), fallbackUrls: [] };
for (const file of files) {
  if (!file.url) continue;
  scanned.urls.set(file.url, { hashes: headingIds(file.content) });
}
for (const url of ['/', '/llms.txt', '/llms-full.txt']) scanned.urls.set(url, {});
// Files in public/, such as the API documents in /openapi.
function walkPublic(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    return entry.isDirectory() ? walkPublic(path) : [path];
  });
}
for (const file of walkPublic('public')) {
  scanned.urls.set(`/${relative('public', file).split(sep).join('/')}`, {});
}
for (const file of files) {
  if (file.url) scanned.urls.set(`/md${file.url.slice('/docs'.length) || '/index'}.md`, {});
}

const results = await validateFiles(files, {
  scanned,
  pathToUrl,
  checkRelativePaths: 'as-url',
  markdown: { components: { Card: { attributes: ['href'] } } },
});

// The links in docs.config.ts, which the pages do not contain.
const configLinks = [
  ...config.nav.map((link) => link.url),
  ...config.actions.map((link) => link.url),
  ...config.features.flatMap((feature) => (feature.url ? [feature.url] : [])),
];
const configErrors = configLinks
  .filter((url) => url.startsWith('/'))
  .filter((url) => {
    const [path, hash] = url.replace(/\/(?=#|$)/, '').split('#');
    const meta = scanned.urls.get(path || '/');
    return !meta || (hash !== undefined && !meta.hashes?.includes(hash));
  });

printErrors(results);
for (const url of configErrors) console.error(`docs.config.ts: ${url}: not-found`);

const broken = results.reduce((count, result) => count + result.errors.length, 0) + configErrors.length;
if (broken > 0) {
  console.error(`${broken} broken link(s).`);
  process.exit(1);
}
console.log(`Checked ${files.length} pages: no broken links.`);

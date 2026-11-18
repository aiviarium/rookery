import { createMDX } from 'fumadocs-mdx/next';

const withMDX = createMDX();

// Set when the site is served from a folder, such as /docs-base on github.io.
const basePath = process.env.DOCS_BASE_PATH || undefined;

/** @type {import('next').NextConfig} */
const config = {
  // A static site: `npm run build` writes it to out/.
  output: 'export',
  trailingSlash: true,
  reactStrictMode: true,
  basePath,
  // A static host has no image server.
  images: { unoptimized: true },
  // Code that names files in public/ or fetches the search index adds the folder itself.
  env: { NEXT_PUBLIC_BASE_PATH: basePath ?? '' },
};

export default withMDX(config);

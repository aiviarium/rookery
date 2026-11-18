import { defineConfig, defineDocs } from 'fumadocs-mdx/config';
import lastModified from 'fumadocs-mdx/plugins/last-modified';
import { neutralDark, neutralLight } from './src/lib/shiki-themes';

const LAYOUT_ONLY = new Set(['Steps', 'Step', 'Tabs', 'Tab']);

export const docs = defineDocs({
  dir: 'content/docs',
  docs: {
    postprocess: {
      // Each page also as plain Markdown, for llms.txt and the .md files.
      includeProcessedMarkdown: {
        headingIds: false,
        // Layout components add nothing to the text; keep what is inside them.
        filterElement: (node) =>
          (node.type === 'mdxJsxFlowElement' || node.type === 'mdxJsxTextElement') &&
          node.name !== null &&
          LAYOUT_ONLY.has(node.name)
            ? 'children-only'
            : true,
      },
    },
  },
});

export default defineConfig({
  // "Last updated" on each page, from the git history.
  plugins: [lastModified()],
  mdxOptions: {
    rehypeCodeOptions: {
      themes: { light: neutralLight, dark: neutralDark },
      defaultColor: false,
    },
  },
});

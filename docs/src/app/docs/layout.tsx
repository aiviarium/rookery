import { DocsLayout } from 'fumadocs-ui/layouts/notebook';
import type { ReactNode } from 'react';
import { baseOptions } from '@/lib/layout.shared';
import { source } from '@/lib/source';

// The notebook layout: one header across the top, like the docs header of the
// identity, with the sidebar and the page below it.
export default function Layout({ children }: { children: ReactNode }) {
  const base = baseOptions();
  return (
    <DocsLayout {...base} nav={{ ...base.nav, mode: 'top' }} tree={source.getPageTree()}>
      {children}
    </DocsLayout>
  );
}

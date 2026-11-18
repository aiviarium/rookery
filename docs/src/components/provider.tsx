'use client';
import { RootProvider } from 'fumadocs-ui/provider/next';
import type { ReactNode } from 'react';

const basePath = process.env.NEXT_PUBLIC_BASE_PATH ?? '';

export function Provider({ children }: { children: ReactNode }) {
  return (
    <RootProvider
      // The index is a static file written by /api/search at build time.
      search={{ options: { type: 'static', api: `${basePath}/api/search` } }}
      theme={{ defaultTheme: 'system', enableSystem: true }}
    >
      {children}
    </RootProvider>
  );
}

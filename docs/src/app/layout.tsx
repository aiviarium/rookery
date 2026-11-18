import type { Metadata, Viewport } from 'next';
import localFont from 'next/font/local';
import type { CSSProperties, ReactNode } from 'react';
import { Provider } from '@/components/provider';
import { asset, config } from '@/lib/site';
import './global.css';

// Self-hosted variable fonts, licensed under the OFL (see the texts beside them).
const inter = localFont({
  src: '../fonts/inter.woff2',
  variable: '--font-inter',
  weight: '100 900',
  display: 'swap',
});
const mono = localFont({
  src: '../fonts/jetbrains-mono.woff2',
  variable: '--font-jetbrains-mono',
  weight: '100 800',
  display: 'swap',
});

export const metadata: Metadata = {
  title: { default: config.name, template: `%s · ${config.name}` },
  description: config.description,
  icons: {
    icon: [
      { url: asset(config.assets.favicon.dark), type: 'image/svg+xml', media: '(prefers-color-scheme: dark)' },
      { url: asset(config.assets.favicon.light), type: 'image/svg+xml', media: '(prefers-color-scheme: light)' },
    ],
  },
};

export const viewport: Viewport = {
  themeColor: [
    { media: '(prefers-color-scheme: dark)', color: '#0A0A0B' },
    { media: '(prefers-color-scheme: light)', color: '#FFFFFF' },
  ],
};

// The project colour from docs.config.ts, as two CSS variables. global.css picks one per theme.
const brand = {
  '--brand-on-dark': config.colour.dark,
  '--brand-on-light': config.colour.light,
} as CSSProperties;

export default function Layout({ children }: { children: ReactNode }) {
  return (
    <html lang="en" className={`${inter.variable} ${mono.variable}`} style={brand} suppressHydrationWarning>
      <body className="flex min-h-screen flex-col">
        <Provider>{children}</Provider>
      </body>
    </html>
  );
}

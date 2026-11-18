import type { BaseLayoutProps, LinkItemType } from 'fumadocs-ui/layouts/shared';
import { Badge } from '@/components/badge';
import { NavLockup } from '@/components/brand';
import { SearchButton, SearchField, ThemeToggle } from '@/components/header-controls';
import { config, isExternal } from '@/lib/site';

const navLinks: LinkItemType[] = config.nav.map((link) => ({
  text: link.text,
  url: link.url,
  external: link.external ?? isExternal(link.url),
  // Exact match: /docs is a prefix of every other docs link.
  active: 'url',
}));

/** The header shared by the landing page and the docs. */
export function baseOptions(): BaseLayoutProps {
  return {
    nav: {
      title: (
        <>
          <NavLockup />
          {config.chip ? (
            <Badge label={config.chip.label} value={config.chip.value} icon={false} className="nav-chip" />
          ) : null}
        </>
      ),
    },
    // Header controls in Nocturne's treatments, one control height.
    slots: {
      themeSwitch: ThemeToggle,
      searchTrigger: { sm: SearchButton, full: SearchField },
    },
    links: [
      ...navLinks,
      { type: 'main', text: 'GitHub ↗', url: config.repo, external: true },
    ],
  };
}

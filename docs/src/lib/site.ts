import config from '@config';

/** The folder the site is served from, such as /docs-base, or an empty string. */
export const basePath = process.env.NEXT_PUBLIC_BASE_PATH ?? '';

/** A file in public/, with the base path. Next.js adds it to links, but not to plain URLs. */
export const asset = (path: string) => `${basePath}${path}`;

export const isExternal = (url: string) => /^https?:\/\//.test(url);

/** "Edit on GitHub" for a page, from its path below content/docs. */
export const editUrl = (path: string) =>
  `${config.repo}/blob/main/${config.repoDocsPath}/${path}`;

export { config };

/** The owner and name of the repository, from its URL. */
export const repoPath = new URL(config.repo).pathname.replace(/^\/|\/$/g, '');

export interface EndpointBadge {
  alt: string;
  src: string;
  href: string;
}

/**
 * The badges of the project from the badge endpoint: the family badge with the
 * wordmark, then CI and the latest release when the config asks for them.
 */
export function endpointBadges(): EndpointBadge[] {
  const { host, ci, release } = config.badges;
  const base = `${host}/${config.slug}`;
  // The wordmark above already names the project, so the badges carry state only.
  const list: EndpointBadge[] = [];
  if (ci) {
    list.push({
      alt: 'CI',
      src: `${base}/github/actions/workflow/status/${repoPath}/${ci}?label=CI`,
      href: `${config.repo}/actions/workflows/${ci}`,
    });
  }
  if (release) {
    list.push({
      alt: 'Release',
      src: `${base}/github/v/release/${repoPath}`,
      href: `${config.repo}/releases`,
    });
  }
  return list;
}

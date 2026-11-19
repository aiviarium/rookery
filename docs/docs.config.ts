/**
 * Everything that differs between the documentation sites of the organisation.
 *
 * The values of Rookery. The rest of the site follows the docs base
 * (https://github.com/aiviarium/docs-base), which reads them from here.
 */

export interface NavLink {
  text: string;
  url: string;
  /** Opens outside the site. Links that start with http are external anyway. */
  external?: boolean;
}

export interface Feature {
  title: string;
  description: string;
  url?: string;
}

/** One image in a dark and a light version, as paths under public/. */
export interface Themed {
  dark: string;
  light: string;
}

export interface DocsConfig {
  /** The name in title case, as it appears in prose: "Hatchery". */
  name: string;
  /** The lowercase name used in URLs, packages and commands: "hatchery". */
  slug: string;
  /** One sentence, shown under the wordmark and in the page metadata. */
  description: string;
  /** The area after "Aiviarium /" in the small label above the wordmark. */
  area: string;
  /** The project colour. `light` is the darker tone that passes 4.5:1 on white. */
  colour: Themed;
  /** The repository, linked from the header and the "Edit on GitHub" links. */
  repo: string;
  /** The folder of the docs inside the repository, for "Edit on GitHub". */
  repoDocsPath: string;
  /**
   * The badge beside the name in the header, such as the version. Drawn like the
   * organisation's badges: the label on a dark field, the value on the project
   * colour. Leave it out to hide it.
   */
  chip?: { label: string; value: string };
  /**
   * Badges from the organisation's badge endpoint, shown on the landing page and
   * in the README.
   */
  badges: {
    /** The badge endpoint. */
    host: string;
    /** The file name of a GitHub Actions workflow, for a CI badge. */
    ci?: string;
    /** Show the latest GitHub release. */
    release?: boolean;
  };
  install: {
    label: string;
    command: string;
  };
  /** The two buttons under the description. The first one is outlined in the project colour. */
  actions: [NavLink, NavLink];
  assets: {
    /** The detailed icon, shown at 128 px on the landing page. */
    icon: Themed;
    /** The `-icon` file, drawn for 24 to 48 px, used in the header lockup. */
    navIcon: Themed;
    /** The `-16` file, drawn on the pixel grid for 16 px: the favicon. */
    favicon: Themed;
    /** The faceted wordmark, for word sizes of 24 px and above. */
    wordmark: Themed;
    /** The plain wordmark in `currentColor` (`-mono`), for the 18 px header lockup. */
    wordmarkSolid: string;
  };
  /** Links in the header, after the name. */
  nav: NavLink[];
  /** Cards on the landing page. Leave the list empty to hide them. */
  features: Feature[];
}

const config: DocsConfig = {
  name: 'Rookery',
  slug: 'rookery',
  description: 'Provisioning and management for development machines.',
  area: 'Development machines',
  colour: { dark: '#F2B33D', light: '#9A6200' },
  repo: 'https://github.com/aiviarium/rookery',
  repoDocsPath: 'docs/content/docs',
  badges: {
    host: 'https://aiviarium-badges.helight.workers.dev',
    ci: 'ci.yml',
    release: true,
  },
  install: {
    label: 'Install on Linux x86_64',
    command: 'curl -fsSL https://raw.githubusercontent.com/aiviarium/rookery/main/install.sh | sh',
  },
  actions: [
    { text: 'Get started', url: '/docs/start/quick-start' },
    { text: 'Read the docs', url: '/docs' },
  ],
  assets: {
    icon: { dark: '/brand/rookery.svg', light: '/brand/rookery-light.svg' },
    navIcon: { dark: '/brand/rookery-icon.svg', light: '/brand/rookery-icon-light.svg' },
    favicon: { dark: '/brand/rookery-16.svg', light: '/brand/rookery-16-light.svg' },
    wordmark: {
      dark: '/brand/rookery-wordmark.svg',
      light: '/brand/rookery-wordmark-light.svg',
    },
    wordmarkSolid: '/brand/rookery-wordmark-mono.svg',
  },
  nav: [
    { text: 'Guide', url: '/docs' },
    { text: 'API', url: '/docs/api' },
  ],
  features: [
    {
      title: 'A desktop you can watch',
      description:
        'XFCE on a persistent virtual display, in your browser through noVNC, ready for the tools you install.',
      url: '/docs/use/access',
    },
    {
      title: 'One command line for every host',
      description: 'Status, a shell, updates and reboots per host, from the same API a script uses.',
      url: '/docs/use/cli',
    },
    {
      title: 'Sign-ins and secrets',
      description: 'Sign the agents in from your own browser, and hand over tokens and registry logins.',
      url: '/docs/use/sign-ins',
    },
    {
      title: 'Plugins',
      description:
        'Coding agents, provisioners and other software as npm packages: Docker, Incus, Unity, backups, or your own.',
      url: '/docs/extend/plugins',
    },
    {
      title: 'APIs on both sides',
      description: 'rookeryd on each host and rookery serve on your machine, described as OpenAPI and AsyncAPI.',
      url: '/docs/extend/serve',
    },
    {
      title: 'One role for every install',
      description: 'The same Ansible role sets up a laptop, a server, a cloud machine or a container.',
      url: '/docs/start/concepts',
    },
  ],
};

export default config;

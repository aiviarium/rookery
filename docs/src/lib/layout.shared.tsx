import type { BaseLayoutProps } from 'fumadocs-ui/layouts/shared';

export const github = 'https://github.com/aiviarium/rookery';

/** Files in public/, with the folder the site is served from (DOCS_BASE_PATH). */
export const asset = (path: string) => `${process.env.NEXT_PUBLIC_BASE_PATH ?? ''}${path}`;

export function baseOptions(): BaseLayoutProps {
  return {
    nav: {
      title: (
        <>
          <img src={asset('/logo.svg')} alt="" width={27} height={27} />
          <img src={asset('/wordmark.svg')} alt="Rookery" width={72} height={19} />
        </>
      ),
    },
    githubUrl: github,
    // Dark only.
    themeSwitch: { enabled: false },
  };
}

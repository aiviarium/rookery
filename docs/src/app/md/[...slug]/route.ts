import { notFound } from 'next/navigation';
import { markdownSlug, pageMarkdown } from '@/lib/llms';
import { source } from '@/lib/source';

export const dynamic = 'force-static';
export const dynamicParams = false;

// Each page as a Markdown file: /md/components/tabs.md.
export async function GET(_: Request, { params }: RouteContext<'/md/[...slug]'>) {
  const wanted = (await params).slug.join('/');
  const page = source.getPages().find((candidate) => markdownSlug(candidate).join('/') === wanted);
  if (!page) notFound();
  return new Response(await pageMarkdown(page), {
    headers: { 'content-type': 'text/markdown; charset=utf-8' },
  });
}

export function generateStaticParams() {
  return source.getPages().map((page) => ({ slug: markdownSlug(page) }));
}

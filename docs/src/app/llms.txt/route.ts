import { llmsIndex } from '@/lib/llms';

export const dynamic = 'force-static';

// The index of the docs for language models: every page with a link to its Markdown.
export async function GET() {
  return new Response(await llmsIndex(), {
    headers: { 'content-type': 'text/markdown; charset=utf-8' },
  });
}

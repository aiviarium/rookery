import { createFromSource } from 'fumadocs-core/search/server';
import { source } from '@/lib/source';

export const dynamic = 'force-static';

// The search index, written at build time. The search dialog downloads it and
// searches in the browser with Orama, so the site needs no server.
export const { staticGET: GET } = createFromSource(source, { language: 'english' });

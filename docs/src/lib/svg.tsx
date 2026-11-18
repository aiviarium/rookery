import { readFileSync } from 'node:fs';
import { join } from 'node:path';

interface Svg {
  markup: string;
  /** Width over height, from the viewBox. */
  ratio: number;
}

const cache = new Map<string, Svg>();

/**
 * Reads an SVG from public/ at build time, so it can be inlined. Inlining lets a
 * `currentColor` wordmark take the project colour from CSS, and keeps the
 * wordmark sharp without an extra request.
 */
export function readSvg(path: string): Svg {
  let svg = cache.get(path);
  if (!svg) {
    const text = readFileSync(join(process.cwd(), 'public', path), 'utf8');
    const box = /viewBox="([\d.\s-]+)"/.exec(text)?.[1].trim().split(/\s+/).map(Number);
    const ratio = box ? box[2] / box[3] : 1;
    // Drop the size so CSS decides it, and the XML prolog if there is one.
    const markup = text
      .replace(/<\?xml[^>]*>\s*/, '')
      .replace(/<svg([^>]*?)\s(width|height)="[^"]*"/g, '<svg$1');
    svg = { markup, ratio };
    cache.set(path, svg);
  }
  return svg;
}

interface InlineSvgProps {
  path: string;
  className?: string;
  /** The height in em. The width follows from the viewBox. */
  height?: number;
  label?: string;
}

/** An SVG from public/ placed inline, sized in em so it scales with the font size. */
export function InlineSvg({ path, className, height = 1, label }: InlineSvgProps) {
  const { markup, ratio } = readSvg(path);
  return (
    <span
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      className={`inline-svg ${className ?? ''}`}
      style={{ width: `${ratio * height}em`, height: `${height}em` }}
      dangerouslySetInnerHTML={{ __html: markup }}
    />
  );
}

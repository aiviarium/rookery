/* eslint-disable @next/next/no-img-element -- a static export has no image server; these are SVG files. */
import type { Themed } from '@config';
import { asset, config } from '@/lib/site';
import { InlineSvg, readSvg } from '@/lib/svg';

interface ThemedImgProps {
  src: Themed;
  /** Empty for decoration. */
  alt: string;
  width: number | string;
  height: number | string;
  className?: string;
}

/** An image with a dark and a light file. CSS shows the one for the current theme. */
export function ThemedImg({ src, alt, width, height, className = '' }: ThemedImgProps) {
  const style = { width, height };
  return (
    <>
      <img src={asset(src.dark)} alt={alt} style={style} className={`only-dark ${className}`} />
      <img src={asset(src.light)} alt={alt} style={style} className={`only-light ${className}`} />
    </>
  );
}

/**
 * The header lockup at an 18 px word size: the `-icon` file in a 1.5 em box, a
 * 0.25 em gap and the plain wordmark in the project colour. Below 24 px the
 * identity uses the plain outlines, not the faceted ones.
 */
export function NavLockup() {
  return (
    <span className="lockup" aria-label={config.name} role="img">
      <ThemedImg src={config.assets.navIcon} alt="" width="1.5em" height="1.5em" className="lockup-icon" />
      <InlineSvg path={config.assets.wordmarkSolid} height={1.05} className="lockup-word" />
    </span>
  );
}

/** The faceted wordmark, for word sizes of 24 px and above. Sized by the font size of its parent. */
export function Wordmark() {
  const ratio = readSvg(config.assets.wordmark.dark).ratio;
  return (
    <ThemedImg
      src={config.assets.wordmark}
      alt={config.name}
      width={`${(ratio * 1.05).toFixed(3)}em`}
      height="1.05em"
      className="wordmark"
    />
  );
}

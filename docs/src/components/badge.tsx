/* eslint-disable @next/next/no-img-element -- a static export has no image server; this is an SVG file. */
import type { ReactNode } from 'react';
import { asset, config } from '@/lib/site';

interface BadgeProps {
  /** The text on the dark field. */
  label: ReactNode;
  /** The text on the project colour. Leave it out for a label alone. */
  value?: ReactNode;
  /** Show the project icon at the left. */
  icon?: boolean;
  className?: string;
}

/**
 * A badge in the style of the organisation's badge endpoint: a dark field with a
 * 1 px outline and radius 5, the icon at the left, the label in Inter 600 and
 * the value in Inter 650 on the project colour. Badges are dark in both themes,
 * like the badge images. This is the one place the project colour fills an area.
 */
export function Badge({ label, value, icon = true, className = '' }: BadgeProps) {
  return (
    <span className={`badge ${className}`}>
      <span className="badge-label">
        {icon && <img src={asset(config.assets.navIcon.dark)} alt="" className="badge-icon" />}
        {label}
      </span>
      {value !== undefined && <span className="badge-value">{value}</span>}
    </span>
  );
}

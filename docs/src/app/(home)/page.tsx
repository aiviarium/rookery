import Link from 'next/link';
import { Fragment } from 'react';
import { ThemedImg, Wordmark } from '@/components/brand';
import { CopyButton } from '@/components/copy-button';
import { config, endpointBadges, isExternal } from '@/lib/site';

function Action({ text, url, primary }: { text: string; url: string; primary?: boolean }) {
  const external = isExternal(url);
  // Nocturne's large buttons: the one primary action filled with the text colour, the other a neutral fill.
  const className = primary ? 'nc-btn nc-btn--lg nc-btn--primary' : 'nc-btn nc-btn--lg';
  return external ? (
    <a href={url} className={className} rel="noreferrer noopener" target="_blank">
      {text} ↗
    </a>
  ) : (
    <Link href={url} className={className}>
      {text}
    </Link>
  );
}

export default function HomePage() {
  const [primary, secondary] = config.actions;
  return (
    <div className="landing">
      <section className="hero">
        <div className="label">
          Aiviarium{'  '}/{'  '}
          {config.area}
        </div>
        <div className="hero-row">
          <div>
            <h1 className="hero-wordmark">
              <Wordmark />
            </h1>
            <p className="hero-description">{config.description}</p>
            <div className="hero-badges">
              {endpointBadges().map((badge) => (
                <a key={badge.src} href={badge.href} rel="noreferrer noopener" target="_blank">
                  {/* eslint-disable-next-line @next/next/no-img-element -- drawn by the badge endpoint */}
                  <img src={badge.src} alt={badge.alt} height={22} />
                </a>
              ))}
            </div>
          </div>
          <ThemedImg src={config.assets.icon} alt="" width={128} height={128} className="hero-icon" />
        </div>
        <div className="install">
          <small>{config.install.label}</small>
          <div className="install-line">
            <code>
              <span className="prompt" aria-hidden>
                ${' '}
              </span>
              {/* Words stay whole, so a line breaks only at a space, not after a hyphen. */}
              {config.install.command.split(' ').map((word, i) => (
                <Fragment key={i}>
                  {i > 0 ? ' ' : null}
                  <span className="install-word">{word}</span>
                </Fragment>
              ))}
            </code>
            <CopyButton text={config.install.command} />
          </div>
        </div>
        <div className="actions">
          <Action {...primary} primary />
          <Action {...secondary} />
        </div>
        {config.features.length > 0 && (
          <div className="cards">
            {config.features.map((feature) => {
              const body = (
                <>
                  <h2>{feature.title}</h2>
                  <p>{feature.description}</p>
                </>
              );
              return feature.url ? (
                <Link key={feature.title} href={feature.url} className="card card-link">
                  {body}
                </Link>
              ) : (
                <div key={feature.title} className="card">
                  {body}
                </div>
              );
            })}
          </div>
        )}
      </section>
    </div>
  );
}

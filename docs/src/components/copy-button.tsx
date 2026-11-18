'use client';
import { Check, Copy } from 'lucide-react';
import { useState } from 'react';

/** Copies a command to the clipboard and confirms for two seconds. */
export function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);
  return (
    <button
      type="button"
      className="nc-btn nc-btn--icon"
      aria-label={copied ? 'Copied' : 'Copy command'}
      title={copied ? 'Copied' : 'Copy'}
      onClick={() => {
        void navigator.clipboard.writeText(text).then(() => {
          setCopied(true);
          setTimeout(() => setCopied(false), 2000);
        });
      }}
    >
      {copied ? <Check aria-hidden /> : <Copy aria-hidden />}
    </button>
  );
}

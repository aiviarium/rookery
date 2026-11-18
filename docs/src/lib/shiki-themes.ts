// Code colours for the neutral surfaces of the identity. The background is left
// transparent so code sits on the page colour. The hues avoid orange, copper and
// teal, and stay apart from any one project colour. Every token passes 4.5:1 on
// the page and raised colours of its theme.
import type { ThemeRegistrationRaw } from 'shiki';

interface Palette {
  plain: string;
  comment: string;
  keyword: string;
  string: string;
  fn: string;
  type: string;
  constant: string;
  punct: string;
  tag: string;
  attr: string;
}

function theme(name: string, type: 'dark' | 'light', c: Palette): ThemeRegistrationRaw {
  return {
    name,
    type,
    colors: { 'editor.foreground': c.plain, 'editor.background': '#00000000' },
    settings: [
      { settings: { foreground: c.plain } },
      {
        scope: ['comment', 'punctuation.definition.comment', 'string.comment'],
        settings: { foreground: c.comment, fontStyle: 'italic' },
      },
      {
        scope: [
          'keyword',
          'storage',
          'storage.type',
          'storage.modifier',
          'keyword.operator.new',
          'keyword.control',
          'variable.language',
        ],
        settings: { foreground: c.keyword },
      },
      {
        scope: ['string', 'string.template', 'markup.inline.raw', 'string.regexp'],
        settings: { foreground: c.string },
      },
      {
        scope: [
          'entity.name.function',
          'support.function',
          'meta.function-call',
          'variable.function',
          'entity.name.command',
        ],
        settings: { foreground: c.fn },
      },
      {
        scope: [
          'entity.name.type',
          'support.type',
          'entity.name.class',
          'entity.other.inherited-class',
          'support.class',
          'entity.name.namespace',
        ],
        settings: { foreground: c.type },
      },
      {
        scope: [
          'constant',
          'constant.numeric',
          'constant.language',
          'support.constant',
          'variable.other.constant',
        ],
        settings: { foreground: c.constant },
      },
      {
        scope: ['punctuation', 'meta.brace', 'keyword.operator'],
        settings: { foreground: c.punct },
      },
      { scope: ['entity.name.tag', 'support.type.property-name'], settings: { foreground: c.tag } },
      { scope: ['entity.other.attribute-name'], settings: { foreground: c.attr } },
      { scope: ['markup.heading'], settings: { foreground: c.keyword, fontStyle: 'bold' } },
    ],
  };
}

export const neutralDark = theme('aiviarium-neutral-dark', 'dark', {
  plain: '#EDEDEF',
  comment: '#8B8B96',
  keyword: '#C4B5FD',
  string: '#A5D6FF',
  fn: '#F5F5F7',
  type: '#F0ABFC',
  constant: '#FDA4AF',
  punct: '#A1A1AA',
  tag: '#C4B5FD',
  attr: '#A5D6FF',
});

export const neutralLight = theme('aiviarium-neutral-light', 'light', {
  plain: '#111114',
  comment: '#62626C',
  keyword: '#5B3FC4',
  string: '#0B5CAD',
  fn: '#111114',
  type: '#8A2A9E',
  constant: '#B4234A',
  punct: '#4F4F58',
  tag: '#5B3FC4',
  attr: '#0B5CAD',
});

// Markdown for agent and user text: `marked` (GFM) then DOMPurify with a strict allow-list.
//
// - NUL characters are removed before anything else runs (the prototype's slot-marker injection).
// - Raw HTML in the source is shown as text, never parsed.
// - Images never load: they become links to their source.
// - Links keep only http(s) and mailto URLs and open in a new tab with `rel="noopener noreferrer"`.
import DOMPurify from 'dompurify';
import { Marked, type Tokens } from 'marked';

const ALLOWED_TAGS = [
  'p', 'br', 'hr', 'h1', 'h2', 'h3', 'h4', 'h5', 'h6', 'blockquote', 'pre', 'code', 'em', 'strong', 'del',
  'a', 'ul', 'ol', 'li', 'table', 'thead', 'tbody', 'tr', 'th', 'td',
];
const ALLOWED_ATTR = ['href', 'title', 'align', 'start'];
const SAFE_URL = /^(?:https?:|mailto:)/i;

const escapeHtml = (s: string): string =>
  s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;').replace(/"/g, '&quot;').replace(/'/g, '&#39;');

const marked = new Marked({
  gfm: true,
  breaks: false,
  async: false,
  renderer: {
    html: ({ text }: Tokens.HTML | Tokens.Tag) => escapeHtml(text),
    image: ({ href, text }: Tokens.Image) => `<a href="${escapeHtml(href)}">${escapeHtml(text || href)}</a>`,
  },
});

let purify: ReturnType<typeof DOMPurify> | null = null;

/** The sanitizer, created on first use (it needs a DOM). */
function sanitizer(): ReturnType<typeof DOMPurify> {
  if (purify) return purify;
  purify = DOMPurify(window);
  purify.addHook('afterSanitizeAttributes', (node) => {
    if (node.tagName !== 'A') return;
    const href = node.getAttribute('href');
    if (href !== null && !SAFE_URL.test(href.trim())) node.removeAttribute('href');
    if (node.hasAttribute('href')) {
      node.setAttribute('target', '_blank');
      node.setAttribute('rel', 'noopener noreferrer');
    }
  });
  return purify;
}

const CONFIG = { ALLOWED_TAGS, ALLOWED_ATTR, ALLOWED_URI_REGEXP: SAFE_URL, ALLOW_DATA_ATTR: false };

const toHtml = (source: string): string => marked.parse(source.replace(/\0/g, '')) as string;

/** Renders Markdown to sanitized HTML. */
export function renderMarkdown(source: string): string {
  return sanitizer().sanitize(toHtml(source), CONFIG);
}

/** Renders Markdown to a sanitized fragment, for patching into the DOM (see `morph`). */
export function markdownFragment(source: string): DocumentFragment {
  return sanitizer().sanitize(toHtml(source), { ...CONFIG, RETURN_DOM_FRAGMENT: true });
}

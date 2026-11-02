import { describe, expect, it } from 'vitest';
import { markdownFragment, renderMarkdown } from './markdown';
import { morph } from './morph';

/** Parses rendered HTML into a detached container. */
function dom(html: string): HTMLElement {
  const el = document.createElement('div');
  el.append(new DOMParser().parseFromString(html, 'text/html').body.cloneNode(true));
  return el;
}

/** Everything that would run script, load a resource or break out of an attribute. */
function assertInert(html: string): void {
  const el = dom(html);
  for (const node of el.querySelectorAll('*')) {
    expect(['script', 'iframe', 'object', 'embed', 'img', 'svg', 'math', 'style', 'link', 'meta', 'form', 'input', 'video', 'audio', 'base']).not.toContain(node.tagName.toLowerCase());
    for (const attr of Array.from(node.attributes)) {
      expect(attr.name).not.toMatch(/^on/i);
      expect(['href', 'title', 'align', 'start', 'target', 'rel']).toContain(attr.name);
      if (attr.name === 'href') expect(attr.value).toMatch(/^(https?:|mailto:)/i);
    }
    if (node.tagName === 'A' && node.hasAttribute('href')) {
      expect(node.getAttribute('rel')).toBe('noopener noreferrer');
      expect(node.getAttribute('target')).toBe('_blank');
    }
  }
  expect(html).not.toContain('\0');
}

// An XSS corpus: raw HTML, dangerous URL schemes, attribute breakouts, entity tricks, and the
// prototype's NUL slot-marker injection.
const CORPUS = [
  '<script>alert(1)</script>',
  '<img src=x onerror=alert(1)>',
  '<svg onload=alert(1)>',
  '<iframe src="https://example.com"></iframe>',
  '<a href="javascript:alert(1)">x</a>',
  '[click](javascript:alert(1))',
  '[click](JaVaScRiPt:alert(1))',
  '[click](  javascript:alert(1))',
  '[click](java\tscript:alert(1))',
  '[click](&#106;avascript:alert(1))',
  '[click](data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==)',
  '[click](vbscript:msgbox(1))',
  '[x](https://example.com "a\\" onmouseover=\\"alert(1)")',
  '[x](https://example.com/"onmouseover="alert(1))',
  '![x](https://example.com/a.png)',
  '![x" onerror="alert(1)](https://example.com/a.png)',
  '<https://example.com/onmouseover=alert(1)//>',
  '[a](https://x/\u00001\u0000) <https://y/onmouseover=alert(1)//>',
  'a\u0000b<img src=x onerror=alert(1)>\u0000',
  '<details open ontoggle=alert(1)>',
  '<style>body{display:none}</style>',
  '<form action="https://example.com"><input name=x></form>',
  '`<script>alert(1)</script>`',
  '```html\n<script>alert(1)</script>\n```',
  '<a href="https://example.com" style="position:fixed;top:0">overlay</a>',
  '[x](mailto:alice@example.com)',
  '<base href="https://example.com/">',
  '<meta http-equiv="refresh" content="0;url=https://example.com">',
  '<math><mi xlink:href="javascript:alert(1)">x</mi></math>',
  '[x]: javascript:alert(1)\n\n[ref][x]',
];

describe('renderMarkdown', () => {
  it.each(CORPUS)('keeps %j inert', (source) => {
    assertInert(renderMarkdown(source));
  });

  it('renders ordinary Markdown', () => {
    const el = dom(renderMarkdown('# Title\n\nSome **bold**, `code` and a [link](https://example.com).\n\n- one\n- two\n\n```\nfn main() {}\n```'));
    expect(el.querySelector('h1')?.textContent).toBe('Title');
    expect(el.querySelector('strong')?.textContent).toBe('bold');
    expect(el.querySelectorAll('li')).toHaveLength(2);
    expect(el.querySelector('pre code')?.textContent).toBe('fn main() {}\n');
    const a = el.querySelector('a')!;
    expect(a.getAttribute('href')).toBe('https://example.com');
    expect(a.getAttribute('rel')).toBe('noopener noreferrer');
  });

  it('shows raw HTML as text', () => {
    expect(dom(renderMarkdown('a <b>bold</b> c')).textContent).toContain('<b>bold</b>');
  });

  it('turns images into links (no remote loads)', () => {
    const el = dom(renderMarkdown('![diagram](https://example.com/d.png)'));
    expect(el.querySelector('img')).toBeNull();
    expect(el.querySelector('a')?.getAttribute('href')).toBe('https://example.com/d.png');
    expect(el.textContent).toContain('diagram');
  });

  it('strips NUL before parsing', () => {
    expect(renderMarkdown('a\u0000b')).toContain('ab');
  });

  it('drops unsafe hrefs but keeps the text', () => {
    const a = dom(renderMarkdown('[run](javascript:alert(1))')).querySelector('a');
    expect(a?.hasAttribute('href') ?? false).toBe(false);
    expect(dom(renderMarkdown('[run](javascript:alert(1))')).textContent).toContain('run');
  });
});

describe('morph', () => {
  it('keeps unchanged nodes while text streams', () => {
    const el = document.createElement('div');
    morph(el, markdownFragment('First paragraph.\n\nSecond'));
    const [p1, p2] = Array.from(el.children);
    const text = p2.firstChild;
    morph(el, markdownFragment('First paragraph.\n\nSecond, longer'));
    expect(el.children[0]).toBe(p1);
    expect(el.children[1]).toBe(p2);
    expect(p2.firstChild).toBe(text);
    expect(p2.textContent).toBe('Second, longer');
    morph(el, markdownFragment('First paragraph.\n\nSecond, longer\n\n- item'));
    expect(el.children[0]).toBe(p1);
    expect(el.children[2].tagName).toBe('UL');
    morph(el, markdownFragment('Only'));
    expect(el.children).toHaveLength(1);
    expect(el.textContent?.trim()).toBe('Only');
  });
});

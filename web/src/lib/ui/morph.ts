// Patches a rendered (already sanitized) fragment into an element in place: nodes whose tag (or
// text) is unchanged are kept, so a streamed message only touches its last text node and
// selection, hover and focus inside the earlier parts survive.

/** Makes `target`'s children match `next` with as few DOM changes as possible. */
export function morph(target: Element, next: DocumentFragment): void {
  patchChildren(target, next);
}

function patchChildren(parent: Node, next: Node): void {
  const have = Array.from(parent.childNodes);
  const want = Array.from(next.childNodes);
  want.forEach((node, i) => {
    const cur = have[i];
    if (!cur) parent.appendChild(node);
    else if (sameKind(cur, node)) patchNode(cur, node);
    else parent.replaceChild(node, cur);
  });
  for (const extra of have.slice(want.length)) parent.removeChild(extra);
}

const sameKind = (a: Node, b: Node): boolean =>
  a.nodeType === b.nodeType && (a.nodeType !== Node.ELEMENT_NODE || (a as Element).tagName === (b as Element).tagName);

function patchNode(cur: Node, next: Node): void {
  if (cur.nodeType !== Node.ELEMENT_NODE) {
    if (cur.nodeValue !== next.nodeValue) cur.nodeValue = next.nodeValue;
    return;
  }
  const el = cur as Element;
  const want = next as Element;
  for (const { name } of Array.from(el.attributes)) if (!want.hasAttribute(name)) el.removeAttribute(name);
  for (const { name, value } of Array.from(want.attributes)) if (el.getAttribute(name) !== value) el.setAttribute(name, value);
  patchChildren(el, want);
}

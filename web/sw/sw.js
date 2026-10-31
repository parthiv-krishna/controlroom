/* controlroom service worker, emitted at <root>/sw.js by the build (vite.config.ts).
 * Caches the app shell only, so the app opens without a network and shows "can't reach server".
 * Never caches api/ or auth/ (no session data). Push → notification; click → the session deep link. */
const PRECACHE = self.__PRECACHE__;
const CACHE = 'controlroom-' + self.__VERSION__;
// The scope always ends with a slash; every shell file lives below it.
const scope = self.registration.scope;
const isLive = (url) => url.startsWith(scope + 'api/') || url.startsWith(scope + 'auth/');
const inScope = (path) => scope + path.replace(/^\.\//, '');

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      .then((cache) => cache.addAll(PRECACHE.map(inScope)))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k.startsWith('controlroom-') && k !== CACHE).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener('fetch', (event) => {
  const request = event.request;
  if (request.method !== 'GET') return;
  if (!request.url.startsWith(scope) || isLive(request.url)) return;
  if (request.mode === 'navigate') {
    // Network first so a new deploy is picked up; the cached shell when offline.
    event.respondWith(fetch(request).catch(() => caches.match(scope)));
    return;
  }
  event.respondWith(caches.match(request).then((hit) => hit || fetch(request)));
});

// Payload: { title?, body?, machine_id, session_id, name?, machine?, state? }
self.addEventListener('push', (event) => {
  let data = {};
  try {
    data = event.data ? event.data.json() : {};
  } catch {
    data = { body: event.data && event.data.text() };
  }
  const title = data.title || data.name || 'controlroom';
  event.waitUntil(
    self.registration.showNotification(title, {
      body: data.body || [data.machine, data.state].filter(Boolean).join(' · '),
      tag: data.machine_id && data.session_id ? `${data.machine_id}/${data.session_id}` : undefined,
      data,
      icon: inScope('icon.svg'),
    }),
  );
});

/** The SPA's deep link; its query lets the page pre-render before the socket opens. */
function deepLink(data) {
  if (!data.machine_id || !data.session_id) return scope;
  const query = new URLSearchParams();
  if (data.name) query.set('n', data.name);
  if (data.machine) query.set('m', data.machine);
  if (data.state) query.set('st', data.state);
  const qs = query.toString();
  return `${scope}#/s/${encodeURIComponent(data.machine_id)}/${encodeURIComponent(data.session_id)}${qs ? '?' + qs : ''}`;
}

self.addEventListener('notificationclick', (event) => {
  event.notification.close();
  const url = deepLink(event.notification.data || {});
  event.waitUntil(
    self.clients.matchAll({ type: 'window', includeUncontrolled: true }).then((windows) => {
      const open = windows.find((w) => w.url.startsWith(scope));
      if (open) return open.navigate(url).then((w) => (w || open).focus()).catch(() => open.focus());
      return self.clients.openWindow(url);
    }),
  );
});

// The installable app: the manifest is linked from index.html; the service worker (sw/sw.js,
// emitted at <root>/sw.js by the build) caches the app shell only.
export async function registerServiceWorker(base: string): Promise<void> {
  if (!('serviceWorker' in navigator)) return;
  try {
    await navigator.serviceWorker.register(`${base}sw.js`, { scope: new URL(base).pathname });
  } catch {
    // no worker: the app still works online
  }
}

/** Subscribes this browser to Web Push with the server's VAPID key. */
export async function pushSubscription(vapidPublicKey: string): Promise<PushSubscriptionJSON> {
  const reg = await navigator.serviceWorker?.getRegistration();
  if (!reg) throw new Error('The app is not installed as a service worker here');
  const sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: base64url(vapidPublicKey) });
  return sub.toJSON();
}

function base64url(s: string): Uint8Array<ArrayBuffer> {
  const bin = atob(s.replace(/-/g, '+').replace(/_/g, '/') + '==='.slice((s.length + 3) % 4));
  const out = new Uint8Array(new ArrayBuffer(bin.length));
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

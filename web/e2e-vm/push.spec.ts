import { expect, test } from '@playwright/test';
import { login } from './helpers';

// Web Push on a device. Headless Chromium has no push service offline and denies notifications,
// so both are faked: notifications are granted, and `subscribe` returns a subscription (with a
// real P-256 key) at the VM's mock push service, push.example.com. The device then goes away;
// the next approval (approvals.spec.ts) is pushed to it, and the test script checks that the
// server reached the mock push service.
test('push is enabled on a device', async ({ browser }) => {
  const context = await browser.newContext();
  await context.addInitScript(fakePushService);
  const page = await context.newPage();
  await login(page, 'iPhone');
  await page.evaluate(async () => {
    await navigator.serviceWorker.ready;
  });

  await page.getByRole('button', { name: 'Settings' }).click();
  const settings = page.getByRole('dialog', { name: 'Settings' });
  await settings.getByRole('tab', { name: 'Notifications' }).click();
  await settings.getByRole('button', { name: 'Enable', exact: true }).click();
  await expect(settings).toContainText('Push is on for iPhone');
  await expect(settings.getByRole('button', { name: 'Turn off' })).toBeVisible();
  await context.close();
});

/** Runs in the page: notifications are allowed, and `PushManager.subscribe` answers with a
 * subscription at push.example.com. */
function fakePushService(): void {
  Object.defineProperty(Notification, 'permission', { get: () => 'granted' });
  Notification.requestPermission = async () => 'granted';
  const base64url = (bytes: Uint8Array) =>
    btoa(String.fromCharCode(...bytes))
      .replace(/\+/g, '-')
      .replace(/\//g, '_')
      .replace(/=+$/, '');
  PushManager.prototype.subscribe = async function () {
    const keys = await crypto.subtle.generateKey({ name: 'ECDH', namedCurve: 'P-256' }, true, ['deriveBits']);
    const p256dh = new Uint8Array(await crypto.subtle.exportKey('raw', keys.publicKey));
    const json = {
      endpoint: 'https://push.example.com/push/iphone',
      expirationTime: null,
      keys: { p256dh: base64url(p256dh), auth: base64url(crypto.getRandomValues(new Uint8Array(16))) },
    };
    return { ...json, toJSON: () => json } as unknown as PushSubscription;
  };
}

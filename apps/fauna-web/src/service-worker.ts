/// <reference lib="webworker" />
declare const self: ServiceWorkerGlobalScope;

self.addEventListener('push', (event: PushEvent) => {
  const data = event.data?.json() ?? { title: 'Fauna', body: 'New notification' };
  event.waitUntil(
    self.registration.showNotification(data.title, {
      body: data.body,
      icon: '/favicon.png',
      badge: '/favicon.png',
      tag: data.tag ?? 'fauna-default',
    })
  );
});

self.addEventListener('notificationclick', (event: NotificationEvent) => {
  event.notification.close();
  event.waitUntil(
    self.clients.matchAll({ type: 'window', includeUncontrolled: true }).then((clients) => {
      if (clients.length > 0) {
        return clients[0].focus();
      }
      return self.clients.openWindow('/app');
    })
  );
});

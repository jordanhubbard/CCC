self.addEventListener('install', (event) => {
  event.waitUntil(caches.open('clawchat-v1').then(cache => cache.add('/')));
});
self.addEventListener('fetch', (event) => {
  event.respondWith(
    caches.match(event.request).then(resp => resp || fetch(event.request))
  );
});

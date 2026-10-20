// Direct-origin offline shell. Authentication URLs and all RPC/WS routes stay network-only.
const CACHE='krill-direct-shell-v2';
const SHELL=['/','/app.css','/app.js','/i18n.js','/crypto.js','/client.js','/terminal.js','/qr.js','/screens/home.js','/screens/workspaces.js','/screens/chat.js','/screens/session.js','/screens/details.js','/screens/settings.js','/manifest.json','/icon.svg'];
self.addEventListener('install',event=>event.waitUntil(caches.open(CACHE).then(cache=>cache.addAll(SHELL)).then(()=>self.skipWaiting())));
self.addEventListener('activate',event=>event.waitUntil(Promise.all([self.clients.claim(),caches.keys().then(keys=>Promise.all(keys.filter(key=>key.startsWith('krill-direct-shell-')&&key!==CACHE).map(key=>caches.delete(key))))])));
self.addEventListener('fetch',event=>{
 const url=new URL(event.request.url);
 if(event.request.method!=='GET'||url.origin!==location.origin)return;
 if(url.searchParams.has('auth')||['pair','key','credential','token'].some(key=>url.searchParams.has(key))){event.respondWith(fetch(event.request,{cache:'no-store'}).catch(async error=>{if(url.pathname==='/'){const shell=await (await caches.open(CACHE)).match(url.origin+'/');if(shell)return shell;}throw error;}));return;}
 if(!SHELL.includes(url.pathname))return;
 event.respondWith((async()=>{const cache=await caches.open(CACHE),key=url.origin+url.pathname;try{const response=await fetch(event.request,{cache:'no-store'});if(response.ok&&!response.redirected)await cache.put(key,response.clone());return response;}catch(error){const hit=await cache.match(key);if(hit)return hit;throw error;}})());
});

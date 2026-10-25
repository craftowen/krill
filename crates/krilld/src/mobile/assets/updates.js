// Same-tab, host-compatible PWA updates. Never persist or replay commands or credentials.
(()=>{
 // Reuse this static asset as a hash worker; HTTP deployments need no SubtleCrypto.
 if(typeof document==='undefined'&&typeof importScripts==='function'){importScripts('crypto.js');globalThis.onmessage=event=>{try{const bytes=new Uint8Array(event.data);if(bytes.length>8*1024*1024+16384)throw Error('Update too large');postMessage({hash:MkCrypto.hex(MkCrypto.sha256(bytes)).slice(0,16)});}catch{postMessage({error:true});}};return;}
 const M=globalThis.KrillMobile||={},C=MkCrypto,KEY='krill-mobile-update-v1',CACHE='krill-mobile-v1';
 const validHash=h=>typeof h==='string'&&/^[0-9a-f]{16}$/.test(h);
 const manifest=html=>{
  const match=/<meta name="krill-update" content="1 ([a-z0-9_./ -]+)">/.exec(html);
  if(!match)throw Error('Unsupported update handoff');
  const files=match[1].split(' ');
  if(files.length>64||new Set(files).size!==files.length||!files.includes('index.html')||!files.includes('updates.js')||files.some(f=>!f||f.includes('..')||! /^(?:[a-z0-9_-]+\/)*[a-z0-9_.-]+$/.test(f)))throw Error('Invalid update inventory');
  return files.sort();
 };
 async function digest(bytes,signal){
  if(signal?.aborted)throw Error('Update cancelled');
  if(globalThis.crypto?.subtle){const result=await crypto.subtle.digest('SHA-256',bytes);return C.hex(new Uint8Array(result)).slice(0,16);}
  if(globalThis.Worker)return new Promise((resolve,reject)=>{let worker;const stop=()=>{clearTimeout(timer);signal?.removeEventListener('abort',abort);worker?.terminate();},abort=()=>{stop();reject(Error('Update cancelled'));},timer=setTimeout(abort,15000);try{worker=new Worker('updates.js');worker.onmessage=event=>{stop();if(event.data?.hash)resolve(event.data.hash);else reject(Error('Update hash failed'));};worker.onerror=()=>{stop();reject(Error('Update hash failed'));};signal?.addEventListener('abort',abort,{once:true});worker.postMessage(bytes.buffer,[bytes.buffer]);}catch(error){stop();reject(error);}});
  // Older browsers still verify the bounded bundle rather than accepting unchecked assets.
  return C.hex(C.sha256(bytes)).slice(0,16);
 }
 async function prepare(hash,signal){
  if(!validHash(hash)||!globalThis.caches)throw Error('Update cache unavailable');
  const base=`/m/${hash}/`,controller=new AbortController(),abort=()=>controller.abort();signal?.addEventListener('abort',abort,{once:true});if(signal?.aborted)abort();let total=0;
  const get=async path=>{const response=await fetch(path,{cache:'no-store',redirect:'error',signal:controller.signal});if(!response.ok||response.redirected)throw Error('Incomplete update');const advertised=Number(response.headers?.get('content-length'));if(advertised>2*1024*1024)throw Error('Update asset too large');
   const parts=[];let size=0;const add=bytes=>{size+=bytes.length;total+=bytes.length;if(size>2*1024*1024||total>8*1024*1024)throw Error('Update too large');parts.push(bytes);};
   if(response.body?.getReader){const reader=response.body.getReader();try{while(true){if(controller.signal.aborted)throw Error('Update cancelled');const {done,value}=await reader.read();if(done)break;add(value);}}catch(error){await reader.cancel().catch(()=>{});throw error;}finally{reader.releaseLock();}}else add(new Uint8Array(await response.arrayBuffer()));
   const bytes=C.concat(...parts),headers=new Response(null,{headers:response.headers}).headers;headers.delete('content-encoding');headers.delete('content-length');return {bytes,response:new Response(bytes,{headers})};};
  try{
   const index=await get(base),html=new TextDecoder().decode(index.bytes),files=manifest(html),responses=new Map([['index.html',index]]),queue=files.filter(f=>f!=='index.html');
   // Limit both outstanding fetches and decoded bodies, including chunked responses.
   const consume=async()=>{while(queue.length){if(controller.signal.aborted)throw Error('Update cancelled');const name=queue.shift();responses.set(name,await get(base+name));}};
   await Promise.all([consume(),consume()]);
   const parts=[];for(const name of files){const bytes=responses.get(name).bytes;parts.push(C.enc.encode(`${name}\n${bytes.length}\n`),bytes);}
   if(await digest(C.concat(...parts),controller.signal)!==hash)throw Error('Update hash mismatch');
   const cache=await caches.open(CACHE);
   for(const [name,{response}] of responses){if(controller.signal.aborted)throw Error('Update cancelled');await cache.put(new URL(base+name,location.origin).href,response.clone());}
   await cache.put(new URL(base,location.origin).href,index.response.clone());
  }catch(error){
   controller.abort();
   // The deployed worker is cache-first. Evict this failed candidate so a repaired deploy can recover.
   try{const cache=await caches.open(CACHE);for(const request of await cache.keys()){const url=new URL(request.url);if(url.origin===location.origin&&url.pathname.startsWith(base))await cache.delete(request);}}catch{}
   throw error;
  }finally{signal?.removeEventListener('abort',abort);}
 }
 const scrollSelectors=['.home-list','.workspace-list','.detail-list','.account-list','.terminal-scroll','.chat-feed','.tab-scroll','.diff-patch','.git-patch','.settings-list','.connection-log'];
 function captureView(root){return scrollSelectors.flatMap(selector=>[...root.querySelectorAll(selector)].map((node,index)=>({selector,index,top:node.scrollTop,left:node.scrollLeft})));}
 function restoreView(root,view){
  if(!Array.isArray(view))return;
  let pending=view.filter(row=>scrollSelectors.includes(row?.selector)&&Number.isSafeInteger(row.index)&&row.index>=0&&Number.isFinite(row.top)&&Number.isFinite(row.left)&&row.top>=0&&row.left>=0),observer,timer;
  const stop=()=>{observer?.disconnect();clearTimeout(timer);for(const name of ['pointerdown','keydown','input'])root.removeEventListener(name,stop,true);};
  const apply=(force=false)=>{pending=pending.filter(row=>{const node=root.querySelectorAll(row.selector)[row.index];if(!node||!force&&(row.top>node.scrollHeight-node.clientHeight||row.left>node.scrollWidth-node.clientWidth))return true;node.scrollTop=row.top;node.scrollLeft=row.left;return false;});if(!pending.length)stop();};
  observer=new MutationObserver(()=>apply());observer.observe(root.documentElement||root,{childList:true,subtree:true,attributes:true,attributeFilter:['style','class','open']});
  for(const name of ['pointerdown','keydown','input'])root.addEventListener(name,stop,true);timer=setTimeout(()=>{apply(true);stop();},5000);apply();
 }
 function create(options){
  const {endpoint}=options,current=/^\/m\/([0-9a-f]{16})\/$/.exec(location.pathname)?.[1];
  let timer,applyTimer,work,controller,stopped=false,navigating=false,pending=null,holds=0,composing=false,lastInput=Date.now();
  const identity=()=>endpoint.updateIdentity?.(),enabled=()=>!!(current&&identity()&&!stopped&&!navigating&&!options.blocked?.());
  const idle=()=>enabled()&&!!navigator.serviceWorker?.controller&&!document.hidden&&!composing&&!holds&&Date.now()-lastInput>=1500&&!options.busy?.();
  const schedule=()=>{clearTimeout(timer);if(!stopped)timer=setTimeout(()=>{check();schedule();},30000);};
  const retry=()=>{clearTimeout(applyTimer);if(pending&&!stopped)applyTimer=setTimeout(apply,1000);};
  async function apply(){
   if(!pending)return;if(!idle()){retry();return;}
   const candidate=pending,key=identity();
   if(candidate.identity!==key){pending=null;return;}
   try{
    const found=await endpoint.probeUpdate({signal:controller.signal});
    if(pending!==candidate||!idle()||identity()!==key){retry();return;}
    if(!found||found.hash!==candidate.hash){pending=null;check();return;}
    const record={schema:1,from:current,to:candidate.hash,identity:key,at:Date.now(),state:options.capture()};
    const encoded=JSON.stringify(record);sessionStorage.setItem(KEY,encoded);
    if(sessionStorage.getItem(KEY)!==encoded)throw Error('Update state not saved');
    if(!endpoint.commitUpdate(found)){sessionStorage.removeItem(KEY);retry();return;}
    navigating=true;clearTimeout(timer);clearTimeout(applyTimer);
    location.replace(`/m/${candidate.hash}/`+location.search+location.hash);
   }catch{retry();}
  }
  async function run(){
   const key=identity();controller?.abort();controller=new AbortController();const signal=controller.signal,timeout=setTimeout(()=>controller.abort(),15000);
   try{
    const found=await endpoint.probeUpdate({signal});
    if(!enabled()||signal.aborted||identity()!==key)return;
    if(!found||found.hash===current){pending=null;if(found&&endpoint.updatePending){endpoint.clearUpdatePending();options.resume?.();}return;}
    if(pending?.hash===found.hash&&pending.identity===key){retry();return;}
    pending=null;await prepare(found.hash,signal);
    if(!enabled()||signal.aborted||identity()!==key)return;
    pending=found;retry();
   }catch{}finally{clearTimeout(timeout);}
  }
  function check(){if(!enabled()||document.hidden)return Promise.resolve();if(work)return work;work=run().finally(()=>work=null);return work;}
  function invalidate(){controller?.abort();pending=null;clearTimeout(applyTimer);}
  function restore(){
   try{const raw=sessionStorage.getItem(KEY);if(!raw)return;const record=JSON.parse(raw);
    if(record.schema!==1||record.to!==current||!validHash(record.from)||typeof record.at!=='number'||!Number.isFinite(record.at)||record.identity!==identity()||Date.now()-record.at>300000||record.at>Date.now()||!record.state)return;
    options.restore(record.state);
   }catch{}finally{try{sessionStorage.removeItem(KEY);}catch{}}
  }
  const activity=()=>{lastInput=Date.now();},resume=()=>{if(!document.hidden){check();retry();}},startComposition=()=>{composing=true;activity();},endComposition=()=>{composing=false;activity();};
  const events=[['input',activity],['pointerdown',activity],['keydown',activity],['compositionstart',startComposition],['compositionend',endComposition],['visibilitychange',resume]];
  function start(){endpoint.setUpdateHandler?.(check);for(const [name,fn] of events)document.addEventListener(name,fn,true);globalThis.addEventListener('pageshow',resume);globalThis.addEventListener('online',resume);navigator.serviceWorker?.addEventListener?.('controllerchange',resume);schedule();check();}
  function stop(){stopped=true;invalidate();clearTimeout(timer);endpoint.setUpdateHandler?.(null);for(const [name,fn] of events)document.removeEventListener(name,fn,true);globalThis.removeEventListener('pageshow',resume);globalThis.removeEventListener('online',resume);navigator.serviceWorker?.removeEventListener?.('controllerchange',resume);}
  return {start,stop,check,invalidate,restore,hold(){holds++;let released=false;return ()=>{if(!released){released=true;holds--;activity();}};}};
 }
 function pickFile(picker){const release=M.update?.hold?.()||(()=>{}),done=()=>{picker.removeEventListener('change',done);picker.removeEventListener('cancel',done);release();};picker.addEventListener('change',done);picker.addEventListener('cancel',done);try{picker.click();}catch(error){done();throw error;}}
 M.updates={create,prepare,manifest,pickFile,captureView,restoreView};
})();

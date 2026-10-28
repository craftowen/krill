import {HttpError} from './validation.js';
export const REPOSITORY = 'craftowen/krill';
const MAX_PAGES=5;
const PAGE_SIZE=20;
const MAX_RESPONSE_BYTES=2*1024*1024;
// Only binary formats actually produced by packaging/, not arbitrary .zip/source files.
export function binaryAsset(name) {
  return typeof name==='string' && name.length<=256 && (
    /^Krill-[0-9][0-9A-Za-z.+-]*\.dmg$/.test(name)
    || /^Krill-[0-9][0-9A-Za-z.+-]*-(?:aarch64|x86_64|arm64|x64)-adhoc\.zip$/.test(name)
    || /^Krill-[0-9][0-9A-Za-z.+-]*-windows-x64\.zip$/.test(name)
    || /^krill-[0-9][0-9A-Za-z.+-]*-linux-(?:x86_64|aarch64)\.tar\.gz$/.test(name)
  ) && !/source|checksum|manifest/i.test(name);
}
async function limitedJson(response) {
  if (!response.body) throw new Error('missing upstream body');
  const reader=response.body.getReader();const chunks=[];let bytes=0;
  try {
    while(true){const {done,value}=await reader.read();if(done)break;bytes+=value.length;if(bytes>MAX_RESPONSE_BYTES){await reader.cancel();throw new Error('upstream body limit');}chunks.push(value);}
  } finally {reader.releaseLock();}
  const body=new Uint8Array(bytes);let offset=0;for(const chunk of chunks){body.set(chunk,offset);offset+=chunk.length;}
  return JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(body));
}
export async function downloads(env, requestFn=fetch) {
  const headers={'accept':'application/vnd.github+json','user-agent':'Krill-telemetry','x-github-api-version':'2026-03-10'};
  if (env.GITHUB_TOKEN) headers.authorization='Bearer '+env.GITHUB_TOKEN;
  const releases=[];let pages=0;let truncated=false;let total=0;
  const signal=AbortSignal.timeout(10000);
  try {
    for(let page=1;page<=MAX_PAGES;page++){
      const response=await requestFn(`https://api.github.com/repos/${REPOSITORY}/releases?per_page=${PAGE_SIZE}&page=${page}`,{headers,signal,redirect:'manual'});
      if(!response.ok){await response.body?.cancel();throw new Error('GitHub unavailable');}
      const batch=await limitedJson(response);
      if(!Array.isArray(batch)||batch.length>PAGE_SIZE)throw new Error('invalid upstream releases');
      pages++;
      for(const release of batch){
        if(release.draft)continue;
        if(typeof release.tag_name!=='string'||release.tag_name.length>128||!Array.isArray(release.assets))throw new Error('invalid upstream release');
        if(release.assets.length>1000)truncated=true;
        let count=0;let assets=0;
        for(const asset of release.assets.slice(0,1000))if(binaryAsset(asset.name)){
          if(!Number.isSafeInteger(asset.download_count)||asset.download_count<0)throw new Error('invalid upstream count');
          count+=asset.download_count;assets++;
        }
        if(!Number.isSafeInteger(total+count))throw new Error('upstream count limit');
        if(assets){releases.push({tag:release.tag_name,binary_downloads:count,binary_assets:assets});total+=count;}
      }
      const more=/<[^>]+>;\s*rel="next"/.test(response.headers.get('link')||'');
      if(!more)break;
      if(page===MAX_PAGES)truncated=true;
    }
  } catch {throw new HttpError(503,'GitHub download counts unavailable');}
  return {repository:REPOSITORY,binary_downloads:total,releases,pages_fetched:pages,truncated,
    meaning:'Cumulative binary asset download requests for the fetched published releases; not unique people, installations or active devices.',
    fetched_at_unix:Math.floor(Date.now()/1000)};
}

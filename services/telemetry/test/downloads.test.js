import {test} from 'node:test';
import assert from 'node:assert/strict';
import {binaryAsset,downloads} from '../src/downloads.js';
test('download assets match actual packaged binaries, exclude source/scripts/manifests/checksums',()=>{
  for(const name of ['Krill-0.2.7.dmg','Krill-0.2.7-aarch64-adhoc.zip','Krill-0.2.7-windows-x64.zip','krill-0.2.7-linux-x86_64.tar.gz'])assert.equal(binaryAsset(name),true,name);
  for(const name of ['Krill-0.2.7-source.tar.gz','source.zip','install.sh','latest.json','Krill-0.2.7.dmg.sha256','Krill-source-0.2.7.dmg','Krill-0.2.7-windows-x64.zip.sha256'])assert.equal(binaryAsset(name),false,name);
});
const release = assets=>({tag_name:'v0.2.7',draft:false,assets});
const asset=(name,count)=>({name,download_count:count});
test('GitHub pagination bounded to five fixed-origin pages; whitelisted counts; secret never reflected',async()=>{
  const calls=[];const token='server-only';const d=await downloads({GITHUB_TOKEN:token},async(url,init)=>{calls.push(url);assert.equal(init.redirect,'manual');assert.equal(init.headers.authorization,'Bearer '+token);return Response.json([release([asset('Krill-0.2.7.dmg',3),asset('install.sh',999),asset('Krill-0.2.7-source.tar.gz',888)])],{headers:{link:'<https://evil.example/steal>; rel="next"'}});});
  assert.equal(calls.length,5);assert.ok(calls.every(x=>x.startsWith('https://api.github.com/repos/craftowen/krill/releases?')));assert.equal(d.binary_downloads,15);assert.equal(d.truncated,true);assert.doesNotMatch(JSON.stringify(d),/server-only|evil\.example/);
});
test('complete coverage, drafts excluded, unconfigured token uses public GitHub API',async()=>{
  const d=await downloads({},async(url,init)=>{assert.equal(init.headers.authorization,undefined);return Response.json([{...release([asset('Krill-0.2.7.dmg',900)]),draft:true},release([asset('Krill-0.2.7.dmg',2)])]);});assert.equal(d.binary_downloads,2);assert.equal(d.truncated,false);
});
test('upstream failures, invalid counts and body limits return generic unavailable without credentials',async()=>{
  for(const response of [new Response('secret upstream detail',{status:403}),Response.json([release([asset('Krill-0.2.7.dmg',-1)])]),new Response('x'.repeat(2*1024*1024+1)),Response.json({bad:true})]){
    await assert.rejects(downloads({GITHUB_TOKEN:'secret'},async()=>response),error=>error.status===503&&error.message==='GitHub download counts unavailable');
  }
});

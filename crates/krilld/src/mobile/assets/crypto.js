/* M9 E2EE v2. Offline implementation: SHA-256, HKDF, ChaCha20, Poly1305. */
'use strict';
const MkCrypto=(()=>{
 const enc=new TextEncoder();
 const concat=(...parts)=>{const b=new Uint8Array(parts.reduce((n,p)=>n+p.length,0));let at=0;for(const p of parts){b.set(p,at);at+=p.length;}return b;};
 const rotr=(v,n)=>(v>>>n)|(v<<(32-n));
 const K=Uint32Array.from([0x428a2f98,0x71374491,0xb5c0fbcf,0xe9b5dba5,0x3956c25b,0x59f111f1,0x923f82a4,0xab1c5ed5,0xd807aa98,0x12835b01,0x243185be,0x550c7dc3,0x72be5d74,0x80deb1fe,0x9bdc06a7,0xc19bf174,0xe49b69c1,0xefbe4786,0x0fc19dc6,0x240ca1cc,0x2de92c6f,0x4a7484aa,0x5cb0a9dc,0x76f988da,0x983e5152,0xa831c66d,0xb00327c8,0xbf597fc7,0xc6e00bf3,0xd5a79147,0x06ca6351,0x14292967,0x27b70a85,0x2e1b2138,0x4d2c6dfc,0x53380d13,0x650a7354,0x766a0abb,0x81c2c92e,0x92722c85,0xa2bfe8a1,0xa81a664b,0xc24b8b70,0xc76c51a3,0xd192e819,0xd6990624,0xf40e3585,0x106aa070,0x19a4c116,0x1e376c08,0x2748774c,0x34b0bcb5,0x391c0cb3,0x4ed8aa4a,0x5b9cca4f,0x682e6ff3,0x748f82ee,0x78a5636f,0x84c87814,0x8cc70208,0x90befffa,0xa4506ceb,0xbef9a3f7,0xc67178f2]);
 function sha256(input){
  const n=Math.ceil((input.length+9)/64)*64,b=new Uint8Array(n);b.set(input);b[input.length]=128;
  let length=BigInt(input.length)*8n;for(let i=0;i<8;i++){b[n-1-i]=Number(length&255n);length>>=8n;}
  const h=Uint32Array.from([0x6a09e667,0xbb67ae85,0x3c6ef372,0xa54ff53a,0x510e527f,0x9b05688c,0x1f83d9ab,0x5be0cd19]),w=new Uint32Array(64);
  for(let at=0;at<n;at+=64){
   for(let i=0;i<16;i++){let p=at+i*4;w[i]=((b[p]<<24)|(b[p+1]<<16)|(b[p+2]<<8)|b[p+3])>>>0;}
   for(let i=16;i<64;i++){const a=w[i-15],c=w[i-2],s0=rotr(a,7)^rotr(a,18)^(a>>>3),s1=rotr(c,17)^rotr(c,19)^(c>>>10);w[i]=(w[i-16]+s0+w[i-7]+s1)>>>0;}
   let [a,c,d,e,f,g,j,k]=h;
   for(let i=0;i<64;i++){const s1=rotr(f,6)^rotr(f,11)^rotr(f,25),ch=(f&g)^(~f&j),t1=(k+s1+ch+K[i]+w[i])>>>0,s0=rotr(a,2)^rotr(a,13)^rotr(a,22),maj=(a&c)^(a&d)^(c&d),t2=(s0+maj)>>>0;k=j;j=g;g=f;f=(e+t1)>>>0;e=d;d=c;c=a;a=(t1+t2)>>>0;}
   const next=[a,c,d,e,f,g,j,k];for(let i=0;i<8;i++)h[i]=(h[i]+next[i])>>>0;
  }
  const out=new Uint8Array(32),view=new DataView(out.buffer);for(let i=0;i<8;i++)view.setUint32(i*4,h[i]);return out;
 }
 function hmac(key,data){if(key.length>64)key=sha256(key);const a=new Uint8Array(64).fill(0x36),b=new Uint8Array(64).fill(0x5c);for(let i=0;i<key.length;i++){a[i]^=key[i];b[i]^=key[i];}return sha256(concat(b,sha256(concat(a,data))));}
 const hkdf=(key,salt,info)=>hmac(hmac(salt,key),concat(enc.encode(info),Uint8Array.of(1)));
 const le=(value,n=8)=>{let b=new Uint8Array(n);value=BigInt(value);for(let i=0;i<n;i++){b[i]=Number(value&255n);value>>=8n;}return b;};
 const bigint=b=>{let v=0n;for(let i=b.length-1;i>=0;i--)v=(v<<8n)|BigInt(b[i]);return v;};
 function chacha(key,nonce,counter){
  const state=new Uint32Array(16),v=new DataView(key.buffer,key.byteOffset,key.byteLength),n=new DataView(nonce.buffer,nonce.byteOffset,nonce.byteLength);
  state.set([0x61707865,0x3320646e,0x79622d32,0x6b206574]);for(let i=0;i<8;i++)state[i+4]=v.getUint32(i*4,true);state[12]=counter;for(let i=0;i<3;i++)state[13+i]=n.getUint32(i*4,true);
  const x=state.slice(),rot=(a,n)=>(a<<n)|(a>>>(32-n));
  const q=(a,b,c,d)=>{x[a]=(x[a]+x[b])>>>0;x[d]=rot(x[d]^x[a],16);x[c]=(x[c]+x[d])>>>0;x[b]=rot(x[b]^x[c],12);x[a]=(x[a]+x[b])>>>0;x[d]=rot(x[d]^x[a],8);x[c]=(x[c]+x[d])>>>0;x[b]=rot(x[b]^x[c],7);};
  for(let i=0;i<10;i++){q(0,4,8,12);q(1,5,9,13);q(2,6,10,14);q(3,7,11,15);q(0,5,10,15);q(1,6,11,12);q(2,7,8,13);q(3,4,9,14);}
  const out=new Uint8Array(64),view=new DataView(out.buffer);for(let i=0;i<16;i++)view.setUint32(i*4,(x[i]+state[i])>>>0,true);return out;
 }
 function xor(key,nonce,data){const out=new Uint8Array(data.length);for(let at=0;at<data.length;at+=64){const stream=chacha(key,nonce,1+at/64);for(let i=0;i<Math.min(64,data.length-at);i++)out[at+i]=data[at+i]^stream[i];}return out;}
 function poly1305(key,data){const r=bigint(key.subarray(0,16))&0x0ffffffc0ffffffc0ffffffc0fffffffn,s=bigint(key.subarray(16,32)),p=(1n<<130n)-5n;let a=0n;for(let at=0;at<data.length;at+=16){let part=data.subarray(at,at+16);a=((a+bigint(part)+(1n<<BigInt(part.length*8)))*r)%p;}return le((a+s)&((1n<<128n)-1n),16);}
 const pad=b=>new Uint8Array((16-b.length%16)%16);
 const tag=(key,nonce,aad,data)=>poly1305(chacha(key,nonce,0).subarray(0,32),concat(aad,pad(aad),data,pad(data),le(aad.length),le(data.length)));
 const hex=b=>Array.from(b,x=>x.toString(16).padStart(2,'0')).join('');
 const unhex=s=>{if(!/^[0-9a-f]{64}$/i.test(s))throw Error('Invalid key');return Uint8Array.from(s.match(/../g),v=>parseInt(v,16));};
 const unbase64=s=>Uint8Array.from(atob(s.replace(/-/g,'+').replace(/_/g,'/')),c=>c.charCodeAt(0));
 class Channel {
  constructor(secret,salt,client=true){this.tx=hkdf(secret,salt,client?'mk-mobile-v2/client-to-daemon':'mk-mobile-v2/daemon-to-client');this.rx=hkdf(secret,salt,client?'mk-mobile-v2/daemon-to-client':'mk-mobile-v2/client-to-daemon');this.txPrefix=enc.encode(client?'MKC2':'MKD2');this.rxPrefix=enc.encode(client?'MKD2':'MKC2');this.sequence=0n;this.received=-1n;}
  seal(plain){if(this.sequence>0xffffffffffffffffn||plain.length>1048580)throw Error('Frame limit');const seq=le(this.sequence++),nonce=concat(this.txPrefix,seq),body=xor(this.tx,nonce,plain);return concat(seq,body,tag(this.tx,nonce,seq,body));}
  open(frame){frame=new Uint8Array(frame);if(frame.length<24||frame.length>1048604)throw Error('Frame size');const seq=frame.subarray(0,8),number=bigint(seq);if(number<=this.received)throw Error('Replay rejected');const nonce=concat(this.rxPrefix,seq),body=frame.subarray(8,-16),expected=tag(this.rx,nonce,seq,body);let different=0;for(let i=0;i<16;i++)different|=expected[i]^frame[frame.length-16+i];if(different)throw Error('Authentication failed');this.received=number;return xor(this.rx,nonce,body);}
 }
 return {sha256,hmac,hkdf,Channel,concat,hex,unhex,unbase64,enc};
})();
globalThis.MkCrypto=MkCrypto;

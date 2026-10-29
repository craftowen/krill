"""Two isolated desktops behind a local relay substitute; real E2EE and Chromium UI.

Run after building krill/krilld/krill-holder, with KRILL_BIN and KRILLD_BIN set.
The proxy only substitutes the relay routing; desktop authentication/RPC stay real.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

from mobile_browser import until


def main():
    target = Path(os.environ.get('CARGO_TARGET_DIR', Path(__file__).resolve().parents[3] / 'target'))
    binaries = Path(os.environ.get('KRILL_MOBILE_TEST_BIN', str(target / 'debug')))
    krilld = Path(os.environ.get('KRILLD_BIN', str(binaries / 'krilld')))
    krill = Path(os.environ.get('KRILL_BIN', str(binaries / 'krill')))
    tests = Path(__file__).resolve().parent
    assets = tests.parent / 'src/mobile/assets'
    session = f'krill-hosts-{os.getpid()}'
    shots = Path(os.environ.get('KRILL_MOBILE_SHOTS', '/tmp/krill-mobile-hosts'))
    shots.mkdir(parents=True, exist_ok=True)
    fixtures = []
    proxy = None
    logs = []
    with tempfile.TemporaryDirectory(prefix='krill-hosts-') as scratch:
        scratch = Path(scratch)
        try:
            configs = []
            for index in range(2):
                metadata = scratch / f'desktop-{index}.json'
                log = (scratch / f'desktop-{index}.log').open('w+')
                logs.append(log)
                fixture = subprocess.Popen(
                    ['node', str(tests / 'mobile_pwa.cjs'), str(krilld), '--keep'],
                    env=dict(os.environ, M9_BROWSER_METADATA=str(metadata), M9_BROWSER_ADDRESS='127.0.0.1:0'),
                    stdout=log, stderr=log,
                )
                fixtures.append(fixture)
                until(lambda: metadata.exists() or fixture.poll() is not None, 90)
                if not metadata.exists():
                    log.seek(0)
                    raise AssertionError(log.read())
                configs.append(json.loads(metadata.read_text()))

            # Like the hosted app, both desktops use one origin and distinct relay IDs.
            # Rewrite Origin/Host only inside this test proxy for direct-mode fixtures.
            router = scratch / 'relay.cjs'
            router.write_text(r"""
const http=require('node:http'),net=require('node:net'),fs=require('node:fs'),path=require('node:path'),{execFileSync}=require('node:child_process');
const [root,configFile,ready]=process.argv.slice(2),configs=JSON.parse(fs.readFileSync(configFile)),promotion=!!process.env.M9_UI_PROMOTION,oldHash='10cc9c1f17f4b33a',hash=promotion?execFileSync('node',['cloud/mobile/build.mjs','--repo','.','--print-hash'],{encoding:'utf8'}).trim():'0123456789abcdef';
const routes=new Map(configs.map((c,i)=>[(i?'b':'a').repeat(32),new URL(c.base)]));
const server=http.createServer((req,res)=>{
 const u=new URL(req.url,'http://localhost');
 if(u.pathname.startsWith('/r/')){res.setHeader('Content-Type','application/json');res.end(JSON.stringify({relay:true,hash}));return;}
 let file=u.pathname.replace(/^\/m\/[0-9a-f]{16}\//,'').replace(/^\//,'')||'index.html';
 const resolved=path.resolve(root,file);if(!resolved.startsWith(root+path.sep)){res.writeHead(403);res.end();return;}
 try{const data=promotion&&u.pathname==='/sw.js'?fs.readFileSync('cloud/mobile/public/sw.js'):promotion&&u.pathname.startsWith('/m/'+oldHash+'/')?execFileSync('git',['show','v0.1.25:crates/krilld/src/mobile/assets/'+file]):fs.readFileSync(resolved);res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':file.endsWith('.html')?'text/html':file.endsWith('.png')?'image/png':file.endsWith('.svg')?'image/svg+xml':'application/json');res.end(data);}catch{res.writeHead(404);res.end();}
});
server.on('upgrade',(req,socket,head)=>{
 const route=routes.get(new URL(req.url,'http://localhost').pathname.split('/')[2]);if(!route){socket.destroy();return;}
 const upstream=net.connect(Number(route.port),route.hostname,()=>{
  const headers={...req.headers,host:route.host,origin:route.origin};
  upstream.write('GET /ws HTTP/1.1\r\n'+Object.entries(headers).map(([k,v])=>k+': '+v).join('\r\n')+'\r\n\r\n');
  if(head.length)upstream.write(head);socket.pipe(upstream);upstream.pipe(socket);
 });upstream.on('error',()=>socket.destroy());socket.on('error',()=>upstream.destroy());socket.on('close',()=>upstream.destroy());
});
server.listen(0,'127.0.0.1',()=>fs.writeFileSync(ready,String(server.address().port)));
""")
            config_file = scratch / 'configs.json'
            config_file.write_text(json.dumps(configs))
            ready = scratch / 'proxy-port'
            proxy = subprocess.Popen(['node', str(router), str(assets), str(config_file), str(ready)])
            until(ready.exists)
            initial_hash = '10cc9c1f17f4b33a' if os.environ.get('M9_UI_PROMOTION') else '0123456789abcdef'
            base = f'http://127.0.0.1:{ready.read_text()}/m/{initial_hash}/'

            def browser(*args):
                result = subprocess.run(['agent-browser', '--session', session, *args], text=True, capture_output=True, timeout=45)
                if result.returncode != 0:
                    detail = subprocess.run(['agent-browser', '--session', session, 'snapshot', '-i'], text=True, capture_output=True, timeout=15)
                    debug = subprocess.run(['agent-browser', '--session', session, 'eval', '--json', "(async()=>({path:location.pathname,ready:KrillMobile.store?.connection.phase,controller:!!navigator.serviceWorker?.controller,hash:await fetch('/r/'+'a'.repeat(32)).then(r=>r.json()).then(r=>r.hash),prepare:await KrillMobile.updates.prepare(await fetch('/r/'+'a'.repeat(32)).then(r=>r.json()).then(r=>r.hash),new AbortController().signal).then(()=>true).catch(e=>String(e)),assets:performance.getEntriesByType('resource').map(r=>new URL(r.name).pathname).slice(-32)}))()"], text=True, capture_output=True, timeout=15)
                    raise AssertionError(result.stdout + result.stderr + detail.stdout + debug.stdout + debug.stderr)
                return result.stdout

            def evaluate(script):
                return json.loads(browser('eval', '--json', script))['data']['result']

            def wait(script):
                browser('wait', '--fn', script)

            def click(name):
                browser('find', 'role', 'button', 'click', '--name', name, '--exact')

            def home():
                evaluate("KrillMobile.app.navigate('/');true")
                wait('!!document.querySelector(".home-list,.home-empty")')

            def pair(index):
                if index:
                    click('Pair Desktop')
                    click('Paste link instead')
                else:
                    click('Paste pairing link')
                wait("document.querySelector('input[type=url]')!==null && document.querySelector('#action-sheet').open")
                # Fresh ticket, since a previous scan may already have consumed one.
                offer = json.loads(subprocess.check_output(
                    [str(krill), 'mobile', 'pair', '--address', configs[index]['base'].split('://')[1], '--json'],
                    env=dict(os.environ, KRILL_STATE_DIR=configs[index]['dir']), text=True,
                ))['PairingOffer']['url']
                fragment = offer.split('#', 1)[1]
                browser('fill', 'input[type=url]', base + '#' + fragment + '&r=' + ('b' if index else 'a') * 32)
                click('Continue')
                if os.environ.get('M9_UI_PROMOTION') and index == 0:
                    wait('KrillMobile.store.connection.phase==="ready"')
                    browser('reload')
                    wait('KrillMobile.store.connection.phase==="ready"')
                    evaluate("dispatchEvent(new Event('pageshow'));true")
                wait('typeof KrillMobile.app.getConnections==="function" && KrillMobile.app.getConnections().length===' + str(index+1) + ' && KrillMobile.app.getConnections().every(h=>h.connection.phase==="ready")')
                home()

            browser('open', base)
            evaluate("localStorage.setItem('krill-mobile-ui-v1',JSON.stringify({language:'en'}));true")
            browser('reload')
            wait('!!document.querySelector(".home-list,.home-empty")')
            browser('set', 'viewport', '390', '844')
            assert evaluate('document.querySelector(".home-hero")!==null')
            evaluate("KrillMobile.store.preferences.installOffered=true;true")
            pair(0)
            if os.environ.get('M9_UI_PROMOTION'):
                assert evaluate('location.pathname') != '/m/10cc9c1f17f4b33a/', 'old PWA must advance automatically without another QR'
                base = evaluate('location.origin+location.pathname')
            assert evaluate('JSON.parse(localStorage.getItem("krill-mobile-hosts-v1")).hosts.length') == 1
            evaluate("KrillMobile.store.drafts.set('1','first desktop draft');true")
            pair(1)
            assert evaluate('JSON.parse(localStorage.getItem("krill-mobile-hosts-v1")).hosts.length') == 2
            assert evaluate('KrillMobile.store.drafts.size') == 0
            assert evaluate('document.querySelectorAll(".host-card").length') == 2
            wait('KrillMobile.app.getConnections().length===2 && KrillMobile.app.getConnections().every(h=>h.connection.phase==="ready")')
            assert evaluate('KrillMobile.app.getConnections().every(h=>h.snapshot.sessions.length>0)')
            hosts = evaluate('KrillMobile.app.getConnections().map(h=>({id:h.id,session:h.snapshot.sessions[0].id,cwd:h.snapshot.sessions[0].cwd}))')
            assert hosts[0]['session'] == hosts[1]['session'], 'fixture must exercise equal session IDs on different PCs'
            assert hosts[0]['cwd'] != hosts[1]['cwd']
            evaluate("document.querySelector('.host-main').click();true")
            wait('KrillMobile.store.connection.phase==="ready" && !!document.querySelector(".workspace-list")')
            home()
            assert evaluate('KrillMobile.app.getConnections().every(h=>h.connection.phase==="ready")')
            assert evaluate('JSON.parse(localStorage.getItem("krill-mobile-hosts-v1")).active') == 'relay:' + 'a' * 32
            assert evaluate("KrillMobile.store.drafts.get('1')") == 'first desktop draft'
            # The second card keeps its original authenticated connection while switching views.
            evaluate("document.querySelectorAll('.host-main')[1].click();true")
            wait('!!document.querySelector(".workspace-list")')
            assert evaluate('KrillMobile.app.getConnections().every(h=>h.connection.phase==="ready")')
            assert evaluate('KrillMobile.store.drafts.size') == 0
            home()
            browser('reload')
            wait('KrillMobile.app.getConnections().length===2 && KrillMobile.app.getConnections().every(h=>h.connection.phase==="ready")')
            home()
            assert evaluate('document.querySelectorAll(".recent-session-row[data-session]").length') == 2
            evaluate("[...document.querySelectorAll('.recent-session-row[data-session]')].find(n=>n.dataset.host==="+json.dumps(hosts[0]['id'])+").click();true")
            wait('!!document.querySelector(".session-tabs")')
            assert evaluate('JSON.parse(localStorage.getItem("krill-mobile-hosts-v1")).active') == hosts[0]['id']
            assert evaluate('KrillMobile.store.sessions.get('+str(hosts[0]['session'])+').cwd') == hosts[0]['cwd']
            home()
            evaluate("[...document.querySelectorAll('.recent-session-row[data-session]')].find(n=>n.dataset.host==="+json.dumps(hosts[1]['id'])+").click();true")
            wait('!!document.querySelector(".session-tabs")')
            assert evaluate('JSON.parse(localStorage.getItem("krill-mobile-hosts-v1")).active') == hosts[1]['id']
            assert evaluate('KrillMobile.store.sessions.get('+str(hosts[1]['session'])+').cwd') == hosts[1]['cwd']
            home()
            assert evaluate('document.querySelector(".krill-logo").complete && document.querySelector(".krill-logo").naturalWidth>0')
            browser('screenshot', str(shots / 'home.png'))
            click('Accounts')
            wait('!!document.querySelector(".account-row")')
            assert evaluate('document.querySelectorAll(".usage-bar").length>=2')
            assert evaluate('[...document.querySelectorAll(".usage-column")].every(n=>n.getBoundingClientRect().height<32)')
            assert evaluate('[...document.querySelectorAll(".usage-label")].every(n=>["5h","7d"].includes(n.textContent))')
            assert evaluate('!document.querySelector(".account-list").innerText.includes("until reset")')
            browser('screenshot', str(shots / 'accounts.png'))
            click('Choose desktop')
            wait('document.querySelector("#action-sheet").open')
            evaluate("document.querySelector('.sheet-action').click();true")
            wait('!!document.querySelector(".account-row")')
            assert evaluate('JSON.parse(localStorage.getItem("krill-mobile-hosts-v1")).active') == hosts[0]['id']
            home()
            # One unavailable PC must not disconnect the other or mislabel cached rows as live.
            evaluate("globalThis.offlineHost="+json.dumps(hosts[0]['id'])+";true")
            subprocess.check_output([str(krill), 'mobile', 'revoke', '--all'], env=dict(os.environ, KRILL_STATE_DIR=configs[0]['dir']), text=True)
            wait('KrillMobile.app.getConnections().find(h=>h.id===offlineHost).connection.phase!=="ready"')
            assert evaluate('KrillMobile.app.getConnections().find(h=>h.id!==offlineHost).connection.phase==="ready"')
            assert evaluate('[...document.querySelectorAll(".recent-session-row[data-session]")].some(b=>b.disabled)')
            evaluate("KrillMobile.store.preferences.language='ko';KrillMobile.i18n.configure('ko');KrillMobile.app.navigate('/accounts');true")
            wait('!!document.querySelector(".accounts")')
            # Choose the remaining connected PC, then inspect Korean UI with English duration units.
            evaluate('KrillMobile.app.selectHost('+json.dumps(hosts[1]['id'])+",()=>KrillMobile.app.navigate('/accounts'));true")
            wait('!!document.querySelector(".account-row")')
            browser('set', 'viewport', '320', '740')
            evaluate("document.documentElement.style.setProperty('--ui-scale','1.5');true")
            assert evaluate('document.documentElement.scrollWidth<=innerWidth')
            assert evaluate('[...document.querySelectorAll(".usage-reset")].every(n=>!/[가-힣]/.test(n.textContent))')
            browser('screenshot', str(shots / 'accounts-ko-320.png'))
            home()
            assert evaluate('document.documentElement.scrollWidth<=innerWidth')
            assert evaluate('!document.body.innerText.includes("다시 오셨군요")')
            browser('screenshot', str(shots / 'home-ko-320.png'))
            errors=browser('errors')
            assert 'ReferenceError' not in errors and 'TypeError' not in errors, errors
            print('PASS two real desktops: simultaneous authenticated connections, equal-ID routing, scoped drafts, reload, recent sessions, account PC picker, partial outage and compact Korean UI; screenshots '+str(shots))
        finally:
            subprocess.run(['agent-browser', '--session', session, 'close'], capture_output=True)
            if proxy:
                proxy.terminate()
                proxy.wait(timeout=10)
            for fixture in fixtures:
                fixture.terminate()
            for fixture in fixtures:
                try:
                    fixture.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    fixture.kill()
                    fixture.wait()
            for log in logs:
                log.close()


if __name__ == '__main__':
    main()

"""Two isolated desktops behind a local relay substitute; real E2EE and Chromium UI.

Run after building krill/krilld/krill-holder, with CARGO_TARGET_DIR on krill-build.
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
    target = Path(os.environ['CARGO_TARGET_DIR'])
    tests = Path(__file__).resolve().parent
    assets = tests.parent / 'src/mobile/assets'
    session = f'krill-hosts-{os.getpid()}'
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
                    ['node', str(tests / 'mobile_pwa.cjs'), str(target / 'debug/krilld'), '--keep'],
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
const http=require('node:http'),net=require('node:net'),fs=require('node:fs'),path=require('node:path');
const [root,configFile,ready]=process.argv.slice(2),configs=JSON.parse(fs.readFileSync(configFile)),hash='0123456789abcdef';
const routes=new Map(configs.map((c,i)=>[(i?'b':'a').repeat(32),new URL(c.base)]));
const server=http.createServer((req,res)=>{
 const u=new URL(req.url,'http://localhost');
 if(u.pathname.startsWith('/r/')){res.setHeader('Content-Type','application/json');res.end(JSON.stringify({relay:true,hash}));return;}
 let file=u.pathname.replace(/^\/m\/[0-9a-f]{16}\//,'').replace(/^\//,'')||'index.html';
 const resolved=path.resolve(root,file);if(!resolved.startsWith(root+path.sep)){res.writeHead(403);res.end();return;}
 try{const data=fs.readFileSync(resolved);res.setHeader('Content-Type',file.endsWith('.js')?'text/javascript':file.endsWith('.css')?'text/css':file.endsWith('.html')?'text/html':'application/json');res.end(data);}catch{res.writeHead(404);res.end();}
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
            base = f'http://127.0.0.1:{ready.read_text()}/m/0123456789abcdef/'

            def browser(*args):
                result = subprocess.run(['agent-browser', '--session', session, *args], text=True, capture_output=True, timeout=45)
                if result.returncode != 0:
                    detail = subprocess.run(['agent-browser', '--session', session, 'snapshot', '-i'], text=True, capture_output=True, timeout=15)
                    raise AssertionError(result.stdout + result.stderr + detail.stdout)
                return result.stdout

            def evaluate(script):
                return json.loads(browser('eval', '--json', script))['data']['result']

            def wait(script):
                browser('wait', '--fn', script)

            def click(name):
                browser('find', 'role', 'button', 'click', '--name', name, '--exact')

            def home():
                evaluate("KrillMobile.app.navigate('/');true")
                wait('!!document.querySelector(".quick-actions")')

            def pair(index):
                click('Pair Desktop')
                click('Paste link instead')
                wait("document.querySelector('input[type=url]')!==null && document.querySelector('#action-sheet').open")
                # Fresh ticket, since a previous scan may already have consumed one.
                offer = json.loads(subprocess.check_output(
                    [str(target / 'debug/krill'), 'mobile', 'pair', '--address', configs[index]['base'].split('://')[1], '--json'],
                    env=dict(os.environ, KRILL_STATE_DIR=configs[index]['dir']), text=True,
                ))['PairingOffer']['url']
                fragment = offer.split('#', 1)[1]
                browser('fill', 'input[type=url]', base + '#' + fragment + '&r=' + ('b' if index else 'a') * 32)
                click('Continue')
                wait('KrillMobile.store.connection.phase==="ready"')
                home()

            browser('open', base)
            evaluate("localStorage.setItem('krill-mobile-ui-v1',JSON.stringify({language:'en'}));true")
            browser('reload')
            wait('!!document.querySelector(".quick-actions")')
            browser('set', 'viewport', '390', '844')
            assert evaluate('document.querySelector(".quick-actions button:last-child").disabled')
            pair(0)
            assert evaluate('JSON.parse(localStorage.getItem("krill-mobile-hosts-v1")).hosts.length') == 1
            evaluate("KrillMobile.store.drafts.set('1','first desktop draft');true")
            pair(1)
            assert evaluate('JSON.parse(localStorage.getItem("krill-mobile-hosts-v1")).hosts.length') == 2
            assert evaluate('KrillMobile.store.drafts.size') == 0
            assert evaluate('document.querySelectorAll(".host-card").length') == 2
            assert evaluate('KrillMobile.store.snapshot.sessions.some(s=>s.cwd.startsWith(' + json.dumps(configs[1]['dir'].replace('/tmp/', '/private/tmp/')) + '))')
            # All host state is cleared synchronously before the next PC responds.
            evaluate("document.querySelector('.saved-host .host-main').click();if(KrillMobile.store.snapshot!==null||KrillMobile.store.sessions.size)throw Error('Previous host state leaked');true")
            wait('KrillMobile.store.connection.phase==="ready"')
            home()
            assert evaluate('JSON.parse(localStorage.getItem("krill-mobile-hosts-v1")).active') == 'relay:' + 'a' * 32
            # Reload returns to the selected PC without QR pairing.
            browser('reload')
            wait('KrillMobile.store.connection.phase==="ready"')
            home()
            # A failed attempt to add another PC must still let the saved PC reconnect.
            click('Pair Desktop')
            click('Paste link instead')
            wait("document.querySelector('input[type=url]')!==null && document.querySelector('#action-sheet').open")
            browser('fill', 'input[type=url]', base + '#pair=ab12&key=' + 'A' * 43 + '&r=' + 'b' * 32)
            click('Continue')
            wait('KrillMobile.app.transport.blocked')
            evaluate("document.querySelector('.host-main').click();true")
            wait('KrillMobile.store.connection.phase==="ready"')
            assert evaluate('JSON.parse(localStorage.getItem("krill-mobile-hosts-v1")).active') == 'relay:' + 'a' * 32
            home()
            click('New Workspace')
            wait('document.querySelector("#sheet-title")?.textContent==="Choose desktop"')
            # Pick the other host. Creation must wait for its authenticated snapshot.
            evaluate("[...document.querySelectorAll('.sheet-action')].find(b=>b.textContent.includes('bbbbbbbb')).click();true")
            wait("document.querySelector('input[aria-label=\"Workspace name\"]')!==null")
            assert evaluate('JSON.parse(localStorage.getItem("krill-mobile-hosts-v1")).active') == 'relay:' + 'b' * 32
            browser('fill', 'input[aria-label="Workspace name"]', 'from-phone-ui')
            click('Create workspace')
            wait('KrillMobile.store.snapshot?.sessions.some(s=>s.cwd?.includes("from-phone-ui"))')
            assert Path(configs[1]['dir'], 'home/Minikrill/worktrees/repo/from-phone-ui/.git').exists()
            assert not Path(configs[0]['dir'], 'home/Minikrill/worktrees/repo/from-phone-ui').exists()
            home()
            screenshot = Path('/tmp/krill-mobile-hosts.png')
            browser('screenshot', str(screenshot))
            # A narrow screen and enlarged type must keep both quick actions reachable.
            browser('set', 'viewport', '320', '740')
            evaluate("KrillMobile.store.preferences.uiTextScale=1.5;document.documentElement.style.setProperty('--ui-scale','1.5');true")
            assert evaluate('document.documentElement.scrollWidth<=innerWidth')
            assert evaluate('[...document.querySelectorAll(".quick-action")].every(b=>b.getBoundingClientRect().width>0)')
            print(f'PASS two real desktops: pairing, preserved credentials, isolated state, switch/reload, host picker and workspace creation; screenshot {screenshot}')
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
